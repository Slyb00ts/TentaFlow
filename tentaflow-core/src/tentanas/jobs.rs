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
use serde_json::Value;
use tentaflow_protocol::tentanas::{NasHealthReason, NasJob, NasSmartSelfTest};
use tentanas_helper::{HelperCommand, PackageManager, SelfTestKind};
use tokio_util::sync::CancellationToken;

use super::db as store;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use tentanas_helper::elastic::{ElasticCreateSpec, ElasticOwner, ElasticSnapraidKind};
use futures::FutureExt;

pub enum ElasticJobIntent {
    Create(ElasticCreateSpec),
    Restore { owner: ElasticOwner, array_id: String, operation_id: String },
    /// `acknowledge_parity_fault` is carried into the helper's Sync command:
    /// the admin's confirm of a Sync over a recorded Scrub or Repair fault,
    /// naming the operation that recorded THAT fault. Only a manual Sync
    /// request ever sets it; the scheduler never does.
    Snapraid { owner: ElasticOwner, array_id: String, operation_id: String, kind: ElasticSnapraidKind,
        acknowledge_parity_fault: Option<String> },
    /// One mover run. `resume_operation_id` is reserved up front: the helper
    /// records it with the run and refuses a command whose resume repeats its
    /// operation, and it is the UUID the Resume of a Hold an older helper's run
    /// left must carry. Movers no longer hold the union themselves.
    /// `rules` and `coupled_sync` travel WITH the intent because they are
    /// derived from the array row (its mover settings and its folders' cache
    /// policies) and cannot be rebuilt from the persisted `ElasticCreateSpec`
    /// alone, which is all the store has. The row therefore records the exact
    /// command the body will run.
    Mover { owner: ElasticOwner, array_id: String, operation_id: String, resume_operation_id: String,
        rules: tentanas_helper::elastic::MoverRules, coupled_sync: bool },
    /// One more data disk on a live array. `disk` travels WITH the intent
    /// because nothing else records it: the array's persisted spec is still
    /// the array WITHOUT the disk until the operation closes, so the request
    /// row is the only place the slot's identity — including the filesystem
    /// UUID its mkfs is given — is written down before anything is formatted.
    AddDisk { owner: ElasticOwner, array_id: String, operation_id: String,
        disk: tentanas_helper::elastic::ElasticDiskSpec },
    /// DORMANT: disk replacement is withdrawn (round 4). `db::insert_job`
    /// refuses this intent outright, so no `replace_disk` operation can be
    /// opened by any caller; the variant and its command builder stay for the
    /// task that finishes the feature.
    ///
    /// The disk of ONE data slot replaced by a new one, and the array rebuilt
    /// onto it. `branch` names the slot (`d1`, `d2`, …) and `disk` the
    /// replacement: as with an add, nothing else records the new identity —
    /// including the filesystem UUID its mkfs will stamp — until the operation
    /// closes, so the request row is where it is written down first.
    /// `rebuild_operation_id` and `sync_operation_id` reserve the two SnapRAID
    /// runs the flow records; `accept_stale_parity` is the admin's explicit
    /// word that a rebuild from parity which is not current leaves some blocks
    /// unrecoverable.
    ReplaceDisk { owner: ElasticOwner, array_id: String, operation_id: String,
        rebuild_operation_id: String, sync_operation_id: String, branch: String,
        disk: tentanas_helper::elastic::ElasticDiskSpec, accept_stale_parity: bool },
    /// Undoes the unfinished add of `disk`, the pinned add's own identity
    /// (the filesystem UUID included: the undo erases only that filesystem).
    AddDiskAbort { owner: ElasticOwner, array_id: String, operation_id: String,
        disk: tentanas_helper::elastic::ElasticDiskSpec },
    /// Stops serving the array and deletes its rows. Formats nothing.
    Dissolve { owner: ElasticOwner, array_id: String, operation_id: String },
}

pub(crate) struct RunningJob {
    cancel: CancellationToken,
    cancellable: bool,
}

pub(crate) fn running() -> &'static Mutex<HashMap<String, RunningJob>> {
    static REG: OnceLock<Mutex<HashMap<String, RunningJob>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Handle a job body uses to report. Log lines are persisted immediately so
/// a dashboard polling `JobGetRequest` sees them as they happen.
#[derive(Clone)]
pub struct JobHandle {
    db: DbPool,
    pub job_id: String,
    cancel: CancellationToken,
    /// Ids this job knows the name of that the node's tables may not hold
    /// any more (a dissolved array's id, released by a wipe): see `name_id`.
    extra_names: Arc<Mutex<Vec<(String, String)>>>,
}

impl JobHandle {
    /// A handle over an in-memory database, for tests that need to drive a job
    /// BODY rather than the job machinery around it.
    ///
    /// It exists because `config_io::apply` — the import — could not be
    /// reached by any test at all, so the collision report it ends with was
    /// written, reviewed and shipped without once being executed. The row it
    /// needs in `jobs` is inserted here so `log` has somewhere to write.
    #[cfg(test)]
    pub fn for_test(db: &DbPool, job_id: &str) -> Self {
        let _ = store::insert_job(
            db,
            &NasJob {
                job_id: job_id.to_string(),
                kind: "config_import".to_string(),
                subject: "test".to_string(),
                status: "running".to_string(),
                progress_pct: None,
                started_by: "test".to_string(),
                started_at: store::now(),
                finished_at: None,
                error: None,
                log: Vec::new(),
                subject_last_known: false,
                disks: Vec::new(),
            },
            None,
        );
        Self {
            db: db.clone(),
            job_id: job_id.to_string(),
            cancel: CancellationToken::new(),
            extra_names: Arc::default(),
        }
    }

    /// Every line is written with its ids named or hidden
    /// (`log_ids::LogNames`, owner decision 2026-09-26): the log is an
    /// audit trail an admin reads, and an id is the one thing in it nobody
    /// can recognise.
    pub fn log(&self, line: impl AsRef<str>) {
        let text = line.as_ref();
        if text.trim().is_empty() {
            return;
        }
        let names = self.names();
        for l in text.lines() {
            let l = l.trim_end();
            if l.is_empty() {
                continue;
            }
            let l = names.scrub(l);
            if let Err(e) = store::append_job_log(&self.db, &self.job_id, &l) {
                tracing::warn!("tentanas job {}: log write failed: {e}", self.job_id);
            }
        }
    }

    /// `id` reads as `name` in every later line of this job: for an id whose
    /// row is gone before the line is written (the array a wipe released).
    pub fn name_id(&self, id: &str, name: &str) {
        self.extra_names
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((id.to_string(), name.to_string()));
    }

    fn names(&self) -> super::log_ids::LogNames {
        let mut names = super::log_ids::LogNames::load(&self.db);
        for (id, name) in self.extra_names.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            names.insert(id, name);
        }
        names
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

    /// The token `spawn` races the body against. A job that must undo
    /// something on cancellation (a scrub has to be told to stop) holds it in
    /// a guard, because the body future is dropped, not resumed.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

/// Runs one catalog command inside a job and logs the exact argv it executed.
/// The argv is what the helper resolved, so the log shows the real command —
/// and never a secret: passwords travel through the elevation token and
/// encryption keys through stdin, neither of which is an argv word.
pub async fn run_step(
    h: &JobHandle,
    command: &HelperCommand,
    explicit: Option<&ElevationToken>,
    timeout: Duration,
) -> Result<super::broker::CommandOutput> {
    if let Ok(plan) = command.plan() {
        h.log(format!("$ {}", plan.display()));
    }
    let (out, channel) = super::broker::run_privileged(h.db(), command, explicit, timeout).await?;
    h.log(format!("channel: {}", channel.as_str()));
    h.log(&out.stdout);
    h.log(&out.stderr);
    if !out.success() {
        return Err(anyhow!(
            "{} exited with {}: {}",
            command_label(command),
            out.code,
            out.stderr.trim().lines().next().unwrap_or("no output")
        ));
    }
    Ok(out)
}

/// The same, with raw key material on stdin — `zfs create -o encryption=…`,
/// `zpool create -O encryption=…` and `zfs load-key`.
pub async fn run_step_with_key(
    h: &JobHandle,
    command: &HelperCommand,
    key: &[u8],
    explicit: Option<&ElevationToken>,
    timeout: Duration,
) -> Result<super::broker::CommandOutput> {
    if let Ok(plan) = command.plan() {
        h.log(format!("$ {} (payload on stdin)", plan.display()));
    }
    let (out, channel) =
        super::broker::run_privileged_with_key(h.db(), command, key, explicit, timeout).await?;
    h.log(format!("channel: {}", channel.as_str()));
    h.log(&out.stdout);
    h.log(&out.stderr);
    if !out.success() {
        return Err(anyhow!(
            "{} exited with {}: {}",
            command_label(command),
            out.code,
            out.stderr.trim().lines().next().unwrap_or("no output")
        ));
    }
    Ok(out)
}

fn command_label(command: &HelperCommand) -> &'static str {
    match command {
        HelperCommand::ZpoolCreate { .. }
        | HelperCommand::ZpoolDestroy { .. }
        | HelperCommand::ZpoolScrub { .. }
        | HelperCommand::ZpoolExport { .. }
        | HelperCommand::ZpoolImportScan {}
        | HelperCommand::ZpoolImport { .. }
        | HelperCommand::ZpoolAdd { .. }
        | HelperCommand::ZpoolAttach { .. }
        | HelperCommand::ZpoolRemove { .. }
        | HelperCommand::ZpoolReplace { .. }
        | HelperCommand::ZpoolDetach { .. }
        | HelperCommand::ZpoolOffline { .. }
        | HelperCommand::ZpoolOnline { .. }
        | HelperCommand::ZpoolClear { .. }
        | HelperCommand::ZpoolTrim { .. }
        | HelperCommand::ZpoolSet { .. } => "zpool",
        HelperCommand::ZfsCreate { .. }
        | HelperCommand::ZfsDestroy { .. }
        | HelperCommand::ZfsSet { .. }
        | HelperCommand::ZfsInherit { .. }
        | HelperCommand::ZfsSnapshot { .. }
        | HelperCommand::ZfsHold { .. }
        | HelperCommand::ZfsRelease { .. }
        | HelperCommand::ZfsRollback { .. }
        | HelperCommand::ZfsClone { .. }
        | HelperCommand::ZfsMount { .. }
        | HelperCommand::ZfsUnmount { .. }
        | HelperCommand::ZfsLoadKey { .. }
        | HelperCommand::ZfsUnloadKey { .. } => "zfs",
        HelperCommand::SmartctlInfo { .. } | HelperCommand::SmartctlSelfTest { .. } => "smartctl",
        HelperCommand::NvmeSmartLog { .. } => "nvme",
        HelperCommand::Locate { .. } => "ledctl",
        HelperCommand::PackageInstall { .. } => "the package manager",
        HelperCommand::SmbIncludeEnsure {}
        | HelperCommand::SmbIncludeRemove {}
        | HelperCommand::SmbConfigWrite {}
        | HelperCommand::SmbUserSet { .. }
        | HelperCommand::SmbUserDelete { .. }
        | HelperCommand::SmbStatus {} => "samba",
        HelperCommand::SmbAuditRead { .. } => "the access audit",
        HelperCommand::AuditRulesWrite {} | HelperCommand::AuditRulesClear {} => "auditd",
        HelperCommand::KsmbdConfigWrite {}
        | HelperCommand::KsmbdConfigClear {}
        | HelperCommand::KsmbdUserSet { .. }
        | HelperCommand::KsmbdUserDelete { .. } => "ksmbd",
        HelperCommand::NfsExportsWrite {} => "exportfs",
        HelperCommand::NfsRdmaSet { .. } | HelperCommand::NfsRdmaClear {} => "the NFS transport",
        HelperCommand::ShareChown { .. } => "the share root",
        HelperCommand::FleetMount { .. } | HelperCommand::FleetUmount { .. } => "mount",
        HelperCommand::ArcLimitSet { .. } | HelperCommand::ArcLimitClear {} => "the ARC limit",
        HelperCommand::BlockModulesLoad { .. } => "the kernel target modules",
        HelperCommand::IscsiTargetApply {} | HelperCommand::IscsiTargetRemove { .. } => {
            "the iSCSI target"
        }
        HelperCommand::NvmetSubsystemApply {} | HelperCommand::NvmetSubsystemRemove { .. } => {
            "the NVMe-oF subsystem"
        }
        HelperCommand::NvmetSessionsRead {} => "the NVMe-oF controller list",
        HelperCommand::ElasticCreate { .. } | HelperCommand::ElasticRestore { .. }
        | HelperCommand::ElasticInspect { .. } | HelperCommand::ElasticCacheAge { .. }
        | HelperCommand::ElasticFolderUsage { .. }
        | HelperCommand::ElasticClaims { .. }
        | HelperCommand::ElasticJournals {} | HelperCommand::ElasticAdopt { .. }
        | HelperCommand::ElasticSync { .. } | HelperCommand::ElasticScrub { .. }
        | HelperCommand::ElasticEnterService { .. } | HelperCommand::ElasticResume { .. }
        | HelperCommand::ElasticMover { .. }
        | HelperCommand::ElasticFix { .. } | HelperCommand::ElasticAddDisk { .. }
        | HelperCommand::ElasticAddDiskAbort { .. }
        | HelperCommand::ElasticReplaceDisk { .. }
        | HelperCommand::ElasticDestroy { .. } => "Elastic Array",
        HelperCommand::DiskWipe { .. } => "wipefs",
    }
}

