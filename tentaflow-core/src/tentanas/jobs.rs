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
use tentaflow_protocol::tentanas::{NasJob, NasSmartSelfTest};
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
    Snapraid { owner: ElasticOwner, array_id: String, operation_id: String, kind: ElasticSnapraidKind },
    /// One mover run. `resume_operation_id` is reserved up front because the
    /// helper needs a SEPARATE operation to close a run that stopped
    /// part-way, and `validate_observation` refuses a resume that reuses the
    /// run's own operation or the array's create.
    /// `rules` and `coupled_sync` travel WITH the intent because they are
    /// derived from the array row (its mover settings and its folders' cache
    /// policies) and cannot be rebuilt from the persisted `ElasticCreateSpec`
    /// alone, which is all the store has. The row therefore records the exact
    /// command the body will run.
    Mover { owner: ElasticOwner, array_id: String, operation_id: String, resume_operation_id: String,
        rules: tentanas_helper::elastic::MoverRules, coupled_sync: bool },
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
            },
            None,
        );
        Self {
            db: db.clone(),
            job_id: job_id.to_string(),
            cancel: CancellationToken::new(),
        }
    }

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
        | HelperCommand::ElasticInspect { .. } | HelperCommand::ElasticClaims { .. }
        | HelperCommand::ElasticSync { .. } | HelperCommand::ElasticScrub { .. }
        | HelperCommand::ElasticEnterService { .. } | HelperCommand::ElasticResume { .. }
        | HelperCommand::ElasticMover { .. } => "Elastic Array",
    }
}

/// Creates the row and spawns `body`. The returned job is the row as
/// queued; callers answer with it and the UI polls.
pub fn spawn<F, Fut>(db: &DbPool, kind: &str, subject: &str, started_by: &str,
    intent: Option<ElasticJobIntent>, completion: Option<tokio::sync::oneshot::Sender<Result<()>>>,
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
    };
    let cancellable = intent.is_none();
    // Maintenance closes its own operation row inside `finish_job`, from the
    // result the body recorded. The blanket `fail_elastic_job` below would put
    // the ARRAY into needs_attention for a run that merely reported a failure
    // it already persisted, so maintenance is excluded from it.
    let maintenance = matches!(
        intent,
        Some(ElasticJobIntent::Snapraid { .. } | ElasticJobIntent::Mover { .. })
    );
    let mut registry = running().lock().unwrap_or_else(|p| p.into_inner());
    store::insert_job(db, &job, intent.as_ref())?;
    let cancel = CancellationToken::new();
    registry.insert(job.job_id.clone(), RunningJob { cancel: cancel.clone(), cancellable });
    drop(registry);
    let handle = JobHandle {
        db: db.clone(),
        job_id: job.job_id.clone(),
        cancel: cancel.clone(),
    };
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
        if !cancellable && !maintenance {
            if let Err(error) = &outcome {
                if let Err(persist) = store::fail_elastic_job(&db,&job_id,&error.to_string()) {
                    tracing::error!("tentanas job {job_id}: nie utrwalono needs_attention: {persist}");
                    outcome = Err(anyhow!("{error}; nie utrwalono needs_attention: {persist}"));
                }
            }
        }
        let (status, error) = match &outcome {
            Ok(()) => ("succeeded", None),
            Err(e) if e.to_string() == "cancelled" => ("cancelled", None),
            Err(e) => ("failed", Some(e.to_string())),
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
    let status = super::elevation::helper_status().await;
    h.log(format!("helper state after provisioning: {}", status.state));
    if status.state != "ok" {
        return Err(anyhow!("helper verification failed: {}", status.state));
    }
    super::elevation::set_mode(h.db(), super::elevation::Mode::Helper)?;
    // Only now: a provisioning that did not verify has nobody to attribute.
    super::elevation::record_provisioning(h.db(), &admin)?;
    h.log(format!("provisioned by {admin}"));
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
/// Only a `Running` poll pushes the deadline back, and SCSI reports progress
/// solely as an in-progress log entry, so for a disk that never writes one this
/// is an ABSOLUTE deadline measured from the start, not a rolling stall window.
/// That is why the doubling matters: it has to cover the whole run in one go.
///
/// LIMITATION — NVMe advertises neither field read here, so an NVMe extended
/// test falls to the 6 h floor and one that genuinely runs longer is errored as
/// stalled. smartctl does report an extended self-test time for NVMe, but not
/// under either key above; wiring it up needs a captured NVMe document to name
/// the key from, and guessing at JSON key names is what this whole area is
/// recovering from. Follow-up "NVMe self-test window".
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

/// Starts a SMART self-test and follows it through `smartctl` polls until
/// the disk reports completion. Progress is what the disk reports.
pub async fn smart_self_test(
    h: JobHandle,
    device: String,
    kind: SelfTestKind,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<()> {
    // The self-test log carries nothing that identifies a run, so what it held
    // BEFORE the start is the only way to tell this run's entry from one an
    // earlier test left behind. It is read with the same credential the start
    // uses, and it happens before the start, so a failure here cannot stop the
    // test. An unreadable baseline no longer degrades to "the newest entry is
    // this run's": with nothing to compare against, no entry is attributed to
    // this run at all, and the job ends at the window rather than repeating a
    // previous test's verdict as if it were this one's.
    let before = super::disks::read_smart_document(h.db(), &device, explicit.as_deref())
        .await
        .unwrap_or_else(|e| {
            h.log(format!("could not read the self-test log before the start: {e}"));
            Value::Null
        });
    let baseline = super::disks::smart_self_tests(&before);
    let window = self_test_window(&before, kind);
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
    // Bit 2 stays in the mask here, unlike the document read: this run issues a
    // command instead of printing a report, so "a SMART command failed" means
    // the test did not start. The helper starts the test with `--json=c`, so
    // stderr is empty by design and the reason has to come out of the document.
    if out.code & 0b111 != 0 {
        return Err(anyhow!(
            "smartctl could not start the test ({}): {}",
            out.code,
            super::disks::smartctl_failure_detail(&out)
        ));
    }
    h.log(format!("self-test started via {}", channel.as_str()));
    // The one-shot password is consumed by the start; polling uses the
    // node's channel. An interactive node whose TTL expires mid-test leaves
    // the job "running" until the next arm — the disk keeps testing.
    drop(explicit);
    let poll = Duration::from_secs(if kind == SelfTestKind::Short { 20 } else { 120 });
    // Only a `Running` poll pushes this back. A SAS disk that never writes an
    // in-progress entry never produces one, so for that disk this is an
    // absolute deadline from the start — which is exactly why `self_test_window`
    // doubles the advertised duration instead of trusting a reset to arrive.
    let mut stall_deadline = tokio::time::Instant::now() + window;
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
        match classify_self_test_poll(&doc, &baseline) {
            SelfTestPoll::Running(pct) => {
                if let Some(pct) = pct {
                    h.progress(pct);
                }
                stall_deadline = tokio::time::Instant::now() + window;
            }
            SelfTestPoll::Stalled if tokio::time::Instant::now() >= stall_deadline => {
                super::disks::request_smart_refresh();
                return Err(anyhow!(
                    "the disk recorded no self-test result within {} min",
                    window.as_secs() / 60
                ));
            }
            SelfTestPoll::Stalled => {}
            SelfTestPoll::Finished(latest) => {
                super::disks::request_smart_refresh();
                h.log(self_test_outcome(latest.as_ref())?);
                return Ok(());
            }
        }
    }
}