/// Creates the row and spawns `body`. The returned job is the row as
/// queued; callers answer with it and the UI polls.
///
/// The row's owner follows `store::insert_job`: an Elastic job belongs to
/// its array's organisation, everything else is node-wide. A job that must
/// belong to one organisation from birth uses `spawn_owned`.
pub fn spawn<F, Fut>(db: &DbPool, kind: &str, subject: &str, started_by: &str,
    intent: Option<ElasticJobIntent>, completion: Option<tokio::sync::oneshot::Sender<Result<()>>>,
    body: F) -> Result<NasJob>
where
    F: FnOnce(JobHandle) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    spawn_owned(db, kind, subject, started_by, None, intent, completion, body)
}

/// `spawn` with an explicit OWNER written by the very INSERT that creates the
/// row (`store::insert_job_owned`): a non-Elastic job that will name one
/// organisation's array — a disk wipe releasing that array's journal logs it
/// by name — is never node-wide, not even between two writes. A variant
/// rather than a new parameter on `spawn` because every other caller (the
/// scheduler, the pools, the snapshots, the Elastic paths) has no owner to
/// give, and none of them should have to spell `None`.
#[allow(clippy::too_many_arguments)]
pub fn spawn_owned<F, Fut>(db: &DbPool, kind: &str, subject: &str, started_by: &str,
    owner: Option<&str>,
    intent: Option<ElasticJobIntent>, completion: Option<tokio::sync::oneshot::Sender<Result<()>>>,
    body: F) -> Result<NasJob>
where
    F: FnOnce(JobHandle) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    spawn_inner(db, kind, subject, started_by, owner, intent, completion, &[], body)
}

/// One SMART self-test job over several disks (`store::SMART_BATCH_KIND`).
/// `disks` is `(disk_id, kernel name)` in run order; the lines are written by
/// the same transaction as the job row (`store::insert_job_full`), which is
/// where a disk that is already testing is marked refused.
pub fn spawn_smart_batch<F, Fut>(db: &DbPool, subject: &str, started_by: &str,
    disks: &[(String, String)], body: F) -> Result<NasJob>
where
    F: FnOnce(JobHandle) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    spawn_inner(db, store::SMART_BATCH_KIND, subject, started_by, None, None, None, disks, body)
}

#[allow(clippy::too_many_arguments)]
fn spawn_inner<F, Fut>(db: &DbPool, kind: &str, subject: &str, started_by: &str,
    owner: Option<&str>,
    intent: Option<ElasticJobIntent>, completion: Option<tokio::sync::oneshot::Sender<Result<()>>>,
    disks: &[(String, String)],
    body: F) -> Result<NasJob>
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
        subject_last_known: false,
        disks: Vec::new(),
    };
    // `intent.is_none()` alone made the one IRREVERSIBLE job of this family the
    // only cancellable one: a disk wipe carries no elastic intent, so it got a
    // Cancel button. Cancelling drops the future, never the helper process —
    // which by then has released the array journal and is inside
    // `wipefs --all`. The row would be written `cancelled` with no error while
    // the disk was being erased, which is the worst possible thing for a job
    // log to say. A job is cancellable only if cancelling it can still stop
    // the work.
    let cancellable = intent.is_none() && kind != "disk_wipe";
    // These close their own operation row — maintenance and the mover inside
    // `finish_job`, from the result the body recorded; the add inside its own
    // body, because the member row and the operation have to land together.
    // The blanket `fail_elastic_job` below would put the ARRAY into
    // needs_attention for a run that merely reported a failure it already
    // persisted, so they are excluded from it.
    let self_closing = matches!(
        intent,
        Some(
            ElasticJobIntent::Snapraid { .. }
                | ElasticJobIntent::Mover { .. }
                | ElasticJobIntent::AddDisk { .. }
                | ElasticJobIntent::AddDiskAbort { .. }
                | ElasticJobIntent::ReplaceDisk { .. }
        )
    );
    let mut registry = running().lock().unwrap_or_else(|p| p.into_inner());
    store::insert_job_full(db, &job, intent.as_ref(), owner, disks)?;
    let cancel = CancellationToken::new();
    registry.insert(job.job_id.clone(), RunningJob { cancel: cancel.clone(), cancellable });
    drop(registry);
    let handle = JobHandle {
        db: db.clone(),
        job_id: job.job_id.clone(),
        cancel: cancel.clone(),
        extra_names: Arc::default(),
    };
    let error_names = handle.clone();
    let db = db.clone();
    let job_id = job.job_id.clone();
    tokio::spawn(async move {
        let mut outcome = std::panic::AssertUnwindSafe(async {
          if cancellable {
            tokio::select! {
                r = body(handle) => r,
                _ = cancel.cancelled() => Err(anyhow!("cancelled")),
            }
        } else {
            body(handle).await
          }
        }).catch_unwind().await.unwrap_or_else(|_| Err(anyhow!("Przerwanie wykonawcy zadania; stan I/O niepotwierdzony")));
        if !cancellable && !self_closing {
            if let Err(error) = &outcome {
                if let Err(persist) = store::fail_elastic_job(&db,&job_id,&error.to_string()) {
                    tracing::error!("tentanas job {job_id}: nie utrwalono needs_attention: {persist}");
                    outcome = Err(anyhow!("{error}; nie utrwalono needs_attention: {persist}"));
                }
            }
        }
        // The error is the log's last word and follows its rule: no ids.
        let (status, error) = match &outcome {
            Ok(()) => ("succeeded", None),
            Err(e) if e.to_string() == "cancelled" => ("cancelled", None),
            Err(e) => ("failed", Some(error_names.names().scrub(&e.to_string()))),
        };
        if let Err(e) = store::finish_job(&db, &job_id, status, error.as_deref()) {
            tracing::warn!("tentanas job {job_id}: finish write failed: {e}");
            outcome = Err(anyhow!("Nie utrwalono zakończenia zadania: {e}"));
        }
        running()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&job_id);
        if let Some(completion) = completion {
            let _ = completion.send(outcome);
        }
    });
    Ok(job)
}

/// The job kinds an admin may cancel: those where cancelling really stops the
/// work. Every other kind runs its command through the root helper (TRIM,
/// pool create/replace, dataset destroy, share/target apply, imports…) or
/// keeps going on the drive itself (a SMART self-test): dropping the job's
/// future would only stop TRACKING it and still write "cancelled". The same
/// allowlist as the screen's `jobCanCancel` (www/js/modules/tentanas/
/// format.js), enforced here so a request that skips the screen cannot make
/// a job read "cancelled" while its work runs on.
/// - `pool_scrub`: a guard issues `zpool scrub -s` on drop
///   (`pools::StopScrubOnCancel`), so the scrub really stops.
///
/// `snapshot_destroy` stays out by the owner's decision (wave 5): its loop
/// could stop between snapshots, but a cancelled job would then read
/// "cancelled" with some snapshots already destroyed.
///
/// `cancel_all` (the uninstall teardown) is not limited by this: there the
/// point is to stop issuing new commands, not to tell an admin a job stopped.
pub const USER_CANCELLABLE_KINDS: &[&str] = &["pool_scrub"];

/// Whether an admin's cancel request may stop a job of this kind.
pub fn user_cancellable(kind: &str) -> bool {
    USER_CANCELLABLE_KINDS.contains(&kind)
}

/// Cancels every job running on this node and returns how many there were.
/// The uninstall teardown does this first: a scrub or an import still issuing
/// commands while the channel is being taken down would leave work half-done.
pub fn cancel_all() -> usize {
    let registry = running().lock().unwrap_or_else(|p| p.into_inner());
    let mut count = 0;
    for job in registry.values().filter(|j| j.cancellable) {
        job.cancel.cancel();
        count += 1;
    }
    count
}

/// Cancels a running job; false when it is not running on this node (already
/// finished, or a job of another process lifetime).
pub fn cancel(job_id: &str) -> bool {
    match running()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(job_id)
    {
        Some(job) if job.cancellable => {
            job.cancel.cancel();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    async fn finished(db: &DbPool, id: &str) -> NasJob {
        tokio::time::timeout(Duration::from_secs(2),async {
            loop {
                let job=store::job(db,id).unwrap().unwrap();
                if job.finished_at.is_some() { return job; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap()
    }

    /// Owner decision 2026-09-26: whatever a job body writes — its own
    /// sentence, a tool's output, the error it ends with — reaches the log
    /// with its ids named or hidden. Through a real `spawn`, so a writer that
    /// bypassed `JobHandle::log` (or an error stored unscrubbed) fails here.
    #[tokio::test]
    async fn a_job_log_and_its_error_carry_no_ids() {
        let db = database();
        let array_id = "0191f2c0-7a3b-7c11-9d2e-1234567890ab";
        db.write().unwrap().execute(
            "INSERT INTO nas_elastic_arrays (array_id, org_id, addon_id, name, filesystem, state, state_detail, created_at, updated_at) \
             VALUES (?1, 'org-a', 'nas', 'media', 'xfs', 'active', '', 'now', 'now')",
            rusqlite::params![array_id],
        ).unwrap();
        let gone = "5f1e2d3c-aaaa-bbbb-cccc-1234567890ab";
        let job = spawn(&db, "disk_wipe", "sdb", "test", None, None, move |h| async move {
            h.log(format!("rm /var/lib/tentanas/{array_id}.json"));
            h.name_id(gone, "archive");
            h.log(format!("journal {gone} released\n$ zpool detach tank /dev/disk/by-id/wwn-0x5000c500ffffffee"));
            h.log("$ zpool detach tank 12345678901234567890");
            h.log("usunięto sygnaturę xfs na 0x0 (uuid 11111111-2222-3333-4444-555555555555)");
            Err(anyhow!("wipefs failed on /dev/disk/by-id/wwn-0x5000c500ffffffff (job {array_id})"))
        })
        .unwrap();
        let done = finished(&db, &job.job_id).await;
        assert_eq!(
            done.log,
            vec![
                "rm /var/lib/tentanas/media.json".to_string(),
                "journal archive released".to_string(),
                "$ zpool detach tank /dev/disk/by-id/⟦id⟧".to_string(),
                "$ zpool detach tank ⟦id⟧".to_string(),
                "usunięto sygnaturę xfs na 0x0 (uuid ⟦id⟧)".to_string(),
            ]
        );
        assert_eq!(
            done.error.as_deref(),
            Some("wipefs failed on /dev/disk/by-id/⟦id⟧ (job media)")
        );
    }

    /// A2/A3/A5: a cancel an admin can ask for must really stop the work.
    /// Only a scrub has a guard that does (`zpool scrub -s` on drop); a
    /// SMART test keeps running on the drive, a helper command keeps running
    /// as root, so neither may be cancelled on request.
    #[test]
    fn only_a_scrub_is_cancellable_on_request() {
        assert!(user_cancellable("pool_scrub"));
        for kind in ["smart_test", "pool_trim", "pool_create", "pool_replace", "dataset_destroy", "snapshot_destroy",
            "disk_wipe", "config_import", "elastic_create", "elastic_restore", "elastic_sync", ""] {
            assert!(!user_cancellable(kind), "{kind}");
        }
    }

    #[tokio::test]
    async fn elastic_body_sees_committed_intent_and_cannot_be_cancelled_or_orphaned() {
        let db=database();
        let spec=super::super::elastic::tests::create_spec("atomic");
        let work=spec.clone();
        let (entered_tx,entered_rx)=tokio::sync::oneshot::channel();
        let (release_tx,release_rx)=tokio::sync::oneshot::channel();
        let job=spawn(&db,"elastic_create",&spec.name,"test",Some(ElasticJobIntent::Create(spec.clone())),None,move |h| async move {
            let stored=store::elastic_array(h.db(),&work.owner,&work.name)?.unwrap();
            assert_eq!(stored.persisted_spec().unwrap(),&work);
            assert_eq!(store::elastic_claims(h.db())?.len(),2);
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            assert!(!h.cancelled());
            store::finish_elastic_operation(h.db(),&work.owner,&work.operation_id,
                Ok(&super::super::elastic::tests::ready_result(&work)))?;
            Ok(())
        }).unwrap();
        entered_rx.await.unwrap();
        assert!(!cancel(&job.job_id));
        cancel_all();
        assert_eq!(store::fail_orphaned_jobs(&db).unwrap(),0);
        assert_eq!(store::job(&db,&job.job_id).unwrap().unwrap().status,"running");
        release_tx.send(()).unwrap();
        assert_eq!(finished(&db,&job.job_id).await.status,"succeeded");
        assert_eq!(store::elastic_array(&db,&spec.owner,&spec.name).unwrap().unwrap().state,"active");
    }

    #[tokio::test]
    async fn completion_waits_for_body_persistence_and_registry_removal() {
        let db = database();
        let spec = super::super::elastic::tests::create_spec("completion");
        let work = spec.clone();
        let (release, released) = tokio::sync::oneshot::channel();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (complete, mut completed) = tokio::sync::oneshot::channel();
        let job = spawn(
            &db,
            "elastic_create",
            &spec.name,
            "test",
            Some(ElasticJobIntent::Create(spec.clone())),
            Some(complete),
            move |h| async move {
                entered.send(()).unwrap();
                released.await.unwrap();
                store::finish_elastic_operation(
                    h.db(),
                    &work.owner,
                    &work.operation_id,
                    Ok(&super::super::elastic::tests::ready_result(&work)),
                )
            },
        )
        .unwrap();
        entering.await.unwrap();
        assert!(matches!(
            completed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(running().lock().unwrap().contains_key(&job.job_id));
        release.send(()).unwrap();
        completed.await.unwrap().unwrap();
        assert!(!running().lock().unwrap().contains_key(&job.job_id));
        assert_eq!(
            store::job(&db, &job.job_id).unwrap().unwrap().status,
            "succeeded"
        );
        assert_eq!(
            store::elastic_array(&db, &spec.owner, &spec.name)
                .unwrap()
                .unwrap()
                .state,
            "active"
        );
        let state: String = db
            .read()
            .unwrap()
            .query_row(
                "SELECT state FROM nas_elastic_operations WHERE job_id=?1",
                [&job.job_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "succeeded");
    }

    #[tokio::test]
    async fn completion_reports_body_error_and_panic_after_attention_is_persisted() {
        for crash in [false, true] {
            let db = database();
            let spec = super::super::elastic::tests::create_spec("completion-error");
            let (complete, completed) = tokio::sync::oneshot::channel();
            let job = spawn(
                &db,
                "elastic_create",
                &spec.name,
                "test",
                Some(ElasticJobIntent::Create(spec.clone())),
                Some(complete),
                move |_| async move {
                    assert!(!crash, "kontrolowana panika");
                    Err(anyhow!("kontrolowany błąd wykonawcy"))
                },
            )
            .unwrap();
            assert!(completed.await.unwrap().is_err());
            assert!(!running().lock().unwrap().contains_key(&job.job_id));
            assert_eq!(
                store::job(&db, &job.job_id).unwrap().unwrap().status,
                "failed"
            );
            assert_eq!(
                store::elastic_array(&db, &spec.owner, &spec.name)
                    .unwrap()
                    .unwrap()
                    .state,
                "needs_attention"
            );
        }
    }

    #[tokio::test]
    async fn completion_refuses_real_sqlite_finalization_errors() {
        for attention_failure in [false, true] {
            let db = database();
            let spec = super::super::elastic::tests::create_spec("persist-error");
            let (complete, completed) = tokio::sync::oneshot::channel();
            let job = spawn(&db, "elastic_create", &spec.name, "test",
                    Some(ElasticJobIntent::Create(spec.clone())), Some(complete), move |h| async move {
                        let table = if attention_failure { "nas_elastic_arrays" } else { "nas_jobs" };
                        h.db().write().unwrap().execute_batch(&format!(
                            "CREATE TRIGGER refuse_finish BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT,'persist denied'); END;"))?;
                        if attention_failure { Err(anyhow!("kontrolowany błąd I/O")) } else { Ok(()) }
                    }).unwrap();
            let error = completed.await.unwrap().unwrap_err().to_string();
            assert!(error.contains("persist denied"), "{error}");
            assert!(!running().lock().unwrap().contains_key(&job.job_id));
            assert_eq!(
                store::job(&db, &job.job_id).unwrap().unwrap().status,
                if attention_failure {
                    "failed"
                } else {
                    "running"
                }
            );
        }
    }

    #[tokio::test]
    async fn dropping_completion_receiver_does_not_cancel_elastic_body() {
        let db = database();
        let spec = super::super::elastic::tests::create_spec("dropped-receiver");
        let work = spec.clone();
        let (complete, completed) = tokio::sync::oneshot::channel();
        drop(completed);
        let job = spawn(
            &db,
            "elastic_create",
            &spec.name,
            "test",
            Some(ElasticJobIntent::Create(spec.clone())),
            Some(complete),
            move |h| async move {
                assert!(!h.cancelled());
                store::finish_elastic_operation(
                    h.db(),
                    &work.owner,
                    &work.operation_id,
                    Ok(&super::super::elastic::tests::ready_result(&work)),
                )
            },
        )
        .unwrap();
        assert_eq!(finished(&db, &job.job_id).await.status, "succeeded");
        assert!(!running().lock().unwrap().contains_key(&job.job_id));
        assert_eq!(
            store::elastic_array(&db, &spec.owner, &spec.name)
                .unwrap()
                .unwrap()
                .state,
            "active"
        );
    }

    /// A job spawned WITH an owner (the journal-releasing disk wipe) is that
    /// organisation's from its INSERT: the returned row already reads as
    /// org A's and never as org B's, the body — the first code that could log
    /// the array's name — finds it owned when it starts, and it stays owned
    /// once finished. A plain `spawn` of the same kind stays node-wide.
    #[tokio::test]
    async fn a_job_spawned_with_an_owner_is_owned_from_the_moment_it_exists() {
        let db = database();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let job = spawn_owned(&db, "disk_wipe", "sdq", "u-a", Some("org-a"), None, Some(tx), |h| async move {
            anyhow::ensure!(store::job_for_org(h.db(), "org-b", &h.job_id)?.is_none(),
                "another tenant saw the job while its body ran");
            anyhow::ensure!(store::job_for_org(h.db(), "org-a", &h.job_id)?.is_some(),
                "the owner did not see its own job");
            Ok(())
        })
        .unwrap();
        // Before the body has had any chance to run.
        assert!(store::job_for_org(&db, "org-b", &job.job_id).unwrap().is_none());
        assert!(store::job_for_org(&db, "org-a", &job.job_id).unwrap().is_some());
        rx.await.unwrap().unwrap();
        assert_eq!(finished(&db, &job.job_id).await.status, "succeeded");
        assert!(store::job_for_org(&db, "org-b", &job.job_id).unwrap().is_none());

        let shared = spawn(&db, "disk_wipe", "sdr", "u-a", None, None, |_| async { Ok(()) }).unwrap();
        assert!(store::job_for_org(&db, "org-b", &shared.job_id).unwrap().is_some(), "node-wide");

        // One owner rule: an Elastic kind takes its array's organisation and
        // refuses an explicit one before any row or body exists.
        assert!(spawn_owned(&db, "elastic_sync", "alpha", "u-a", Some("org-a"), None, None,
            |_| async { Ok(()) }).is_err());
    }

    #[tokio::test]
    async fn failed_elastic_transaction_never_starts_body() {
        let db=database();
        let spec=super::super::elastic::tests::create_spec("rollback");
        let before=NasJob { job_id:uuid::Uuid::new_v4().to_string(),kind:"elastic_create".into(),
            subject:spec.name.clone(),status:"running".into(),started_at:store::now(),..Default::default() };
        store::insert_job(&db,&before,Some(&ElasticJobIntent::Create(spec.clone()))).unwrap();
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let body_calls=calls.clone();
        assert!(spawn(&db,"elastic_create",&spec.name,"test",Some(ElasticJobIntent::Create(spec.clone())),None,move |_| async move {
            body_calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst); Ok(())
        }).is_err());
        tokio::task::yield_now().await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
        assert_eq!(store::list_jobs(&db,100).unwrap().len(),1);
    }

    #[tokio::test]
    async fn panic_in_elastic_body_keeps_reservations_and_marks_attention() {
        let db=database();
        let spec=super::super::elastic::tests::create_spec("panic");
        fn crash() -> Result<()> { panic!("kontrolowana awaria wykonawcy") }
        let job=spawn(&db,"elastic_create",&spec.name,"test",Some(ElasticJobIntent::Create(spec.clone())),None,|_| async { crash() }).unwrap();
        assert_eq!(finished(&db,&job.job_id).await.status,"failed");
        let row=store::elastic_array(&db,&spec.owner,&spec.name).unwrap().unwrap();
        assert_eq!(row.state,"needs_attention");
        assert_eq!(row.persisted_spec().unwrap(),&spec);
    }

    #[tokio::test]
    async fn late_elastic_request_after_teardown_never_inserts_or_runs() {
        let db=database();
        let spec=super::super::elastic::tests::create_spec("late");
        store::block_elastic_teardown(&db).unwrap();
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let body_calls=calls.clone();
        let result=spawn(&db,"elastic_create",&spec.name,"test",Some(ElasticJobIntent::Create(spec.clone())),None,move |_| async move {
            body_calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst); Ok(())
        });
        assert!(result.unwrap_err().to_string().contains("usuwanie instancji"));
        tokio::task::yield_now().await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
        assert!(store::list_jobs(&db,100).unwrap().is_empty());
        assert!(store::elastic_claims(&db).unwrap().is_empty());
        assert!(store::setting(&db,"elastic_teardown_started").unwrap().is_some());
    }

    #[tokio::test]
    async fn reserved_elastic_array_blocks_teardown_without_setting_marker() {
        let db=database();
        let spec=super::super::elastic::tests::create_spec("preserved");
        let work=spec.clone();
        let job=spawn(&db,"elastic_create",&spec.name,"test",Some(ElasticJobIntent::Create(spec.clone())),None,move |h| async move {
            store::finish_elastic_operation(h.db(),&work.owner,&work.operation_id,Err("Awaria przed odpowiedzią"))?;
            Err(anyhow!("kontrolowana odmowa"))
        }).unwrap();
        finished(&db,&job.job_id).await;
        assert!(store::block_elastic_teardown(&db).is_err());
        assert!(store::setting(&db,"elastic_teardown_started").unwrap().is_none());
        assert_eq!(store::elastic_array(&db,&spec.owner,&spec.name).unwrap().unwrap().persisted_spec().unwrap(),&spec);
    }

    // ----- SMART self-test polling -------------------------------------------

    /// A real SAS disk, captured verbatim from a node: the SCSI self-test log
    /// lives in numbered top-level keys, carries no percentage anywhere and
    /// nothing that identifies which run wrote an entry.
    const SMART_SAS: &str = include_str!("../../tests/fixtures/smart-sas-sdb.json");

    /// The captured document with its self-test log replaced by `entries`,
    /// newest first — everything else stays as the disk emitted it.
    fn sas_doc(entries: &[Value]) -> Value {
        let mut doc: Value = serde_json::from_str(SMART_SAS).expect("captured SAS document");
        let map = doc.as_object_mut().expect("the capture is an object");
        for i in 0..20 {
            map.remove(&format!("scsi_self_test_{i}"));
        }
        for (i, e) in entries.iter().enumerate() {
            map.insert(format!("scsi_self_test_{i}"), e.clone());
        }
        doc
    }

    /// One SCSI log entry in the shape the captured disk emits.
    fn scsi_entry(result: u64, string: &str, hours: u64) -> Value {
        serde_json::json!({
            "code": {"value": 1, "string": "Background short"},
            "result": {"value": result, "string": string},
            "power_on_time": {"hours": hours},
        })
    }

    fn log_of(doc: &Value) -> Vec<NasSmartSelfTest> {
        crate::tentanas::disks::smart_self_tests(doc)
    }

    /// A SAS disk reports a running test as an in-progress entry in the log
    /// itself (SPC result code 15) and gives no percentage at all, so reading a
    /// missing percentage as "the test is over" declared a 24-hour test done
    /// after the first 20-second poll.
    #[test]
    fn a_running_scsi_self_test_is_not_a_finished_run() {
        let before = sas_doc(&[scsi_entry(0, "Completed", 32392)]);
        let running = sas_doc(&[
            scsi_entry(15, "Self test in progress ...", 32392),
            scsi_entry(0, "Completed", 32392),
        ]);
        // The missing percentage is exactly the signal that was misread.
        assert_eq!(
            crate::tentanas::disks::summarize_smart(&running).self_test_running_pct,
            None
        );
        assert_eq!(
            classify_self_test_poll(&running, &log_of(&before)),
            SelfTestPoll::Running(None)
        );
    }

    /// A failure left by an EARLIER test is the newest entry until this run
    /// writes its own, and it must not be reported as this run's verdict.
    #[test]
    fn a_stale_scsi_failure_does_not_fail_a_fresh_run() {
        let before = sas_doc(&[scsi_entry(5, "Failed in first segment", 30000)]);
        assert_eq!(
            classify_self_test_poll(&before, &log_of(&before)),
            SelfTestPoll::Stalled
        );
        // Once the disk pushes this run's entry the log has moved, and then the
        // newest entry really is the verdict.
        let done = sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(5, "Failed in first segment", 30000),
        ]);
        match classify_self_test_poll(&done, &log_of(&before)) {
            SelfTestPoll::Finished(Some(t)) => {
                assert_eq!(t.status, "passed");
                assert_eq!(t.lifetime_hours, 32392);
            }
            other => panic!("expected this run's result, got {other:?}"),
        }
    }

    /// A re-run whose entry is identical to the one before it still moved the
    /// log: what used to be newest is now second.
    #[test]
    fn an_identical_rerun_entry_still_counts_as_this_runs_result() {
        let before = sas_doc(&[scsi_entry(0, "Completed", 32392)]);
        let again = sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 32392),
        ]);
        assert!(matches!(
            classify_self_test_poll(&again, &log_of(&before)),
            SelfTestPoll::Finished(Some(_))
        ));
    }

    /// ATA reports its own progress and its status block is authoritative in
    /// both directions; comparing logs must not change that reading.
    #[test]
    fn an_ata_self_test_is_still_read_from_its_own_status_block() {
        let running = serde_json::json!({
            "ata_smart_data": {"self_test": {"status": {"remaining_percent": 40}}},
        });
        assert_eq!(
            classify_self_test_poll(&running, &[]),
            SelfTestPoll::Running(Some(60))
        );

        // The block without a remaining percentage means the disk says it is
        // over — even though the log has not moved against the baseline.
        let done = serde_json::json!({
            "ata_smart_data": {"self_test": {"status": {"passed": true}}},
            "ata_smart_self_test_log": {"standard": {"table": [
                {"type": {"string": "Short offline"},
                 "status": {"string": "Completed without error", "passed": true},
                 "lifetime_hours": 12000}
            ]}},
        });
        match classify_self_test_poll(&done, &log_of(&done)) {
            SelfTestPoll::Finished(Some(t)) => assert_eq!(t.status, "passed"),
            other => panic!("ATA must finish on its own status block, got {other:?}"),
        }
    }

    /// The window that bounds a log which never moves comes from the disk's own
    /// advertised duration, not from a guess.
    #[test]
    fn the_stall_window_follows_the_disks_advertised_duration() {
        let doc = sas_doc(&[scsi_entry(0, "Completed", 32392)]);
        // The captured disk advertises 88200 s (24.5 h) for its extended test.
        assert_eq!(
            doc.get("scsi_extended_self_test_seconds").and_then(Value::as_u64),
            Some(88200)
        );
        assert_eq!(
            self_test_window(&doc, SelfTestKind::Long),
            Duration::from_secs(176_400)
        );
        // SCSI advertises nothing for a short test, so the floor applies.
        assert_eq!(
            self_test_window(&doc, SelfTestKind::Short),
            Duration::from_secs(15 * 60)
        );
        // A document that advertises nothing at all still gets a window.
        assert_eq!(
            self_test_window(&Value::Null, SelfTestKind::Long),
            Duration::from_secs(6 * 60 * 60)
        );
    }

    /// The poll loop MUST terminate. The stall bound alone does not make it:
    /// a disk left holding a stale "in progress" entry (an interrupted test, a
    /// power loss) answers `Running` to every poll, which pushed the stall
    /// bound back every time, and the job then polled a dead test forever.
    #[test]
    fn the_poll_loop_terminates_even_when_every_poll_reports_progress() {
        let window = Duration::from_secs(3600);
        // `since_sign` is zero on every call: this is exactly the stale
        // in-progress entry, resetting the rolling bound at every poll.
        let forever = Duration::ZERO;
        assert_eq!(self_test_timeout(window, Duration::from_secs(3600), forever), None);
        assert_eq!(self_test_timeout(window, Duration::from_secs(7199), forever), None);
        // One window short of the cap is still inside it; the cap itself is not.
        let (timeout, bound) = self_test_timeout(window, Duration::from_secs(7200), forever)
            .expect("a run that keeps reporting progress must still be capped");
        assert_eq!(timeout, SelfTestTimeout::Cap);
        assert_eq!(bound, Duration::from_secs(7200), "the cap is twice the window");
    }

    /// An operator chasing a timeout has to be able to tell a disk that never
    /// showed the run from one whose progress reports stopped being believed.
    /// The two bounds are also ordered: the stall bound lies strictly inside
    /// the cap, so adding the cap cannot cut short a run the stall bound would
    /// have allowed.
    #[test]
    fn a_hard_cap_and_a_stall_are_told_apart_by_their_error() {
        let window = Duration::from_secs(3600);
        // A disk that never shows the run at all: `since_sign` tracks `elapsed`.
        let (timeout, bound) = self_test_timeout(window, window, window)
            .expect("a disk that never shows the run must stall out");
        assert_eq!(timeout, SelfTestTimeout::Stall);
        assert_eq!(bound, window, "a stall is reported against the window, not the cap");

        let stall = SelfTestTimeout::Stall.error(window).to_string();
        let cap = SelfTestTimeout::Cap.error(window * 2).to_string();
        assert_eq!(stall, "the disk recorded no self-test result within 60 min");
        assert!(
            cap.contains("still reported the test as running after 120 min"),
            "the cap must say the disk kept claiming progress, got {cap:?}"
        );
        assert!(
            !cap.contains("recorded no self-test result"),
            "the cap must not reuse the stall wording, got {cap:?}"
        );
    }

    /// A failed pre-start read leaves the window on its FLOOR, because the
    /// advertised duration is read from the very document that failed to load:
    /// 6 h for a long test, not the 24.5 h the disk itself advertises. The
    /// duration is static device metadata, so a later successful poll is just
    /// as good a source for it.
    #[test]
    fn the_window_is_rederived_from_a_poll_when_the_pre_read_gave_nothing() {
        let floor = self_test_window(&Value::Null, SelfTestKind::Long);
        assert_eq!(floor, Duration::from_secs(6 * 60 * 60));

        // ATA finishes on its own status block, so its run can be reported
        // with no baseline at all and a longer window strictly helps it.
        let ata = serde_json::json!({
            "ata_smart_data": {"self_test": {
                "status": {"remaining_percent": 40},
                "polling_minutes": {"extended": 300},
            }},
        });
        assert_eq!(
            rederived_self_test_window(floor, &ata, SelfTestKind::Long, &[]),
            Some(Duration::from_secs(600 * 60)),
            "300 advertised minutes, doubled"
        );

        // A SAS poll with no baseline can never have its result attributed to
        // this run, so stretching its window only delays the same error.
        let sas = sas_doc(&[scsi_entry(0, "Completed", 32392)]);
        assert_eq!(rederived_self_test_window(floor, &sas, SelfTestKind::Long, &[]), None);
        // With a baseline the log can move, and then the disk's real 24.5 h
        // figure replaces the floor.
        assert_eq!(
            rederived_self_test_window(floor, &sas, SelfTestKind::Long, &log_of(&sas)),
            Some(Duration::from_secs(176_400))
        );
        // It only ever grows: a window already in force is never shortened.
        assert_eq!(
            rederived_self_test_window(
                Duration::from_secs(200_000),
                &sas,
                SelfTestKind::Long,
                &log_of(&sas)
            ),
            None
        );
    }

    /// smartctl publishes NO self-test duration for NVMe, so an NVMe test runs
    /// on the 6 h floor. What made that harmful was that a running NVMe test
    /// looked like a stall: the log gains its entry only once the test is
    /// over, so every poll in between read as "nothing belongs to this run".
    /// NVMe does report progress — in the self-test log header, not in any
    /// status block — and reading it keeps the stall bound pushed back for as
    /// long as the test really runs.
    #[test]
    fn a_running_nvme_self_test_reports_progress_instead_of_stalling() {
        let running = serde_json::json!({
            "nvme_self_test_log": {
                "current_self_test_operation": {
                    "value": 2, "string": "Extended self-test operation in progress"
                },
                "current_self_test_completion_percent": 37,
                "table": [],
            },
        });
        // Unlike ATA this is the percentage already DONE, so it is not inverted.
        assert_eq!(
            classify_self_test_poll(&running, &[]),
            SelfTestPoll::Running(Some(37))
        );

        // Operation 0 is "no self-test in progress", and smartctl then omits
        // the percentage entirely; that is not a running test.
        let idle = serde_json::json!({
            "nvme_self_test_log": {
                "current_self_test_operation": {
                    "value": 0, "string": "No device self-test operation in progress"
                },
                "table": [],
            },
        });
        assert_eq!(classify_self_test_poll(&idle, &[]), SelfTestPoll::Stalled);
    }

    /// The loop's own decision, not just the bound it consults: a `Running`
    /// poll must NOT be the one classification that escapes the bound. It was
    /// exactly that — the only arm that pushed the deadline back without ever
    /// testing it — so a disk holding a stale in-progress entry answered
    /// `Running` forever and the job polled a dead test with no way out.
    #[test]
    fn a_running_poll_does_not_let_the_loop_escape_its_bound() {
        let window = Duration::from_secs(3600);
        let cap = window * 2;

        // Inside the cap a running poll simply reports progress.
        assert_eq!(
            self_test_step(SelfTestPoll::Running(Some(50)), window, window, Duration::ZERO),
            SelfTestStep::Progress(Some(50))
        );
        // At the cap the very same poll ends the job.
        assert_eq!(
            self_test_step(SelfTestPoll::Running(Some(50)), window, cap, Duration::ZERO),
            SelfTestStep::Expired(SelfTestTimeout::Cap, cap)
        );
        // A SCSI in-progress entry carries no percentage and is bounded alike.
        assert_eq!(
            self_test_step(SelfTestPoll::Running(None), window, cap, Duration::ZERO),
            SelfTestStep::Expired(SelfTestTimeout::Cap, cap)
        );

        // A running poll is itself the sign, so a long silence before it does
        // not stall the run out.
        assert_eq!(
            self_test_step(SelfTestPoll::Running(None), window, window, window * 10),
            SelfTestStep::Progress(None)
        );
        // A silent poll after that same silence does.
        assert_eq!(
            self_test_step(SelfTestPoll::Stalled, window, window, window),
            SelfTestStep::Expired(SelfTestTimeout::Stall, window)
        );

        // A result that arrived is reported as a result however late it is:
        // the bounds never discard a verdict the disk actually gave.
        assert_eq!(
            self_test_step(SelfTestPoll::Finished(None), window, cap * 9, cap * 9),
            SelfTestStep::Done(None)
        );
    }

    /// A poll whose document could not be read is the other way the loop used
    /// to escape its bound: the read-failure arm `continue`d, skipping the
    /// check outright, so a device that had gone away or a credential that
    /// stopped working failed every read and the job polled forever.
    #[test]
    fn a_poll_that_could_not_be_read_is_no_sign_of_the_run() {
        let window = Duration::from_secs(3600);
        assert_eq!(classify_self_test_read(None, &[]), SelfTestPoll::Stalled);
        // A readable document still classifies exactly as before.
        let running = sas_doc(&[scsi_entry(15, "Self test in progress ...", 32392)]);
        assert_eq!(
            classify_self_test_read(Some(&running), &log_of(&sas_doc(&[]))),
            SelfTestPoll::Running(None)
        );
        // And an unreadable poll runs the stall bound down instead of being
        // waved through.
        assert_eq!(
            self_test_step(classify_self_test_read(None, &[]), window, window, window),
            SelfTestStep::Expired(SelfTestTimeout::Stall, window)
        );
    }

    /// A job row answers one question — did THIS run complete, and pass? — and
    /// the disk's health answers another. Reusing one status string for both
    /// showed the admin a green "succeeded" job for a run that never finished:
    /// an abort at hour 20 of a 24.5-hour test, or SPC 3, whose own smartctl
    /// text says "incomplete". Only a passed entry may end the job green.
    #[test]
    fn only_a_passed_self_test_is_a_successful_job() {
        let outcome = |result: u64, string: &str| {
            let log = log_of(&sas_doc(&[scsi_entry(result, string, 32392)]));
            self_test_outcome(log.first()).map_err(|e| e.to_string())
        };

        assert_eq!(outcome(0, "Completed"), Ok("result: Completed".to_string()));

        // A segment failure (SPC 4–7) is the one case that is a verdict on the
        // hardware: the disk took the test and failed it.
        assert_eq!(
            outcome(5, "Failed in first segment").unwrap_err(),
            "self-test failed: Failed in first segment"
        );

        // An abort stopped the run; it says nothing about the disk. SPC 1 is
        // the user's own command, SPC 2 a device reset — the raw string is what
        // tells the two apart, so it is carried through verbatim.
        assert_eq!(
            outcome(1, "Aborted (by user command)").unwrap_err(),
            "self-test did not complete: Aborted (by user command)"
        );
        assert_eq!(
            outcome(2, "Aborted (device reset ?)").unwrap_err(),
            "self-test did not complete: Aborted (device reset ?)"
        );

        // SPC 3 and the reserved 8–14 both read as "unknown", so one message
        // serves both — and it reports what the disk actually said instead of
        // claiming a pass or a failure it cannot support.
        assert_eq!(
            outcome(3, "Unknown error, incomplete").unwrap_err(),
            "self-test gave no usable result: the disk reported \"Unknown error, incomplete\", \
             which is neither a pass nor a disk failure"
        );
        assert_eq!(
            outcome(9, "Unknown result code").unwrap_err(),
            "self-test gave no usable result: the disk reported \"Unknown result code\", \
             which is neither a pass nor a disk failure"
        );
    }

    /// The cases with nothing to read at all: a log entry carrying no result
    /// code, and a document with no entry. Neither is a pass, so neither may
    /// finish the job green.
    #[test]
    fn a_self_test_with_no_readable_result_is_not_a_success() {
        let doc = sas_doc(&[serde_json::json!({
            "code": {"value": 1, "string": "Background short"},
            "power_on_time": {"hours": 32392},
        })]);
        let log = log_of(&doc);
        assert_eq!(log[0].status, "unknown");
        assert_eq!(
            self_test_outcome(log.first()).unwrap_err().to_string(),
            "self-test gave no usable result: the disk reported no result code at all, \
             which is neither a pass nor a disk failure"
        );
        assert_eq!(
            self_test_outcome(None).unwrap_err().to_string(),
            "self-test gave no usable result: the disk recorded no self-test entry at all"
        );
    }

    /// An in-progress entry read as a verdict means the disk still calls the
    /// run unfinished — which is the opposite of a success.
    #[test]
    fn an_in_progress_entry_is_never_a_successful_job() {
        let log = log_of(&sas_doc(&[scsi_entry(15, "Self test in progress ...", 32392)]));
        assert_eq!(
            self_test_outcome(log.first()).unwrap_err().to_string(),
            "self-test did not complete: the disk still reports it as running \
             (Self test in progress ...)"
        );
    }

    // ----- what counts as "the log moved" ------------------------------------

    /// A log whose two newest entries were ALREADY identical before the start
    /// has not moved just because it still looks that way. Comparing only the
    /// top two entries called it moved on the very first poll, which reported a
    /// 24-hour test complete 20 seconds in — the original bug in a new shape.
    #[test]
    fn two_identical_newest_entries_are_not_a_result_until_the_log_moves() {
        let before = sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 32392),
        ]);
        let baseline = log_of(&before);
        assert!(!self_test_log_advanced(&baseline, &baseline));
        assert_eq!(classify_self_test_poll(&before, &baseline), SelfTestPoll::Stalled);

        // The run's own entry, identical for a third time, still makes the log
        // grow — and that growth is the signal.
        let done = sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 32392),
        ]);
        assert!(self_test_log_advanced(&log_of(&done), &baseline));
        assert!(matches!(
            classify_self_test_poll(&done, &baseline),
            SelfTestPoll::Finished(Some(_))
        ));
    }

    /// An unreadable pre-start read leaves no baseline, and then NOTHING in the
    /// log can be attributed to this run. The old fallback — take the newest
    /// entry anyway — is wrong in both directions, so both are pinned here.
    #[test]
    fn an_empty_baseline_attributes_nothing_to_this_run() {
        // The direction that matters most: a failure left by an EARLIER test
        // must not fail a fresh run 20 seconds after it started. This is the
        // same bug `a_stale_scsi_failure_does_not_fail_a_fresh_run` pins for a
        // readable baseline — losing the baseline must not reopen it.
        let stale_failure = sas_doc(&[scsi_entry(5, "Failed in first segment", 30000)]);
        assert!(!self_test_log_advanced(&log_of(&stale_failure), &[]));
        assert_eq!(classify_self_test_poll(&stale_failure, &[]), SelfTestPoll::Stalled);

        // And the other direction, which is this round's whole subject: a pass
        // left by an earlier test is not a green job for a run that has barely
        // begun.
        let stale_pass = sas_doc(&[scsi_entry(0, "Completed", 32392)]);
        assert!(!self_test_log_advanced(&log_of(&stale_pass), &[]));
        assert_eq!(classify_self_test_poll(&stale_pass, &[]), SelfTestPoll::Stalled);

        // A disk that keeps no log at all gives nothing to read either way.
        assert_eq!(classify_self_test_poll(&sas_doc(&[]), &[]), SelfTestPoll::Stalled);
    }

    /// A full log cannot grow: the oldest entry falls off instead. The shift is
    /// still visible, so this run's result is still recognised.
    #[test]
    fn a_full_log_shows_this_run_because_its_oldest_entry_falls_off() {
        // 20 entries is the whole log `smart_self_tests` reads.
        let filler: Vec<Value> =
            (0..20).map(|i| scsi_entry(0, "Completed", 30000 + i)).collect();
        let before = sas_doc(&filler);
        let baseline = log_of(&before);
        assert_eq!(baseline.len(), 20);

        let mut shifted = vec![scsi_entry(5, "Failed in first segment", 32392)];
        shifted.extend(filler.iter().take(19).cloned());
        let done = sas_doc(&shifted);
        assert_eq!(log_of(&done).len(), 20);
        match classify_self_test_poll(&done, &baseline) {
            SelfTestPoll::Finished(Some(t)) => assert_eq!(t.status, "failed"),
            other => panic!("a full log still moves, got {other:?}"),
        }
    }

    /// The one case that genuinely cannot be told apart: a FULL log whose 20
    /// entries are all identical. An append pushes the oldest off and leaves a
    /// document byte-for-byte the same, so nothing observable distinguishes
    /// "this run finished" from "this run has not started writing yet". The
    /// choice pinned here is to stall — the job then ends at the window with an
    /// error — rather than guess a result the disk never showed. This is a real
    /// limitation, not an oversight.
    #[test]
    fn a_full_log_of_identical_entries_cannot_show_that_it_moved() {
        let same: Vec<Value> = (0..20).map(|_| scsi_entry(0, "Completed", 32392)).collect();
        let doc = sas_doc(&same);
        let log = log_of(&doc);
        assert_eq!(log.len(), 20);
        assert!(!self_test_log_advanced(&log, &log));
        assert_eq!(classify_self_test_poll(&doc, &log), SelfTestPoll::Stalled);
    }

    /// A disk that never writes an in-progress entry shows nothing at all while
    /// it tests: every poll of a 24-hour run looks exactly like the baseline.
    /// That is a stall, not a finished run, and the window is twice the disk's
    /// own advertised duration precisely so a silent test never reaches it.
    #[test]
    fn a_disk_that_never_reports_progress_stalls_until_it_writes_its_result() {
        let before = sas_doc(&[scsi_entry(0, "Completed", 30000)]);
        let baseline = log_of(&before);
        assert_eq!(classify_self_test_poll(&before, &baseline), SelfTestPoll::Stalled);
        assert!(self_test_window(&before, SelfTestKind::Long) >= Duration::from_secs(2 * 88200));

        let done = sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 30000),
        ]);
        match classify_self_test_poll(&done, &baseline) {
            SelfTestPoll::Finished(Some(t)) => assert_eq!(t.lifetime_hours, 32392),
            other => panic!("the final entry is the result, got {other:?}"),
        }
    }

    /// A disk that writes its in-progress entry only near the end must have it
    /// read as progress wherever it appears: SPC 15 pushes the stall window
    /// back instead of ending the job on an entry that is not a verdict.
    #[test]
    fn a_late_in_progress_entry_is_progress_and_not_a_result() {
        let before = sas_doc(&[scsi_entry(0, "Completed", 30000)]);
        let baseline = log_of(&before);
        let late = sas_doc(&[
            scsi_entry(15, "Self test in progress ...", 32392),
            scsi_entry(0, "Completed", 30000),
        ]);
        assert_eq!(classify_self_test_poll(&late, &baseline), SelfTestPoll::Running(None));

        let done = sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 30000),
        ]);
        assert!(matches!(
            classify_self_test_poll(&done, &baseline),
            SelfTestPoll::Finished(Some(_))
        ));
    }

    /// NVMe keeps its own table and writes an entry only once a run is over, so
    /// the log-advance reading applies unchanged: the earlier run's entry is not
    /// this run's result, and the new entry is.
    #[test]
    fn the_nvme_path_reads_its_own_table_unchanged() {
        let nvme = |rows: Value| serde_json::json!({"nvme_self_test_log": {"table": rows}});
        let entry = |result: u64, string: &str, hours: u64| {
            serde_json::json!({
                "self_test_code": {"string": "Extended"},
                "self_test_result": {"value": result, "string": string},
                "power_on_hours": hours,
            })
        };
        let before = nvme(serde_json::json!([entry(0, "Completed without error", 100)]));
        let baseline = log_of(&before);
        assert_eq!(baseline.len(), 1);
        assert_eq!(baseline[0].status, "passed");
        // Nothing is written while the run is going, so the earlier entry stays
        // the earlier entry.
        assert_eq!(classify_self_test_poll(&before, &baseline), SelfTestPoll::Stalled);

        let done = nvme(serde_json::json!([
            entry(7, "Completed: unknown failure", 130),
            entry(0, "Completed without error", 100),
        ]));
        match classify_self_test_poll(&done, &baseline) {
            SelfTestPoll::Finished(Some(t)) => {
                assert_eq!(t.status, "failed");
                assert_eq!(t.lifetime_hours, 130);
            }
            other => panic!("the new NVMe entry is this run's result, got {other:?}"),
        }
    }

    // ----- one test per clause of `self_test_log_advanced` -------------------
    //
    // A mutation run showed every clause of the previous version could be
    // deleted with the whole suite still green: the clauses were mutually
    // redundant, so the helper that decides "is this result ours?" was in
    // practice unpinned. Each test below is the sole killer of exactly one
    // clause — deleting that clause alone must turn that test red.

    /// Clause 1 — no baseline means no attribution. Deleting the
    /// `before.is_empty()` guard makes this read the newest entry as a result.
    #[test]
    fn clause_no_baseline_refuses_to_attribute_anything() {
        let one = log_of(&sas_doc(&[scsi_entry(0, "Completed", 32392)]));
        assert!(!self_test_log_advanced(&one, &[]));
    }

    /// Clause 2 — a log that reads SHORTER than the baseline is a truncated
    /// read, not movement. Deleting the length guard makes the shrunken log
    /// differ from the baseline and so count as this run's result.
    #[test]
    fn clause_a_shorter_log_is_a_short_read_not_movement() {
        let before = log_of(&sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 30000),
        ]));
        let truncated = log_of(&sas_doc(&[scsi_entry(0, "Completed", 32392)]));
        assert!(truncated.len() < before.len());
        assert!(!self_test_log_advanced(&truncated, &before));
    }

    /// Clause 3 — an unchanged log has not moved. Replacing the final
    /// comparison with `true` makes every poll a finished run.
    #[test]
    fn clause_an_unchanged_log_has_not_moved() {
        let same = log_of(&sas_doc(&[scsi_entry(0, "Completed", 32392)]));
        assert!(!self_test_log_advanced(&same, &same));
    }

    /// The positive direction, so the three refusals above cannot be satisfied
    /// by a helper that simply always returns false.
    #[test]
    fn a_grown_log_really_does_count_as_movement() {
        let before = log_of(&sas_doc(&[scsi_entry(0, "Completed", 30000)]));
        let grown = log_of(&sas_doc(&[
            scsi_entry(0, "Completed", 32392),
            scsi_entry(0, "Completed", 30000),
        ]));
        assert!(self_test_log_advanced(&grown, &before));
    }

    /// Wave 9b: one SMART job over several disks, through a real `spawn`
    /// and the real broker. The node has no privilege channel configured, so
    /// the first disk that reaches the broker meets the real
    /// `BrokerError::Unarmed` — and the job stops THERE: that disk's line
    /// says so, the disk after it is never started ('skipped'), and the job
    /// fails with that one error. A per-disk refusal BEFORE it (a disk that
    /// left the inventory) is its own line, and the job went on past it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_smart_batch_stops_at_the_first_privilege_error_and_goes_on_past_a_disk_refusal() {
        // The catalog resolves `smartctl` before any channel is chosen; a host
        // without it answers ToolMissing first and has nothing to prove here.
        if let Err(tentanas_helper::CatalogError::ToolMissing(_)) =
            (HelperCommand::SmartctlInfo { device: "/dev/sda".into() }).plan()
        {
            eprintln!("smartctl is not installed on this host — skipped");
            return;
        }
        let db = database();
        for (id, name) in [("sn-wave9b-batch-a", "sdwa"), ("sn-wave9b-batch-b", "sdwb")] {
            super::super::disks::insert_live_for_test(tentaflow_protocol::tentanas::NasDisk {
                disk_id: id.into(),
                name: name.into(),
                path: format!("/dev/{name}"),
                ..Default::default()
            });
        }
        let disks = vec![
            ("sn-wave9b-gone".to_string(), "sdwg".to_string()),
            ("sn-wave9b-batch-a".to_string(), "sdwa".to_string()),
            ("sn-wave9b-batch-b".to_string(), "sdwb".to_string()),
        ];
        let job = spawn_smart_batch(&db, "sdwg, sdwa, sdwb", "test", &disks, |h| {
            smart_self_test_batch(h, SelfTestKind::Short, None)
        })
        .unwrap();
        let done = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let job = store::job(&db, &job.job_id).unwrap().unwrap();
                if job.finished_at.is_some() { return job; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("the batch finished");
        let lines = store::job_disks(&db, &job.job_id).unwrap();
        let shown: Vec<(&str, &str, &str)> = lines
            .iter()
            .map(|l| (l.name.as_str(), l.state.as_str(), l.reasons.first().map(|r| r.code.as_str()).unwrap_or("")))
            .collect();
        assert_eq!(shown, vec![
            ("sdwg", "refused", "disk_gone"),
            ("sdwa", "refused", "privilege"),
            ("sdwb", "skipped", ""),
        ]);
        assert_eq!(done.status, "failed");
        assert!(done.error.as_deref().unwrap_or("").contains("privilege channel not available"), "{:?}", done.error);
        assert!(done.log.iter().any(|l| l.contains("the remaining disks are not started")), "{:?}", done.log);
        for id in ["sn-wave9b-batch-a", "sn-wave9b-batch-b"] {
            super::super::disks::remove_live_for_test(id);
        }
    }
}

// ----- job bodies ----------------------------------------------------------------

/// Mode A provisioning: stage the sudoers line, run the plan's commands with
/// the one-shot password, verify the chain end-to-end, record the mode.
pub async fn provision_helper(
    h: JobHandle,
    token: Arc<ElevationToken>,
    staging_dir: std::path::PathBuf,
    admin: String,
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
    // The binary on disk has just changed, so the cached version probe is
    // stale: drop it before the verification reads it again, and so that the
    // next Elastic operation is admitted without waiting out the TTL.
    super::broker::forget_helper_version();
    let status = super::elevation::helper_status().await;
    h.log(format!("helper state after provisioning: {}", status.state));
    if status.state != "ok" {
        return Err(anyhow!("helper verification failed: {}", status.state));
    }
    super::elevation::set_mode(h.db(), super::elevation::Mode::Helper)?;
    // Only now: a provisioning that did not verify has nobody to attribute.
    super::elevation::record_provisioning(h.db(), &admin)?;
    if admin.is_empty() {
        h.log("provisioned");
    } else {
        h.log(format!("provisioned by {admin}"));
    }
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
    // The binary is gone: the cached probe would otherwise still vouch for it
    // for up to its TTL.
    super::broker::forget_helper_version();
    if super::elevation::mode(h.db()) == super::elevation::Mode::Helper {
        super::elevation::set_mode(h.db(), super::elevation::Mode::Unset)?;
    }
    super::elevation::clear_provisioning(h.db())?;
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

/// What one poll of the SMART document says about a self-test this job started.
#[derive(Debug, PartialEq)]
enum SelfTestPoll {
    /// The disk itself says the run is going. `Some` is its own progress
    /// percentage, which only ATA reports.
    Running(Option<u8>),
    /// Nothing in the document belongs to this run yet: the self-test log still
    /// looks exactly as it did before the test was started.
    Stalled,
    /// The newest log entry is this run's result. `None` when the disk keeps no
    /// readable log and the run can only be reported as finished without one.
    Finished(Option<NasSmartSelfTest>),
}

/// Whether the self-test log has taken on an entry since `before`.
///
/// A SCSI log carries nothing that identifies a run — no serial, no start time,
/// just numbered keys, newest first — so the only honest way to tell this job's
/// result from one an earlier test left behind is that the log MOVED.
///
/// Three independent reasons to say it did not, each of which stands alone:
fn self_test_log_advanced(now: &[NasSmartSelfTest], before: &[NasSmartSelfTest]) -> bool {
    // 1. No baseline: the pre-start read failed, so there is nothing to compare
    //    against and NOTHING can be attributed to this run. Reading the newest
    //    entry anyway (the old fallback) let a failure left by a previous test
    //    fail a fresh run on its first poll — and, just as bad, let a stale
    //    pass report a green job for a test that had barely started. Both
    //    directions are refused; the run stalls to the window instead, and the
    //    disk's real verdict still reaches the detail view through the periodic
    //    `refresh_smart`.
    if before.is_empty() {
        return false;
    }
    // 2. The log reads SHORTER than the baseline. Log entries do not disappear,
    //    so this is a truncated or partial read, not movement.
    if now.len() < before.len() {
        return false;
    }
    // 3. The log is byte-for-byte what it was. An append pushes everything one
    //    place down, so a real new entry always changes this comparison — the
    //    log either grew or, when it is full at 20, dropped its oldest entry.
    //    Comparing only the top two entries (the old check) called a log whose
    //    two newest entries were ALREADY identical "moved" on every poll, which
    //    finished a 24-hour test on its first poll all over again.
    //
    //    A FULL log of 20 identical entries is the one case with no observable
    //    difference at all: it reads as "not advanced", so the run stalls to
    //    the window and errors rather than inventing a result never shown.
    now != before
}

/// How long the poll loop waits while the disk shows no sign of the run at all.
/// The disk's own advertised duration is the only honest basis, doubled because
/// a self-test runs at background priority, and floored so a disk that
/// advertises nothing still gets a sane window.
///
/// This is the STALL bound only, and it is rolling: a `Running` poll pushes it
/// back. It used to be described as an absolute deadline measured from the
/// start, which was true only for a disk that reports no progress whatsoever
/// (a SAS disk that never writes an in-progress entry). For every disk that
/// does report progress the claim was false, and a stale in-progress entry
/// therefore reset it on every poll and the loop never terminated at all. The
/// absolute bound now lives in `self_test_timeout`, which is what actually
/// guarantees termination; the doubling still matters here because it has to
/// cover a whole run in one go for a disk that stays silent throughout.
///
/// FAILED PRE-READ — when the SMART read before the start fails, this is
/// handed a `Value::Null` and there is no advertised duration in it, so the
/// result is the FLOOR (6 h for a long test), not the disk's real duration.
/// The comment here used to imply the disk's own figure still applied. It does
/// not: the duration is read from the very document that failed to load.
/// `rederived_self_test_window` recovers it from a later successful poll where
/// doing so can still help.
///
/// LIMITATION — NVMe advertises NO self-test duration in smartctl's JSON at
/// all, so an NVMe test falls to the floor. This is not a matter of reading a
/// different key: smartctl parses the NVMe Identify Controller EDSTT field
/// (Extended Device Self-test Time, minutes) into `nvme_id_ctrl::edstt` in
/// `nvmecmds.h` and then never prints it — `edstt` appears nowhere in
/// `nvmeprint.cpp`, in no version through current master, and the string
/// `edstt` is absent from the smartctl 7.5 (r5714) binary's own string table,
/// which is the complete inventory of keys it can emit. The only
/// `*_extended_self_test_seconds` key smartctl has is the SCSI one, read
/// above. So no key is guessed at here. What made the floor harmful — an NVMe
/// test being errored as STALLED while it ran — is fixed instead by reading
/// the progress NVMe does report (`summarize_smart`, the self-test log header),
/// which keeps the stall bound pushed back for as long as the test really runs.
fn self_test_window(doc: &Value, kind: SelfTestKind) -> Duration {
    let advertised = match kind {
        SelfTestKind::Long => doc
            .get("scsi_extended_self_test_seconds")
            .and_then(Value::as_u64)
            .or_else(|| {
                doc.pointer("/ata_smart_data/self_test/polling_minutes/extended")
                    .and_then(Value::as_u64)
                    .map(|m| m * 60)
            }),
        SelfTestKind::Short => doc
            .pointer("/ata_smart_data/self_test/polling_minutes/short")
            .and_then(Value::as_u64)
            .map(|m| m * 60),
    };
    let floor = match kind {
        SelfTestKind::Short => 15 * 60,
        SelfTestKind::Long => 6 * 60 * 60,
    };
    Duration::from_secs(advertised.unwrap_or(0).saturating_mul(2).max(floor))
}

/// Recovers the window from a POLLED document when the pre-start read did not
/// carry the disk's advertised duration.
///
/// The advertised duration is static device metadata (ATA IDENTIFY, the SCSI
/// mode page), not run state, so a poll is exactly as trustworthy a source for
/// it as the pre-read — and when the pre-read failed it is the ONLY source.
/// Without this, a failed pre-read silently bounded a long test by the 6 h
/// floor while the disk went on testing for 18 h and the job had already
/// errored. Returns `None` when there is nothing to gain; the caller takes the
/// value at most once, so the window can only ever hold one of two values and
/// the hard cap derived from it stays finite.
fn rederived_self_test_window(
    window: Duration,
    doc: &Value,
    kind: SelfTestKind,
    baseline: &[NasSmartSelfTest],
) -> Option<Duration> {
    // Without a baseline only ATA can still report this run's end: its status
    // block is authoritative on its own, while every other transport needs the
    // log to have MOVED against a baseline this run does not have. Stretching
    // the window of a run whose result can never be attributed does not save
    // it — it only makes the same unavoidable error arrive hours later, with
    // the job row sitting on "running" the whole time.
    if baseline.is_empty() && doc.pointer("/ata_smart_data/self_test/status").is_none() {
        return None;
    }
    let fresh = self_test_window(doc, kind);
    // Only ever grows: a poll that happens to carry less than the pre-read did
    // must not shorten a window already in force.
    (fresh > window).then_some(fresh)
}

/// Which of the poll loop's two bounds ran out.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SelfTestTimeout {
    /// No poll ever showed a sign of this run, for one whole window.
    Stall,
    /// The disk kept claiming the run was going, past the absolute cap.
    Cap,
}

impl SelfTestTimeout {
    /// The two are deliberately different sentences: a stall means the disk
    /// never showed the run at all (a start that silently did nothing, a log
    /// that never moves), while the cap means the disk kept saying it was
    /// working and was no longer believed. An operator chasing one must not be
    /// handed the words for the other.
    fn error(self, bound: Duration) -> anyhow::Error {
        let min = bound.as_secs() / 60;
        match self {
            Self::Stall => anyhow!("the disk recorded no self-test result within {min} min"),
            Self::Cap => anyhow!(
                "the disk still reported the test as running after {min} min, twice the window it was given: its progress reports are no longer believed"
            ),
        }
    }
}

/// The poll loop is bounded TWICE, and both bounds are needed.
///
/// `since_sign` is the time since the last poll that showed any sign of the
/// run, and the rolling stall bound is what catches a start that did nothing.
/// It cannot be the only bound, because a disk with a stale "in progress"
/// entry — an interrupted test, a power loss — reports `Running` on every
/// single poll, resets it every time, and the loop then never terminates at
/// all. So `elapsed` since the start is bounded absolutely, independent of any
/// reset.
///
/// The cap is twice the window, i.e. four times the duration the disk itself
/// advertised. The window is already a doubling, sized to cover one whole run
/// at background priority; a run that has been going for two of them is not
/// merely slow. Doubling is also the smallest multiple that leaves the stall
/// bound strictly INSIDE the cap, so adding the cap cannot cut short any run
/// that the stall bound would have let finish within its first window — the
/// cap only fires for runs that previously never terminated.
fn self_test_timeout(
    window: Duration,
    elapsed: Duration,
    since_sign: Duration,
) -> Option<(SelfTestTimeout, Duration)> {
    if since_sign >= window {
        return Some((SelfTestTimeout::Stall, window));
    }
    let cap = window.saturating_mul(2);
    if elapsed >= cap {
        return Some((SelfTestTimeout::Cap, cap));
    }
    None
}

/// One poll, including the case where the document could not be read at all.
///
/// A failed read is NO SIGN of the run, so it is classified as a stall rather
/// than waved through. The loop used to `continue` past a failed read, which
/// skipped the bounds entirely: a device that had gone away, or a credential
/// that stopped working, failed every read and the job then polled forever —
/// the same unbounded loop as a stale in-progress entry, by a different route.
fn classify_self_test_read(doc: Option<&Value>, baseline: &[NasSmartSelfTest]) -> SelfTestPoll {
    match doc {
        Some(doc) => classify_self_test_poll(doc, baseline),
        None => SelfTestPoll::Stalled,
    }
}

/// What the poll loop does with one polled document.
#[derive(Debug, PartialEq)]
enum SelfTestStep {
    /// Keep polling, and report this progress percentage if the disk gave one.
    Progress(Option<u8>),
    /// Keep polling; the poll said nothing about this run.
    Wait,
    /// The run produced this result.
    Done(Option<NasSmartSelfTest>),
    /// One of the two bounds ran out; the job ends with this error.
    Expired(SelfTestTimeout, Duration),
}

/// The whole decision the loop makes, kept out of the loop so it can be tested
/// without a disk, a broker or a clock.
///
/// `elapsed` is measured from the start of the run and `since_sign` from the
/// last poll that showed a sign of it. The bound is consulted for EVERY poll
/// including a `Running` one: that is the fix. A `Running` poll used to be the
/// one case that only ever pushed the deadline back and never tested it, so a
/// disk holding a stale in-progress entry kept the loop alive indefinitely.
fn self_test_step(
    poll: SelfTestPoll,
    window: Duration,
    elapsed: Duration,
    since_sign: Duration,
) -> SelfTestStep {
    // A `Running` poll IS the sign, so it resets the rolling half here rather
    // than leaving the caller to remember to.
    let since_sign = match poll {
        SelfTestPoll::Running(_) => Duration::ZERO,
        _ => since_sign,
    };
    // A result that did arrive is always reported as a result, never as a
    // timeout, however late it is.
    if let SelfTestPoll::Finished(latest) = poll {
        return SelfTestStep::Done(latest);
    }
    if let Some((timeout, bound)) = self_test_timeout(window, elapsed, since_sign) {
        return SelfTestStep::Expired(timeout, bound);
    }
    match poll {
        SelfTestPoll::Running(pct) => SelfTestStep::Progress(pct),
        _ => SelfTestStep::Wait,
    }
}

/// Turns the log entry this run produced into the job's outcome: `Ok` carries
/// the line to log, `Err` ends the job.
///
/// A self-test answers TWO questions, and they must not be merged into one
/// status string: *is the disk faulty?* and *did this run complete?* The job
/// row answers only the second — did this run finish, and pass. Treating
/// "not failed" as "succeeded" gave the admin a green "succeeded" job for a
/// 24.5-hour test that was aborted at hour 20 (SPC 1, the user's own command,
/// or SPC 2, a device reset), for SPC 3, whose own smartctl text reads
/// "unknown error, incomplete", and for the reserved 8–14 that mean nothing at
/// all. Only a passed entry ends the job green; everything else carries the
/// disk's own words, worded so "the disk failed the test" and "the test never
/// finished" cannot be mistaken for one another.
///
/// This deliberately does NOT mirror `score_health`, which calls a disk
/// critical only on SPC 4–7. That asymmetry is the point: a run that did not
/// complete is a failed *job* but is no evidence at all of a faulty *disk*.
fn self_test_outcome(latest: Option<&NasSmartSelfTest>) -> Result<String> {
    let Some(t) = latest else {
        return Err(anyhow!(
            "self-test gave no usable result: the disk recorded no self-test entry at all"
        ));
    };
    match t.status.as_str() {
        "passed" => Ok(format!("result: {}", t.detail)),
        // SPC 4–7: the disk took the test and failed it. The only status here
        // that is a verdict on the hardware.
        "failed" => Err(anyhow!("self-test failed: {}", t.detail)),
        // SPC 1/2: the run was stopped. Which of the two it was is in the
        // detail string, and that is the only place it survives.
        "aborted" => Err(anyhow!("self-test did not complete: {}", t.detail)),
        // An in-progress entry read as a verdict: the disk itself still calls
        // the run unfinished.
        "running" => Err(anyhow!(
            "self-test did not complete: the disk still reports it as running ({})",
            t.detail
        )),
        // SPC 3, the reserved 8–14, and an entry with no result code at all.
        // Nothing here supports either answer, so the raw string the disk gave
        // is the whole report — never a green tick.
        _ => Err(anyhow!(
            "self-test gave no usable result: the disk reported {}, which is neither a pass nor a disk failure",
            if t.detail.is_empty() {
                "no result code at all".to_string()
            } else {
                format!("\"{}\"", t.detail)
            }
        )),
    }
}

/// Reads one polled document as a verdict on the test this job started.
/// `baseline` is the self-test log as it stood BEFORE the start.
fn classify_self_test_poll(doc: &Value, baseline: &[NasSmartSelfTest]) -> SelfTestPoll {
    // ATA is the only family that reports its own progress, and its status
    // block is authoritative in BOTH directions: a remaining percentage means
    // the run is going, and the block without one means it is over. That
    // reading is unchanged — everything below exists for the families that
    // have no such signal.
    if let Some(pct) = super::disks::summarize_smart(doc).self_test_running_pct {
        return SelfTestPoll::Running(Some(pct));
    }
    let entries = super::disks::smart_self_tests(doc);
    if doc.pointer("/ata_smart_data/self_test/status").is_some() {
        return SelfTestPoll::Finished(entries.into_iter().next());
    }
    match entries.first() {
        // SCSI has no progress field at all: it reports a running test as an
        // in-progress entry in the log itself (SPC result code 15).
        Some(t) if t.status == "running" => SelfTestPoll::Running(None),
        Some(t) if self_test_log_advanced(&entries, baseline) => {
            SelfTestPoll::Finished(Some(t.clone()))
        }
        // The newest entry is the one that was already there before the start,
        // so it belongs to an earlier test and says nothing about this run.
        _ => SelfTestPoll::Stalled,
    }
}

/// A self-test the disk accepted: what the poll loop needs to follow it.
struct StartedSelfTest {
    /// The self-test log as it stood BEFORE the start.
    baseline: Vec<NasSmartSelfTest>,
    window: Duration,
    channel: &'static str,
}

/// Why a self-test did not start. `Privilege` is the one shape that stops a
/// multi-disk job at once: the credential or the channel is what failed, so
/// it fails identically for every disk still to come, and replaying a
/// rejected password once per disk can lock the account (`pam_faillock`).
enum StartError {
    Privilege(anyhow::Error),
    Other(anyhow::Error),
}

impl StartError {
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Privilege(e) | Self::Other(e) => e,
        }
    }
}

/// A broker refusal that is about the credential or the privilege channel,
/// not about the disk: a rejected or expired password, an unarmed channel, a
/// helper of another build than this core.
fn privilege_error(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<super::broker::BrokerError>(),
        Some(super::broker::BrokerError::Unarmed(_) | super::broker::BrokerError::HelperVersion(_))
    )
}

/// Reads the self-test log and starts the test.
///
/// The self-test log carries nothing that identifies a run, so what it held
/// BEFORE the start is the only way to tell this run's entry from one an
/// earlier test left behind. It is read with the same credential the start
/// uses, and it happens before the start, so a failure here cannot stop the
/// test. An unreadable baseline no longer degrades to "the newest entry is
/// this run's": with nothing to compare against, no entry is attributed to
/// this run at all, and the job ends at the window rather than repeating a
/// previous test's verdict as if it were this one's. The one exception is a
/// PRIVILEGE failure of that read: the start would meet the same credential,
/// so it is not attempted.
async fn start_self_test(
    db: &DbPool,
    device: &str,
    kind: SelfTestKind,
    explicit: Option<&ElevationToken>,
    log: &(dyn Fn(String) + Sync),
) -> std::result::Result<StartedSelfTest, StartError> {
    let before = match super::disks::read_smart_document(db, device, explicit).await {
        Ok(doc) => doc,
        Err(e) if privilege_error(&e) => return Err(StartError::Privilege(e)),
        Err(e) => {
            log(format!("could not read the self-test log before the start: {e}"));
            Value::Null
        }
    };
    let baseline = super::disks::smart_self_tests(&before);
    let window = self_test_window(&before, kind);
    let start = HelperCommand::SmartctlSelfTest {
        device: device.to_string(),
        kind,
    };
    let (out, channel) = match super::broker::run_privileged(db, &start, explicit, Duration::from_secs(60)).await {
        Ok(ran) => ran,
        Err(e) => {
            let e = anyhow::Error::from(e);
            return Err(if privilege_error(&e) { StartError::Privilege(e) } else { StartError::Other(e) });
        }
    };
    // Bit 2 stays in the mask here, unlike the document read: this run issues a
    // command instead of printing a report, so "a SMART command failed" means
    // the test did not start. The helper starts the test with `--json=c`, so
    // stderr is empty by design and the reason has to come out of the document.
    if out.code & 0b111 != 0 {
        return Err(StartError::Other(anyhow!(
            "smartctl could not start the test ({}): {}",
            out.code,
            super::disks::smartctl_failure_detail(&out)
        )));
    }
    Ok(StartedSelfTest { baseline, window, channel: channel.as_str() })
}

/// How following a started self-test ended.
enum Followed {
    /// The run produced this entry (or none that could be attributed).
    Done(Option<NasSmartSelfTest>),
    /// One of the two bounds ran out.
    Expired(SelfTestTimeout, Duration),
    Cancelled,
}

/// Follows a started self-test through `smartctl` polls until the disk
/// reports completion or a bound runs out. Progress is what the disk reports.
///
/// Polling uses the node's channel: the one-shot password was consumed by the
/// start. An interactive node whose TTL expires mid-test leaves the job
/// "running" until the next arm — the disk keeps testing.
async fn follow_self_test(
    h: &JobHandle,
    device: &str,
    kind: SelfTestKind,
    started: StartedSelfTest,
    log: &(dyn Fn(String) + Sync),
    progress: &(dyn Fn(u8) + Sync),
) -> Followed {
    let StartedSelfTest { baseline, mut window, .. } = started;
    let poll = Duration::from_secs(if kind == SelfTestKind::Short { 20 } else { 120 });
    // `started` is fixed for the whole run and nothing below may touch it: it
    // is what bounds the loop absolutely. `last_sign` is the rolling half, and
    // a `Running` poll pushes only that one back.
    let started = tokio::time::Instant::now();
    let mut last_sign = started;
    // The window may still grow once, from the first poll that carries the
    // advertised duration the pre-read did not (see `rederived_self_test_window`).
    let mut window_rederived = false;
    loop {
        tokio::time::sleep(poll).await;
        if h.cancelled() {
            return Followed::Cancelled;
        }
        // A read that failed is kept as `None` rather than `continue`d past:
        // every path out of this loop has to reach the bound check below.
        let doc = match super::disks::read_smart_document(h.db(), device, None).await {
            Ok(d) => Some(d),
            Err(e) => {
                log(format!("poll failed: {e}"));
                None
            }
        };
        if let Some(doc) = doc.as_ref() {
            if !window_rederived {
                if let Some(w) = rederived_self_test_window(window, doc, kind, &baseline) {
                    log(format!(
                        "the disk advertises a longer duration than the pre-start read gave: window {} min -> {} min",
                        window.as_secs() / 60,
                        w.as_secs() / 60
                    ));
                    window = w;
                    window_rederived = true;
                }
            }
        }
        let polled = classify_self_test_read(doc.as_ref(), &baseline);
        match self_test_step(polled, window, started.elapsed(), last_sign.elapsed()) {
            SelfTestStep::Progress(pct) => {
                if let Some(pct) = pct {
                    progress(pct);
                }
                last_sign = tokio::time::Instant::now();
            }
            SelfTestStep::Wait => {}
            SelfTestStep::Done(latest) => return Followed::Done(latest),
            SelfTestStep::Expired(timeout, bound) => return Followed::Expired(timeout, bound),
        }
    }
}

/// Starts a SMART self-test and follows it through `smartctl` polls until
/// the disk reports completion. Progress is what the disk reports.
pub async fn smart_self_test(
    h: JobHandle,
    device: String,
    kind: SelfTestKind,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<()> {
    let log = |line: String| h.log(line);
    let started = start_self_test(h.db(), &device, kind, explicit.as_deref(), &log)
        .await
        .map_err(StartError::into_error)?;
    h.log(format!("self-test started via {}", started.channel));
    // The one-shot password is consumed by the start; polling uses the
    // node's channel.
    drop(explicit);
    let progress = |pct: u8| h.progress(pct);
    match follow_self_test(&h, &device, kind, started, &log, &progress).await {
        Followed::Done(latest) => {
            super::disks::request_smart_refresh();
            h.log(self_test_outcome(latest.as_ref())?);
            Ok(())
        }
        Followed::Expired(timeout, bound) => {
            super::disks::request_smart_refresh();
            Err(timeout.error(bound))
        }
        Followed::Cancelled => Err(anyhow!("cancelled")),
    }
}

/// A finished line of a multi-disk job: its state and the code that says
/// why, from the entry the run produced. The same two questions as
/// `self_test_outcome` — did the disk fail the test, did the run complete —
/// kept apart: 'failed' is the disk's verdict, 'incomplete' is a run that
/// says nothing about the disk.
fn line_verdict(latest: Option<&NasSmartSelfTest>) -> (&'static str, Vec<NasHealthReason>) {
    let reason = |code: &str, detail: &str| {
        vec![super::disks::coded_reason(code, &[("detail", detail.to_string())])]
    };
    match latest {
        None => ("incomplete", reason("test_no_entry", "")),
        Some(t) => match t.status.as_str() {
            "passed" => ("passed", Vec::new()),
            "failed" => ("failed", reason("test_failed", &t.detail)),
            "aborted" => ("incomplete", reason("test_aborted", &t.detail)),
            "running" => ("incomplete", reason("test_still_running", &t.detail)),
            _ => ("incomplete", reason("test_no_result", &t.detail)),
        },
    }
}

/// The line of a run one of the two bounds ended.
fn expired_reason(timeout: SelfTestTimeout, bound: Duration) -> NasHealthReason {
    let min = (bound.as_secs() / 60).to_string();
    match timeout {
        SelfTestTimeout::Stall => super::disks::coded_reason("test_stalled", &[("min", min)]),
        SelfTestTimeout::Cap => super::disks::coded_reason("test_overran", &[("min", min)]),
    }
}

/// One SMART self-test job over several disks (`store::SMART_BATCH_KIND`).
///
/// The disks are STARTED one after another with the one credential the
/// request carried, and the first privilege/credential error stops the
/// starting there: that disk's line says why, every disk after it is
/// 'skipped', and the job fails with that one error — the password is never
/// replayed against sudo once per disk. Any other refusal (a disk that left,
/// a disk that rejects the command) is recorded on its own line and the job
/// goes on. The started tests then run side by side — each drive tests in its
/// own firmware — and are followed together; the job succeeds only when
/// every line passed.
pub async fn smart_self_test_batch(
    h: JobHandle,
    kind: SelfTestKind,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<()> {
    let lines = store::job_disks(h.db(), &h.job_id)?;
    let set = |line: &store::JobDiskRow, state: &str, pct: Option<u8>, reasons: &[NasHealthReason]| {
        if let Err(e) = store::set_job_disk(h.db(), &h.job_id, line.position, state, pct, reasons) {
            tracing::warn!("tentanas job {}: disk line write failed: {e}", h.job_id);
        }
    };
    h.log(format!(
        "{} self-test on {} disks: {}",
        match kind {
            SelfTestKind::Short => "short",
            SelfTestKind::Long => "long",
        },
        lines.len(),
        lines.iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(", ")
    ));
    for line in lines.iter().filter(|l| l.state == "refused") {
        h.log(format!("{}: a self-test already runs on this disk", line.name));
    }
    let mut halted: Option<anyhow::Error> = None;
    let mut started = Vec::new();
    for line in lines.iter().filter(|l| l.state == "pending") {
        if halted.is_some() || h.cancelled() {
            set(line, "skipped", None, &[]);
            continue;
        }
        let Some(device) = super::disks::device_path(&line.disk_id) else {
            h.log(format!("{}: the disk is no longer in this node's inventory", line.name));
            set(line, "refused", None, &[super::disks::coded_reason("disk_gone", &[])]);
            continue;
        };
        let name = line.name.clone();
        let log = |text: String| h.log(format!("{name}: {text}"));
        match start_self_test(h.db(), &device, kind, explicit.as_deref(), &log).await {
            Ok(run) => {
                h.log(format!("{}: self-test started via {}", line.name, run.channel));
                set(line, "running", None, &[]);
                started.push((line.clone(), device, run));
            }
            Err(StartError::Privilege(e)) => {
                h.log(format!("{}: {e} — the remaining disks are not started", line.name));
                set(line, "refused", None, &[super::disks::coded_reason("privilege", &[])]);
                halted = Some(e);
            }
            Err(StartError::Other(e)) => {
                h.log(format!("{}: {e}", line.name));
                set(line, "refused", None, &[super::disks::coded_reason("start_failed", &[])]);
            }
        }
    }
    // The one-shot password was for the starts; polling uses the channel.
    drop(explicit);

    let total = started.len().max(1) as u32;
    let percents: Arc<Mutex<HashMap<i64, u8>>> = Arc::default();
    let report = |position: i64, pct: u8| {
        let mut map = percents.lock().unwrap_or_else(|p| p.into_inner());
        map.insert(position, pct);
        let sum: u32 = map.values().map(|p| u32::from(*p)).sum();
        h.progress((sum / total).min(100) as u8);
    };
    let outcomes = futures::future::join_all(started.into_iter().map(|(line, device, run)| {
        let h = &h;
        let set = &set;
        let report = &report;
        async move {
            let name = line.name.clone();
            let log = |text: String| h.log(format!("{name}: {text}"));
            let progress = |pct: u8| {
                set(&line, "running", Some(pct), &[]);
                report(line.position, pct);
            };
            let (state, reasons) = match follow_self_test(h, &device, kind, run, &log, &progress).await {
                Followed::Done(latest) => {
                    if let Ok(text) = self_test_outcome(latest.as_ref()) {
                        log(text);
                    }
                    line_verdict(latest.as_ref())
                }
                Followed::Expired(timeout, bound) => {
                    log(timeout.error(bound).to_string());
                    ("incomplete", vec![expired_reason(timeout, bound)])
                }
                Followed::Cancelled => ("cancelled", Vec::new()),
            };
            set(&line, state, None, &reasons);
            report(line.position, 100);
            state
        }
    }))
    .await;
    super::disks::request_smart_refresh();

    if let Some(e) = halted {
        return Err(e);
    }
    if outcomes.iter().any(|s| *s == "cancelled") {
        return Err(anyhow!("cancelled"));
    }
    let lines = store::job_disks(h.db(), &h.job_id)?;
    let not_passed = lines.iter().filter(|l| l.state != "passed").count();
    if not_passed > 0 {
        return Err(anyhow!(
            "the self-test did not pass on {not_passed} of {} disks",
            lines.len()
        ));
    }
    Ok(())
}
