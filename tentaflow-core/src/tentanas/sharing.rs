// ===== File: tentanas/sharing.rs — stopping and restoring THIS node's sharing (n18d, wave 10) =====
//
// "Wyłącz i zatrzymaj udostępnianie…" (mockup n18d): a four-eyes request that
// takes every share (SMB/NFS) and every block target (iSCSI/NVMe-oF) of this
// node out of service and then disables TentaNas (fleet-wide, the same switch
// the Applications screen flips). Enabling TentaNas again brings back exactly
// what was serving.
//
// "Out of service" is a state of the NODE, not of the rows: one record in
// `tentanas.db` (`SETTING_SUSPENDED`, a `Suspension`) says sharing is stopped
// here, what was SERVING when it stopped, and where the stop or the resume
// stands (`phase`). While it stands
// - the share apply judges every enabled share that would serve `suspended`,
//   so the generated smb.conf include, the ksmbd config and /etc/exports carry
//   none of them (the daemons reload and drop them); a share that is broken
//   keeps its own error;
// - the target judgement (`targets::evaluate_rows`) judges every enabled
//   target `Remove`, so the sweep takes it out of configfs and the restore
//   loop never puts it back.
// The rows keep `enabled`, their options, their grants and their secrets.
//
// Phases (critic wave 10, B1): `stopping` while the stop job runs,
// `stopped` once TentaNas is disabled over a stopped node, `resuming` while
// the resume job runs. ONLY `stopped` with TentaNas enabled may be resumed,
// and every job of this module, and every share apply, holds one node-wide
// lock (`shares::apply_mutex`, M2): a restore tick or an enable hook that
// fires in the middle of a stop finds the phase `stopping` and the lock
// taken, and does nothing.
//
// The resume (B2) applies the shares per row: one share that does not come
// back reports its own error, by its real name in an alert of its
// organisation, and the others serve. The shares that did not come back are
// retried with a back-off, one job per interval at most.
//
// Both jobs report one LINE per step (`nas_job_disks`, keyed `step:<name>`).
// The job belongs to the organisation that asked for the stop; it names that
// organisation's shares and targets and counts every other one — the lines
// the apply and the sweeps write about individual rows go to the node log.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::{NasHealthReason, NasJob};

use super::db::{self as store, ShareRow, TargetRow};
use super::jobs::JobHandle;
use super::CodedText;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;

/// The node-level record that sharing is stopped here (`Suspension`, JSON).
pub const SETTING_SUSPENDED: &str = "sharing_suspended";
/// The shares a resume could not bring back yet (`Pending`, JSON).
pub const SETTING_RESUME_PENDING: &str = "sharing_resume_pending";
/// The job that takes sharing out of service and disables TentaNas.
pub const STOP_KIND: &str = "sharing_stop";
/// The job that puts sharing back when TentaNas is enabled again.
pub const RESUME_KIND: &str = "sharing_resume";
/// The code a suspended share's or target's state carries.
pub const SUSPENDED_CODE: &str = "sharing_suspended";

pub const PHASE_STOPPING: &str = "stopping";
pub const PHASE_STOPPED: &str = "stopped";
pub const PHASE_RESUMING: &str = "resuming";

/// The first wait after a failed resume, doubled on each failure.
const RETRY_FIRST_SECS: i64 = 60;
/// The longest wait between two resume attempts.
const RETRY_MAX_SECS: i64 = 3600;
/// How many times the shares a resume could not bring back are retried
/// before their alerts are left to the admin (the whole resume, which keeps
/// the node dark, is retried without a limit).
const PENDING_MAX_ATTEMPTS: u32 = 8;

/// One step line's key (`nas_job_disks.disk_id`). Prefixed so it can never
/// be mistaken for a disk the SMART busy check looks for.
pub const STEP_SHARES: &str = "shares";
pub const STEP_TARGETS: &str = "targets";
pub const STEP_DISABLE: &str = "disable";

/// Whether `kind` is one of this module's step jobs.
pub fn is_step_kind(kind: &str) -> bool {
    kind == STOP_KIND || kind == RESUME_KIND
}

/// The lines a step job is created with: `(key, name)` in run order.
pub fn step_lines(kind: &str) -> Vec<(String, String)> {
    let steps: &[&str] = if kind == STOP_KIND {
        &[STEP_SHARES, STEP_TARGETS, STEP_DISABLE]
    } else {
        &[STEP_SHARES, STEP_TARGETS]
    };
    steps.iter().map(|s| (format!("step:{s}"), s.to_string())).collect()
}

/// One share or target as the stop recorded it: what the resume brings back
/// and what a partial rollback names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordedRow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub org_id: String,
}

/// What the stop found SERVING (critic wave 10, B2): the enabled shares
/// judged `active` and the enabled targets in the kernel. A share that was
/// already broken is not part of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Serving {
    #[serde(default)]
    pub shares: Vec<RecordedRow>,
    #[serde(default)]
    pub targets: Vec<RecordedRow>,
}

/// The record that sharing is stopped on this node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Suspension {
    /// `stopping` | `stopped` | `resuming`.
    #[serde(default = "stopped_phase")]
    pub phase: String,
    pub since: String,
    /// The parked request that stopped sharing, and who asked for it.
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub requested_by: String,
    /// The organisation the stop was asked in: its names the jobs may say.
    #[serde(default)]
    pub org_id: String,
    #[serde(default)]
    pub serving: Serving,
    /// Failed resume attempts so far, and when the next one may run ('' =
    /// at once).
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_attempt_at: String,
}

fn stopped_phase() -> String {
    PHASE_STOPPED.to_string()
}

/// The shares a resume could not bring back, retried with a back-off.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pending {
    pub shares: Vec<RecordedRow>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub next_attempt_at: String,
}

/// The stop record, when sharing is stopped. A record that no longer parses
/// still means "stopped" (fail closed: the apply would otherwise put every
/// share back), with nothing recorded.
pub fn mark(db: &DbPool) -> Result<Option<Suspension>> {
    Ok(store::setting(db, SETTING_SUSPENDED)?.map(|value| {
        serde_json::from_str(&value).unwrap_or(Suspension {
            phase: PHASE_STOPPED.to_string(),
            since: String::new(),
            request_id: String::new(),
            requested_by: String::new(),
            org_id: String::new(),
            serving: Serving::default(),
            attempts: 0,
            next_attempt_at: String::new(),
        })
    }))
}

/// Whether sharing is stopped on this node. An unreadable database is an
/// error, never "not stopped": the apply that asks would otherwise put every
/// share back into the configs.
pub fn suspended(db: &DbPool) -> Result<bool> {
    Ok(store::setting(db, SETTING_SUSPENDED)?.is_some())
}

fn write_mark(db: &DbPool, mark: &Suspension) -> Result<()> {
    store::set_setting(db, SETTING_SUSPENDED, &serde_json::to_string(mark)?)
}

/// Lifts the record: the next apply and the next sweep serve every enabled
/// row again.
pub fn lift(db: &DbPool) -> Result<()> {
    store::delete_setting(db, SETTING_SUSPENDED)
}

fn pending(db: &DbPool) -> Result<Option<Pending>> {
    Ok(store::setting(db, SETTING_RESUME_PENDING)?.and_then(|v| serde_json::from_str(&v).ok()))
}

/// Why a stop may NOT be asked for on this node now (critic wave 10, M2 and
/// R2-2), or `None`. The node is one, so a stop parked in ANOTHER
/// organisation blocks too — said as such, never naming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// Sharing is stopped here, or a stop or resume job runs.
    Running,
    /// A stop of the asking organisation waits for its approval.
    PendingHere,
    /// A stop parked in another organisation waits for its approval.
    PendingElsewhere,
}

pub fn busy(db: &DbPool, org_id: &str) -> Result<Option<Busy>> {
    if store::setting(db, SETTING_SUSPENDED)?.is_some() {
        return Ok(Some(Busy::Running));
    }
    let conn = db.read().map_err(|e| anyhow!("tentanas db read: {e}"))?;
    let running: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM nas_jobs WHERE kind IN (?1, ?2) AND status = 'running')",
        rusqlite::params![STOP_KIND, RESUME_KIND],
        |r| r.get(0),
    )?;
    if running {
        return Ok(Some(Busy::Running));
    }
    let mut stmt = conn.prepare(
        "SELECT org_id FROM nas_pending_approvals WHERE operation = ?1 AND status = 'pending' AND expires_at > ?2",
    )?;
    let owners = stmt
        .query_map(rusqlite::params![super::approvals::OP_SHARING_STOP, store::now()], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if owners.iter().any(|o| !org_id.is_empty() && o == org_id) {
        Some(Busy::PendingHere)
    } else if !owners.is_empty() {
        Some(Busy::PendingElsewhere)
    } else {
        None
    })
}

/// After a restart no job of this module runs, so a `stopping` or `resuming`
/// record belongs to one that died with the process: it is `stopped` again,
/// and the resume brings sharing back once TentaNas is enabled (an
/// interrupted stop never completed; an interrupted resume is simply retried).
///
/// `native_init` runs again on every reconcile, not only at start, so this
/// acts only when it can take the node-wide share lock: a stop or a resume of
/// THIS process holds it for its whole run, and its record is then live.
pub fn recover_after_restart(db: &DbPool) -> Result<()> {
    let Ok(_serial) = super::shares::apply_mutex().try_lock() else {
        return Ok(());
    };
    if let Some(mut m) = mark(db)? {
        if m.phase != PHASE_STOPPED {
            tracing::warn!("tentanas: a sharing {} was interrupted by a restart; it is resumed when TentaNas is enabled", m.phase);
            m.phase = PHASE_STOPPED.to_string();
            m.next_attempt_at = String::new();
            write_mark(db, &m)?;
        }
    }
    Ok(())
}

fn due(next_attempt_at: &str) -> bool {
    next_attempt_at.is_empty() || next_attempt_at <= store::now().as_str()
}

fn backoff(attempts: u32) -> String {
    let secs = RETRY_FIRST_SECS.saturating_mul(1i64 << attempts.saturating_sub(1).min(20)).min(RETRY_MAX_SECS);
    (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The sentence a stopped share or target carries, in both forms.
pub fn suspended_detail() -> CodedText {
    CodedText::new(
        SUSPENDED_CODE,
        &[],
        "sharing on this node is stopped (TentaNas was disabled with sharing stopped) — enabling TentaNas brings it back",
    )
}

/// A share's judged state while sharing is stopped: an ENABLED share that
/// would serve is `suspended` and stays out of every generated config. A
/// share that is broken keeps its own error — "Wstrzymany" over it would be
/// false — and a disabled one what it was judged, so the resume does not
/// switch it on.
pub fn share_state_while(
    stopped: bool,
    share: &ShareRow,
    judged: (&'static str, CodedText),
) -> (&'static str, CodedText) {
    if stopped && share.enabled && judged.0 == "active" {
        ("suspended", suspended_detail())
    } else {
        judged
    }
}

/// A target's verdict while sharing is stopped: an ENABLED target is
/// `suspended` and judged `Remove`. A frozen target never reaches this — the
/// stop refuses while one exists (`targets::frozen_targets`, critic M3). A
/// disabled target keeps its own verdict and sentence.
pub fn target_state_while(
    stopped: bool,
    target: &TargetRow,
    judged: (&'static str, CodedText, super::targets::Disposition),
) -> (&'static str, CodedText, super::targets::Disposition) {
    if stopped && target.enabled {
        ("suspended", suspended_detail(), super::targets::Disposition::Remove)
    } else {
        judged
    }
}

/// The dedupe key of the alert saying one share did not come back.
pub fn share_alert_key(share_name: &str) -> String {
    format!("sharing:share:{share_name}")
}

/// The dedupe key of the node alert saying the resume keeps failing.
const RESUME_ALERT_KEY: &str = "sharing:resume";

// ----- what a stop takes out -------------------------------------------------------

/// What a stop takes out of service on this node, as the approver sees it:
/// the asking organisation's shares and targets BY NAME, every other
/// organisation's only counted (the owner's rule for another tenant's
/// resources), and the per-protocol counts of the whole node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopPlan {
    pub own_shares: Vec<String>,
    pub own_targets: Vec<String>,
    pub other_shares: usize,
    pub other_targets: usize,
    pub smb: usize,
    pub nfs: usize,
    pub iscsi: usize,
    pub nvmet: usize,
}

impl StopPlan {
    pub fn shares(&self) -> usize {
        self.smb + self.nfs
    }
    pub fn targets(&self) -> usize {
        self.iscsi + self.nvmet
    }
}

/// Reads the plan from the node's rows: every ENABLED share and target (a
/// disabled one serves nothing and has nothing to stop). A row whose owner
/// is not `org_id` — or has no known owner — is counted, never named.
pub fn plan(db: &DbPool, org_id: &str) -> Result<StopPlan> {
    let shares = store::list_shares(db)?;
    let share_owners = store::share_owners(db)?;
    let targets = store::list_targets(db)?;
    let target_owners = store::target_owners(db)?;
    let own = |owners: &std::collections::HashMap<String, String>, id: &str| {
        !org_id.is_empty() && owners.get(id).map(String::as_str) == Some(org_id)
    };
    let mut plan = StopPlan::default();
    for share in shares.iter().filter(|s| s.enabled) {
        match share.protocol.as_str() {
            "smb" => plan.smb += 1,
            _ => plan.nfs += 1,
        }
        if own(&share_owners, &share.share_id) {
            plan.own_shares.push(share.name.clone());
        } else {
            plan.other_shares += 1;
        }
    }
    for target in targets.iter().filter(|t| t.enabled) {
        match target.protocol.as_str() {
            "nvmet" => plan.nvmet += 1,
            _ => plan.iscsi += 1,
        }
        if own(&target_owners, &target.target_id) {
            plan.own_targets.push(target.name.clone());
        } else {
            plan.other_targets += 1;
        }
    }
    plan.own_shares.sort();
    plan.own_targets.sort();
    Ok(plan)
}

/// The parked request's detail (wave 6: a code with parameters the approver's
/// screen words, and the node's English sentence for the audit row, the alert
/// and the tooltip). `node` is the node's NAME, '' when it has none.
pub fn stop_detail(plan: &StopPlan, node: &str) -> CodedText {
    let joined = |names: &[String]| names.join(", ");
    let text = format!(
        "stops sharing on {}: shares {} ({} SMB, {} NFS{}), targets {} ({} iSCSI, {} NVMe-oF{}), then disables TentaNas on every node",
        if node.is_empty() { "this node" } else { node },
        if plan.own_shares.is_empty() { "—".to_string() } else { joined(&plan.own_shares) },
        plan.smb,
        plan.nfs,
        if plan.other_shares > 0 { format!(", {} of other organisations", plan.other_shares) } else { String::new() },
        if plan.own_targets.is_empty() { "—".to_string() } else { joined(&plan.own_targets) },
        plan.iscsi,
        plan.nvmet,
        if plan.other_targets > 0 { format!(", {} of other organisations", plan.other_targets) } else { String::new() },
    );
    CodedText::new(
        "sharing_stop",
        &[
            ("node", node.to_string()),
            ("shares", joined(&plan.own_shares)),
            ("targets", joined(&plan.own_targets)),
            ("other_shares", plan.other_shares.to_string()),
            ("other_targets", plan.other_targets.to_string()),
            ("smb", plan.smb.to_string()),
            ("nfs", plan.nfs.to_string()),
            ("iscsi", plan.iscsi.to_string()),
            ("nvmet", plan.nvmet.to_string()),
        ],
        text,
    )
}

// ----- the steps -------------------------------------------------------------------

/// Who stopped sharing: the parked request, its author, and the organisation
/// the job belongs to (the one whose names the job may say).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopContext {
    pub request_id: String,
    pub requested_by: String,
    pub approved_by: String,
    pub org_id: String,
}

/// What the two jobs do to the node, behind one seam so the ORDER — and what
/// happens after a failure — is testable on its own. `NodeOps` is the real
/// one; with `broker::test_channel` installed it runs end to end in a test.
#[async_trait::async_trait]
pub trait SharingOps: Send + Sync {
    /// How many enabled targets are frozen (portal drift) and serving.
    fn frozen_targets(&self) -> Result<usize>;
    /// What serves now: the enabled shares judged `active`, the enabled
    /// targets in the kernel.
    fn serving(&self) -> Result<Serving>;
    /// Of `recorded`, what does NOT serve now.
    fn not_serving(&self, recorded: &Serving) -> Result<Serving>;
    /// Re-applies the share configs as the rows and the record now say. The
    /// caller holds `shares::apply_mutex`.
    async fn apply_shares(&self, trigger: super::shares::ApplyTrigger) -> Result<()>;
    /// Takes every target the node judged `Remove` out of the kernel and
    /// answers how many ENABLED targets are still in it.
    async fn remove_targets(&self) -> Result<usize>;
    /// Puts every target the node judged `Apply` back into the kernel.
    async fn restore_targets(&self) -> Result<()>;
    /// Disables TentaNas (fleet-wide).
    fn disable_app(&self, stop: &StopContext) -> Result<()>;
}

/// Where a step's line is written: the job's own lines, or a test's list.
pub trait StepSink: Send + Sync {
    fn line(&self, position: i64, state: &str, reasons: &[NasHealthReason]);
    fn log(&self, line: &str);
}

impl StepSink for JobHandle {
    fn line(&self, position: i64, state: &str, reasons: &[NasHealthReason]) {
        if let Err(e) = store::set_job_disk(self.db(), &self.job_id, position, state, None, reasons) {
            tracing::warn!("tentanas sharing job {}: step line not written: {e}", self.job_id);
        }
    }
    fn log(&self, line: &str) {
        JobHandle::log(self, line);
    }
}

fn reason(code: &str, params: &[(&str, String)]) -> NasHealthReason {
    super::disks::coded_reason(code, params)
}

/// Rows as the job may say them: `org_id`'s by name, every other one counted.
fn named(rows: &[RecordedRow], org_id: &str) -> (String, usize) {
    let own: Vec<&str> = rows
        .iter()
        .filter(|r| !org_id.is_empty() && r.org_id == org_id)
        .map(|r| r.name.as_str())
        .collect();
    (own.join(", "), rows.len() - own.len())
}

/// A coded list of rows (`shares`/`targets` by name, `other_*` counted).
fn rows_reason(code: &str, left: &Serving, org_id: &str) -> NasHealthReason {
    let (shares, other_shares) = named(&left.shares, org_id);
    let (targets, other_targets) = named(&left.targets, org_id);
    reason(
        code,
        &[
            ("shares", shares),
            ("targets", targets),
            ("other_shares", other_shares.to_string()),
            ("other_targets", other_targets.to_string()),
        ],
    )
}

/// Why a step failed, as a code the screen words. The node's own error text
/// can name another organisation's share or target (a helper line, an
/// `exportfs` message), so it goes to the node log and never to the line.
fn failure_reason(error: &anyhow::Error) -> NasHealthReason {
    let privilege = error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<super::broker::BrokerError>(),
            Some(super::broker::BrokerError::Unarmed(_)) | Some(super::broker::BrokerError::HelperVersion(_))
        )
    });
    reason(if privilege { "step_privilege" } else { "step_failed" }, &[])
}

/// The stop (n18d), in order: record → shares → targets → disable. The
/// caller holds `shares::apply_mutex` for the whole run. Refused before
/// anything moves while sharing is already stopped here or a frozen target
/// serves. The first failure stops it BEFORE the disable, and what was
/// serving is put back; a rollback that could not put everything back says
/// what, by name (`sharing_stop_failed_partial`).
pub async fn run_stop(db: &DbPool, ops: &dyn SharingOps, sink: &dyn StepSink, stop: &StopContext, plan: &StopPlan) -> Result<()> {
    if mark(db)?.is_some() {
        sink.line(0, "failed", &[reason("already_stopped", &[])]);
        sink.line(1, "skipped", &[]);
        sink.line(2, "skipped", &[]);
        return Err(anyhow!("refusal:sharing_stop_busy"));
    }
    let frozen = ops.frozen_targets()?;
    if frozen > 0 {
        sink.line(0, "skipped", &[]);
        sink.line(1, "failed", &[reason("targets_frozen", &[("count", frozen.to_string())])]);
        sink.line(2, "skipped", &[]);
        sink.log("refused before anything stopped: a block target with a moved portal still serves");
        return Err(anyhow!("refusal:sharing_stop_frozen_targets"));
    }
    let serving = ops.serving()?;
    let mut record = Suspension {
        phase: PHASE_STOPPING.to_string(),
        since: store::now(),
        request_id: stop.request_id.clone(),
        requested_by: stop.requested_by.clone(),
        org_id: stop.org_id.clone(),
        serving: serving.clone(),
        attempts: 0,
        next_attempt_at: String::new(),
    };
    write_mark(db, &record)?;
    let org = stop.org_id.as_str();

    // 1. SMB and NFS: one apply rewrites both configs without the shares.
    sink.line(0, "running", &[]);
    if let Err(e) = ops.apply_shares(super::shares::ApplyTrigger::Change).await {
        tracing::warn!("tentanas sharing stop: shares not taken out: {e:#}");
        return rollback(db, ops, sink, 0, failure_reason(&e), &serving, org, "the shares could not be taken out of service").await;
    }
    sink.line(0, "done", &[reason("shares_stopped", &[("smb", plan.smb.to_string()), ("nfs", plan.nfs.to_string())])]);
    sink.log(&format!("shares: {} SMB and {} NFS taken out of service", plan.smb, plan.nfs));

    // 2. iSCSI and NVMe-oF: the sweep takes out what the judgement now says
    // `Remove`, and the kernel is read back — a target still there is a
    // client still holding a raw disk.
    sink.line(1, "running", &[]);
    match ops.remove_targets().await {
        Ok(0) => {
            sink.line(1, "done", &[reason("targets_stopped", &[("iscsi", plan.iscsi.to_string()), ("nvmet", plan.nvmet.to_string())])]);
            sink.log(&format!("targets: {} iSCSI and {} NVMe-oF taken out of the kernel", plan.iscsi, plan.nvmet));
        }
        Ok(left) => {
            let why = reason("targets_left", &[("count", left.to_string())]);
            return rollback(db, ops, sink, 1, why, &serving, org, "block targets are still in the kernel").await;
        }
        Err(e) => {
            tracing::warn!("tentanas sharing stop: targets not taken out: {e:#}");
            return rollback(db, ops, sink, 1, failure_reason(&e), &serving, org, "the block targets could not be taken out of the kernel").await;
        }
    }

    // 3. The disable, only now that nothing serves any more.
    sink.line(2, "running", &[]);
    if let Err(e) = ops.disable_app(stop) {
        tracing::warn!("tentanas sharing stop: TentaNas not disabled: {e:#}");
        return rollback(db, ops, sink, 2, reason("step_failed", &[]), &serving, org, "TentaNas could not be disabled").await;
    }
    sink.line(2, "done", &[]);
    record.phase = PHASE_STOPPED.to_string();
    write_mark(db, &record)?;
    sink.log("TentaNas disabled; enabling it again brings the shares and targets back");
    Ok(())
}

/// Puts back what a failed stop had stopped. The failed line keeps its own
/// reason and, when the rollback could not bring everything back, a second
/// one naming what stays out (critic wave 10, M4).
#[allow(clippy::too_many_arguments)]
async fn rollback(
    db: &DbPool,
    ops: &dyn SharingOps,
    sink: &dyn StepSink,
    failed_at: i64,
    failure: NasHealthReason,
    serving: &Serving,
    org_id: &str,
    why: &str,
) -> Result<()> {
    sink.line(failed_at, "failed", std::slice::from_ref(&failure));
    for position in failed_at + 1..3 {
        sink.line(position, "skipped", &[]);
    }
    let lifted = lift(db);
    // Unattended, like the resume: an unreadable dataset listing must not
    // write every ZFS share out as an error (critic MINOR 4).
    let shares = ops.apply_shares(super::shares::ApplyTrigger::Resume).await;
    let targets = ops.restore_targets().await;
    for (what, outcome) in [("record", lifted.err()), ("shares", shares.err()), ("targets", targets.err())] {
        if let Some(e) = outcome {
            tracing::warn!("tentanas sharing stop: rollback of {what} failed: {e:#}");
        }
    }
    sink.log(&format!("stopped before the disable: {why}; nothing was disabled"));
    let left = ops.not_serving(serving).unwrap_or_else(|e| {
        tracing::warn!("tentanas sharing stop: what serves after the rollback is unknown: {e:#}");
        serving.clone()
    });
    if left.shares.is_empty() && left.targets.is_empty() {
        sink.log("rolled back: everything that was serving serves again");
        return Err(anyhow!("refusal:sharing_stop_failed"));
    }
    sink.line(failed_at, "failed", &[failure, rows_reason("rollback_partial", &left, org_id)]);
    sink.log(&format!(
        "rolled back only in part: {} share(s) and {} target(s) do not serve again; each row says why",
        left.shares.len(),
        left.targets.len()
    ));
    Err(anyhow!("refusal:sharing_stop_failed_partial"))
}

/// The resume, when TentaNas is enabled again. The caller holds
/// `shares::apply_mutex` and set the record `resuming`. The record is lifted,
/// the shares re-applied PER ROW (a share that does not come back keeps its
/// own error and gets an alert by name; the others serve), then the targets.
/// An apply that fails as a whole puts the record back `stopped` with a
/// back-off.
pub async fn run_resume(db: &DbPool, ops: &dyn SharingOps, sink: &dyn StepSink, record: Suspension, org_id: &str) -> Result<()> {
    lift(db)?;
    sink.line(0, "running", &[]);
    if let Err(e) = ops.apply_shares(super::shares::ApplyTrigger::Resume).await {
        tracing::warn!("tentanas sharing resume: shares not restored: {e:#}");
        let attempts = record.attempts + 1;
        write_mark(db, &Suspension { phase: PHASE_STOPPED.to_string(), attempts, next_attempt_at: backoff(attempts), ..record })?;
        sink.line(0, "failed", &[failure_reason(&e)]);
        sink.line(1, "skipped", &[]);
        let _ = store::raise_coded_alert(
            db,
            RESUME_ALERT_KEY,
            "warning",
            "node",
            "sharing",
            &store::AlertText::new(
                "sharing_resume_failed",
                "Sharing could not be resumed on this node",
                "the share configs could not be written; the node retries with a back-off",
            )
            .param("attempts", attempts),
        );
        sink.log("the shares could not be served again; the next attempt waits longer");
        return Err(anyhow!("refusal:sharing_resume_failed"));
    }
    let _ = store::resolve_alert(db, RESUME_ALERT_KEY);
    let recorded_shares = Serving { shares: record.serving.shares.clone(), targets: Vec::new() };
    let left_shares = ops.not_serving(&recorded_shares).map(|s| s.shares).unwrap_or_else(|_| recorded_shares.shares.clone());
    raise_share_alerts(db, &left_shares);
    if left_shares.is_empty() {
        store::delete_setting(db, SETTING_RESUME_PENDING)?;
        sink.line(0, "done", &[]);
        sink.log("shares: every share that was serving serves again");
    } else {
        write_pending(db, &Pending { shares: left_shares.clone(), attempts: 1, next_attempt_at: backoff(1) })?;
        let left = Serving { shares: left_shares.clone(), targets: Vec::new() };
        sink.line(0, "incomplete", &[rows_reason("not_resumed", &left, org_id)]);
        sink.log(&format!("shares: {} did not come back; each is retried and has an alert", left_shares.len()));
    }

    sink.line(1, "running", &[]);
    if let Err(e) = ops.restore_targets().await {
        tracing::warn!("tentanas sharing resume: targets not restored: {e:#}");
    }
    let recorded_targets = Serving { shares: Vec::new(), targets: record.serving.targets.clone() };
    let left_targets = ops.not_serving(&recorded_targets).map(|s| s.targets).unwrap_or_else(|_| recorded_targets.targets.clone());
    if left_targets.is_empty() {
        sink.line(1, "done", &[]);
        sink.log("targets: every target that was serving is back in the kernel");
    } else {
        // The node's own reconcile keeps applying them and raises its own
        // per-target alert (`targets::report_target_failures`).
        let left = Serving { shares: Vec::new(), targets: left_targets.clone() };
        sink.line(1, "incomplete", &[rows_reason("not_resumed", &left, org_id)]);
        sink.log(&format!("targets: {} not back yet; the node's reconcile retries them", left_targets.len()));
    }
    if left_shares.is_empty() && left_targets.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("refusal:sharing_resume_partial"))
    }
}

/// Another pass over the shares a resume could not bring back — one job per
/// back-off interval at most, and never past `PENDING_MAX_ATTEMPTS`: then
/// their alerts stay for the admin, and any later share edit applies them.
pub async fn run_retry(db: &DbPool, ops: &dyn SharingOps, sink: &dyn StepSink, pending_rows: Pending, org_id: &str) -> Result<()> {
    sink.line(1, "skipped", &[]);
    sink.line(0, "running", &[]);
    let applied = ops.apply_shares(super::shares::ApplyTrigger::Resume).await;
    let still = match &applied {
        Ok(()) => ops
            .not_serving(&Serving { shares: pending_rows.shares.clone(), targets: Vec::new() })
            .map(|s| s.shares)
            .unwrap_or_else(|_| pending_rows.shares.clone()),
        Err(e) => {
            tracing::warn!("tentanas sharing resume retry: {e:#}");
            pending_rows.shares.clone()
        }
    };
    raise_share_alerts(db, &still);
    if still.is_empty() {
        store::delete_setting(db, SETTING_RESUME_PENDING)?;
        sink.line(0, "done", &[]);
        sink.log("shares: the remaining shares serve again");
        return Ok(());
    }
    let attempts = pending_rows.attempts + 1;
    if attempts >= PENDING_MAX_ATTEMPTS {
        store::delete_setting(db, SETTING_RESUME_PENDING)?;
        sink.log("shares: still not back after the last retry; their alerts stay, and the next share change applies them");
    } else {
        write_pending(db, &Pending { shares: still.clone(), attempts, next_attempt_at: backoff(attempts) })?;
    }
    let left = Serving { shares: still, targets: Vec::new() };
    sink.line(0, "incomplete", &[rows_reason("not_resumed", &left, org_id)]);
    Err(anyhow!("refusal:sharing_resume_partial"))
}

fn write_pending(db: &DbPool, p: &Pending) -> Result<()> {
    store::set_setting(db, SETTING_RESUME_PENDING, &serde_json::to_string(p)?)
}

/// One alert per share that did not come back, by its real name, owned by
/// the share's organisation ('share' subject). The share apply resolves it
/// the moment the share serves again.
fn raise_share_alerts(db: &DbPool, shares: &[RecordedRow]) {
    for share in shares {
        if let Err(e) = store::raise_coded_alert(
            db,
            &share_alert_key(&share.name),
            "warning",
            "share",
            &share.name,
            &store::AlertText::new(
                "sharing_share_not_resumed",
                format!("Share {} did not come back after sharing was resumed", share.name),
                "the share's own state says why; it is retried with a back-off",
            )
            .param("share", &share.name),
        ) {
            tracing::warn!("tentanas sharing: alert for share {} not raised: {e}", share.name);
        }
    }
}

// ----- the node's own implementation ------------------------------------------------

/// The real steps on this node.
pub struct NodeOps {
    pub main_db: DbPool,
    pub db: DbPool,
    pub addon_id: String,
    pub explicit: Option<Arc<ElevationToken>>,
    /// The key the target secrets are read with; `None` loads the node's
    /// master key when a target has to be applied.
    pub cipher: Option<Arc<crate::crypto::SettingsCipher>>,
}

#[async_trait::async_trait]
impl SharingOps for NodeOps {
    fn frozen_targets(&self) -> Result<usize> {
        Ok(super::targets::frozen_targets(&self.db)?.len())
    }

    fn serving(&self) -> Result<Serving> {
        let share_owners = store::share_owners(&self.db)?;
        let target_owners = store::target_owners(&self.db)?;
        let row = |id: &str, name: &str, owners: &std::collections::HashMap<String, String>| RecordedRow {
            id: id.to_string(),
            name: name.to_string(),
            org_id: owners.get(id).cloned().unwrap_or_default(),
        };
        Ok(Serving {
            shares: store::list_shares(&self.db)?
                .iter()
                .filter(|s| s.enabled && s.state == "active")
                .map(|s| row(&s.share_id, &s.name, &share_owners))
                .collect(),
            targets: store::list_targets(&self.db)?
                .iter()
                .filter(|t| t.enabled && super::targets::in_kernel(t))
                .map(|t| row(&t.target_id, &t.name, &target_owners))
                .collect(),
        })
    }

    fn not_serving(&self, recorded: &Serving) -> Result<Serving> {
        let shares = store::list_shares(&self.db)?;
        let targets = store::list_targets(&self.db)?;
        Ok(Serving {
            shares: recorded
                .shares
                .iter()
                .filter(|r| !shares.iter().any(|s| s.share_id == r.id && s.state == "active"))
                .cloned()
                .collect(),
            targets: recorded
                .targets
                .iter()
                .filter(|r| !targets.iter().any(|t| t.target_id == r.id && super::targets::in_kernel(t)))
                .cloned()
                .collect(),
        })
    }

    async fn apply_shares(&self, trigger: super::shares::ApplyTrigger) -> Result<()> {
        let log = super::shares::apply_locked(&self.db, &self.main_db, &self.addon_id, self.explicit.as_deref(), trigger).await?;
        for line in log {
            tracing::info!("tentanas sharing: {line}");
        }
        Ok(())
    }

    async fn remove_targets(&self) -> Result<usize> {
        let sweep = super::targets::sweep_removals(&self.db, self.explicit.as_deref()).await?;
        for line in &sweep.log {
            tracing::info!("tentanas sharing: {line}");
        }
        super::targets::enabled_in_kernel(&self.db)
    }

    async fn restore_targets(&self) -> Result<()> {
        let cipher = match &self.cipher {
            Some(cipher) => cipher.clone(),
            None => {
                let key = crate::crypto::load_or_create_master_key().map_err(|e| anyhow!("no master key: {e}"))?;
                Arc::new(crate::crypto::SettingsCipher::new(&key))
            }
        };
        let sweep = super::targets::sweep_applies(&self.db, &cipher, self.explicit.as_deref()).await?;
        for line in &sweep.log {
            tracing::info!("tentanas sharing: {line}");
        }
        if sweep.failed.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("{} block target(s) not applied", sweep.failed.len()))
        }
    }

    fn disable_app(&self, stop: &StopContext) -> Result<()> {
        disable_instance(&self.main_db, &self.addon_id, stop)
    }
}

/// Disables the instance exactly as the Applications screen's switch does
/// (`addon_toggle`): the replicated flag, the native disable hook, and the
/// audit row — which says it came with the stop, which request it was, who
/// asked and who released it.
pub fn disable_instance(main_db: &DbPool, addon_id: &str, stop: &StopContext) -> Result<()> {
    let previous = crate::db::repository::get_addon_enabled(main_db, addon_id)?
        .ok_or_else(|| anyhow!("the TentaNas instance does not exist"))?;
    anyhow::ensure!(crate::db::repository::set_addon_enabled(main_db, addon_id, false)?, "the TentaNas instance does not exist");
    if previous {
        if let Ok(Some(addon)) = crate::db::repository::get_addon(main_db, addon_id) {
            if let Ok(manifest) = crate::addon::lifecycle::parse_manifest_toml(&addon.manifest_json) {
                crate::addon::native_apps::notify_enabled(main_db, addon_id, &addon.package_id, &manifest, false);
            }
        }
    }
    let details = serde_json::json!({
        "enabled_old": previous,
        "enabled_new": false,
        "with": STOP_KIND,
        "request_id": stop.request_id,
        "requested_by": stop.requested_by,
        "approved_by": stop.approved_by,
    })
    .to_string();
    let node = crate::sync::runtime::local_node_id();
    let _ = crate::db::repository::log_audit_full(
        main_db,
        Some(&stop.approved_by),
        Some(addon_id),
        "addon_toggle",
        Some("addon"),
        Some(addon_id),
        Some(&details),
        "warning",
        "A",
        Some("ok"),
        (!stop.org_id.is_empty()).then_some(stop.org_id.as_str()),
        None,
        node.as_deref(),
    );
    Ok(())
}

// ----- the resume trigger -------------------------------------------------------------

/// What a resume pass would do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// Sharing was stopped and TentaNas is enabled: bring it back.
    Resume(Suspension),
    /// Some shares did not come back last time: try them again.
    Retry(Pending),
}

/// The instance and the pass that is due, when one is (critic wave 10, B1):
/// TentaNas is ENABLED — a disabled instance never serves again on its own,
/// in mode A its loops keep running while it is off — and either the
/// record is `stopped` (never `stopping`: that is a stop in progress; never
/// `resuming`) and its back-off has passed, or shares wait for a retry.
pub fn resume_wanted(main_db: &DbPool, db: &DbPool) -> Option<(String, Due)> {
    let addon_id = match crate::db::repository::get_package_instance(main_db, super::PACKAGE_ID) {
        Ok(Some((addon_id, true))) => addon_id,
        _ => return None,
    };
    match mark(db) {
        Ok(Some(m)) => (m.phase == PHASE_STOPPED && due(&m.next_attempt_at)).then(|| (addon_id, Due::Resume(m))),
        Ok(None) => match pending(db) {
            Ok(Some(p)) if !p.shares.is_empty() && due(&p.next_attempt_at) => Some((addon_id, Due::Retry(p))),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Starts the resume (or a retry) when it is due and nothing else touches
/// sharing: the node-wide share lock must be FREE — a stop, a resume or a
/// share apply holds it — and a privilege channel must be there (mode A, or
/// a mode-B session armed). Called when TentaNas is enabled
/// (`native_on_enable`) and on every armed tick of the target restore loop.
/// The lock is taken here and handed to the job, so two passes never run.
pub async fn resume_if_due(main_db: &DbPool, db: &DbPool) -> Option<NasJob> {
    resume_wanted(main_db, db)?;
    if !super::broker::channel_available(db).await {
        return None;
    }
    let guard = super::shares::apply_mutex().try_lock().ok()?;
    // Read again under the lock: a stop may have started in between.
    let (addon_id, due) = resume_wanted(main_db, db)?;
    let org_id = match &due {
        Due::Resume(m) => m.requested_org().to_string(),
        Due::Retry(_) => String::new(),
    };
    if let Due::Resume(m) = &due {
        if let Err(e) = write_mark(db, &Suspension { phase: PHASE_RESUMING.to_string(), ..m.clone() }) {
            tracing::warn!("tentanas: the sharing resume did not start: {e}");
            return None;
        }
    }
    let ops = NodeOps { main_db: main_db.clone(), db: db.clone(), addon_id, explicit: None, cipher: None };
    let body_db = db.clone();
    let owner = (!org_id.is_empty()).then(|| org_id.clone());
    let restore_on_failure = due.clone();
    let spawned = super::jobs::spawn_steps(db, RESUME_KIND, "", super::scheduler::STARTED_BY, owner.as_deref(), move |h| async move {
        let _serial = guard;
        match due {
            Due::Resume(m) => run_resume(&body_db, &ops, &h, m, &org_id).await,
            Due::Retry(p) => run_retry(&body_db, &ops, &h, p, &org_id).await,
        }
    });
    match spawned {
        Ok(job) => Some(job),
        Err(e) => {
            if let Due::Resume(m) = restore_on_failure {
                let _ = write_mark(db, &m);
            }
            tracing::warn!("tentanas: the sharing resume did not start: {e}");
            None
        }
    }
}

impl Suspension {
    /// The organisation the stop was asked in ('' when unknown).
    pub fn requested_org(&self) -> &str {
        &self.org_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn pool() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// A platform database with one TentaNas instance, enabled or not.
    fn main_db(enabled: bool) -> (tempfile::TempDir, DbPool) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let main = crate::db::init(&dir.path().join("sharing_main.db")).expect("init");
        main.write().unwrap().execute(
            "INSERT INTO addons (addon_id, name, version, package_id, package_version, runtime, is_enabled) \
             VALUES ('tentanas-w10', 'tentanas', '1.0.0', 'tentanas', '1.0.0', 'native', ?1)",
            rusqlite::params![enabled as i64],
        ).expect("instance row");
        (dir, main)
    }

    fn share(id: &str, name: &str, protocol: &str, enabled: bool) -> ShareRow {
        ShareRow {
            share_id: id.into(),
            name: name.into(),
            protocol: protocol.into(),
            source_path: format!("/tank/{name}"),
            enabled,
            created_at: "now".into(),
            updated_at: "now".into(),
            ..Default::default()
        }
    }

    fn target(id: &str, name: &str, protocol: &str, enabled: bool) -> TargetRow {
        TargetRow {
            target_id: id.into(),
            name: name.into(),
            protocol: protocol.into(),
            wwn: format!("iqn.2026-09.local.tentaflow:{name}"),
            enabled,
            auth_method: "none".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            ..Default::default()
        }
    }

    fn row(id: &str, name: &str, org: &str) -> RecordedRow {
        RecordedRow { id: id.into(), name: name.into(), org_id: org.into() }
    }

    fn serving_of(shares: &[RecordedRow], targets: &[RecordedRow]) -> Serving {
        Serving { shares: shares.to_vec(), targets: targets.to_vec() }
    }

    /// The fake node: what each step answers, and the order they ran in.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<String>>,
        fail_shares: bool,
        targets_left: usize,
        fail_disable: bool,
        frozen: usize,
        serving: Serving,
        /// What `not_serving` answers (whatever is asked).
        left: Serving,
        db: Option<DbPool>,
        /// Asked in the middle of the stop (inside the share apply): what a
        /// restore tick or the enable hook would find.
        mid_stop: Option<DbPool>,
        seen_mid_stop: Mutex<Vec<String>>,
    }

    impl Fake {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn note(&self, what: &str) {
            let phase = self.db.as_ref().and_then(|db| mark(db).unwrap()).map(|m| m.phase);
            self.calls.lock().unwrap().push(match phase {
                Some(p) => format!("{what}+{p}"),
                None => what.to_string(),
            });
        }
    }

    #[async_trait::async_trait]
    impl SharingOps for Fake {
        fn frozen_targets(&self) -> Result<usize> {
            Ok(self.frozen)
        }
        fn serving(&self) -> Result<Serving> {
            Ok(self.serving.clone())
        }
        fn not_serving(&self, _recorded: &Serving) -> Result<Serving> {
            Ok(self.left.clone())
        }
        async fn apply_shares(&self, trigger: super::super::shares::ApplyTrigger) -> Result<()> {
            self.note(&format!("shares:{trigger:?}"));
            if let (Some(main), Some(db)) = (&self.mid_stop, &self.db) {
                // A restore tick and an enable hook fire now, mid-stop.
                let wanted = resume_wanted(main, db).is_some();
                let job = resume_if_due(main, db).await.is_some();
                let locked = super::super::shares::apply_mutex().try_lock().is_err();
                self.seen_mid_stop.lock().unwrap().push(format!("wanted={wanted} job={job} locked={locked}"));
            }
            if self.fail_shares { Err(anyhow!("smbd refused the include")) } else { Ok(()) }
        }
        async fn remove_targets(&self) -> Result<usize> {
            self.note("remove_targets");
            Ok(self.targets_left)
        }
        async fn restore_targets(&self) -> Result<()> {
            self.note("restore_targets");
            Ok(())
        }
        fn disable_app(&self, _stop: &StopContext) -> Result<()> {
            self.note("disable");
            if self.fail_disable { Err(anyhow!("no instance")) } else { Ok(()) }
        }
    }

    #[derive(Default)]
    struct Lines(Mutex<Vec<(i64, String, Vec<NasHealthReason>)>>);

    impl StepSink for Lines {
        fn line(&self, position: i64, state: &str, reasons: &[NasHealthReason]) {
            self.0.lock().unwrap().push((position, state.to_string(), reasons.to_vec()));
        }
        fn log(&self, _line: &str) {}
    }

    impl Lines {
        /// The last state each line reached, and its last reasons.
        fn last(&self) -> Vec<(String, Vec<NasHealthReason>)> {
            let mut out = vec![(String::new(), Vec::new()); 3];
            for (position, state, reasons) in self.0.lock().unwrap().iter() {
                out[*position as usize] = (state.clone(), reasons.clone());
            }
            out
        }
        fn states(&self) -> Vec<String> {
            self.last().into_iter().map(|(s, _)| s).collect()
        }
    }

    fn plan_of(smb: usize, iscsi: usize) -> StopPlan {
        StopPlan { smb, iscsi, ..Default::default() }
    }

    fn stop_ctx() -> StopContext {
        StopContext { request_id: "req-1".into(), requested_by: "u-anna".into(), approved_by: "u-piotr".into(), org_id: "org-a".into() }
    }

    /// The stop runs shares → targets → disable, each while the record says
    /// `stopping`, and ends `stopped` with what was serving and which request
    /// stopped it.
    #[tokio::test]
    async fn the_stop_takes_shares_then_targets_out_and_only_then_disables() {
        let db = pool();
        let serving = serving_of(&[row("s1", "projekty", "org-a")], &[row("t1", "vm-store", "org-a")]);
        let fake = Fake { db: Some(db.clone()), serving: serving.clone(), ..Default::default() };
        let lines = Lines::default();
        run_stop(&db, &fake, &lines, &stop_ctx(), &plan_of(2, 1)).await.expect("stopped");
        assert_eq!(fake.calls(), vec!["shares:Change+stopping", "remove_targets+stopping", "disable+stopping"]);
        assert_eq!(lines.states(), vec!["done", "done", "done"]);
        let m = mark(&db).unwrap().expect("the record stays until TentaNas is enabled again");
        assert_eq!(m.phase, PHASE_STOPPED);
        assert_eq!((m.request_id.as_str(), m.requested_by.as_str(), m.org_id.as_str()), ("req-1", "u-anna", "org-a"));
        assert_eq!(m.serving, serving);
    }

    /// Critic wave 10, B1: a restore tick and the enable hook that fire in
    /// the middle of a stop resume nothing — the record says `stopping` and
    /// the node-wide share lock is held — and once the stop has finished
    /// (TentaNas enabled again) the resume is due.
    #[tokio::test]
    async fn a_restore_tick_or_an_enable_mid_stop_resumes_nothing() {
        let db = pool();
        let (_dir, main) = main_db(true);
        let _channel = super::super::broker::test_channel::install(&db);
        let fake = Fake { db: Some(db.clone()), mid_stop: Some(main.clone()), ..Default::default() };
        let lines = Lines::default();
        {
            let _serial = super::super::shares::apply_mutex().lock().await;
            run_stop(&db, &fake, &lines, &stop_ctx(), &plan_of(1, 0)).await.expect("stopped");
        }
        assert_eq!(*fake.seen_mid_stop.lock().unwrap(), vec!["wanted=false job=false locked=true".to_string()]);
        assert!(store::list_jobs(&db, 10).unwrap().is_empty(), "no resume job was started");
        assert_eq!(mark(&db).unwrap().unwrap().phase, PHASE_STOPPED, "the stop's record is untouched");
        assert!(matches!(resume_wanted(&main, &db), Some((_, Due::Resume(_)))), "enabled after the stop: due");
        // A `resuming` record (a resume running) is not due either.
        let m = mark(&db).unwrap().unwrap();
        write_mark(&db, &Suspension { phase: PHASE_RESUMING.into(), ..m }).unwrap();
        assert_eq!(resume_wanted(&main, &db), None);
    }

    /// A failure stops BEFORE the disable, whichever step fails, and puts
    /// back what had stopped: the record lifted, shares re-applied the
    /// unattended way (`Resume`: an unreadable listing is refused) and the
    /// targets re-applied. A rollback that brings everything back says
    /// `sharing_stop_failed`; one that does not names what stayed out, the
    /// asking organisation's by name and the others counted (M4).
    #[tokio::test]
    async fn a_failed_step_stops_before_the_disable_and_says_what_the_rollback_put_back() {
        for (fake, failed_at) in [
            (Fake { fail_shares: true, ..Default::default() }, 0usize),
            (Fake { targets_left: 1, ..Default::default() }, 1),
            (Fake { fail_disable: true, ..Default::default() }, 2),
        ] {
            let db = pool();
            let fake = Fake { db: Some(db.clone()), ..fake };
            let lines = Lines::default();
            let error = run_stop(&db, &fake, &lines, &stop_ctx(), &plan_of(1, 1)).await.expect_err("stopped short");
            assert_eq!(error.to_string(), "refusal:sharing_stop_failed");
            let calls = fake.calls();
            assert_eq!(calls.iter().filter(|c| c.starts_with("disable")).count(), usize::from(failed_at == 2), "{calls:?}");
            assert!(!suspended(&db).unwrap(), "the record is lifted again");
            assert!(calls.ends_with(&["shares:Resume".to_string(), "restore_targets".to_string()]), "{calls:?}");
            let states = lines.states();
            assert_eq!(states[failed_at], "failed", "{states:?}");
            assert!(states.iter().skip(failed_at + 1).all(|s| s == "skipped"), "{states:?}");
        }
        // Partial: one share of the asking organisation and one target of
        // another stay out after the rollback.
        let db = pool();
        let left = serving_of(&[row("s1", "projekty", "org-a")], &[row("t9", "bazy", "org-b")]);
        let fake = Fake { db: Some(db.clone()), targets_left: 1, left, ..Default::default() };
        let lines = Lines::default();
        let error = run_stop(&db, &fake, &lines, &stop_ctx(), &plan_of(1, 1)).await.expect_err("stopped short");
        assert_eq!(error.to_string(), "refusal:sharing_stop_failed_partial");
        let (state, reasons) = &lines.last()[1];
        assert_eq!(state, "failed");
        assert_eq!(reasons[0].code, "targets_left");
        assert_eq!(reasons[1].code, "rollback_partial");
        assert_eq!(reasons[1].params["shares"], "projekty");
        assert_eq!(reasons[1].params["targets"], "", "another organisation's target is not named");
        assert_eq!(reasons[1].params["other_targets"], "1");
    }

    /// Critic wave 10, M3: while a target with a moved portal still serves,
    /// the stop refuses before anything moves — no record, no apply.
    #[tokio::test]
    async fn the_stop_refuses_up_front_while_a_frozen_target_serves() {
        let db = pool();
        let fake = Fake { db: Some(db.clone()), frozen: 1, ..Default::default() };
        let lines = Lines::default();
        let error = run_stop(&db, &fake, &lines, &stop_ctx(), &plan_of(1, 1)).await.expect_err("refused");
        assert_eq!(error.to_string(), "refusal:sharing_stop_frozen_targets");
        assert!(fake.calls().is_empty(), "nothing was touched");
        assert!(!suspended(&db).unwrap());
        assert_eq!(lines.states(), vec!["skipped", "failed", "skipped"]);
        // And a second stop over a stopped node never touches the first's record.
        suspend_for_test(&db, PHASE_STOPPING);
        let error = run_stop(&db, &fake, &Lines::default(), &stop_ctx(), &plan_of(1, 1)).await.expect_err("busy");
        assert_eq!(error.to_string(), "refusal:sharing_stop_busy");
        assert_eq!(mark(&db).unwrap().unwrap().phase, PHASE_STOPPING);
    }

    fn suspend_for_test(db: &DbPool, phase: &str) {
        write_mark(db, &Suspension {
            phase: phase.into(),
            since: store::now(),
            request_id: "req-1".into(),
            requested_by: "u-anna".into(),
            org_id: "org-a".into(),
            serving: serving_of(&[row("s1", "projekty", "org-a"), row("s2", "kadry", "org-b")], &[row("t1", "vm-store", "org-a")]),
            attempts: 0,
            next_attempt_at: String::new(),
        }).unwrap();
    }

    /// Critic wave 10, B2: the resume applies the shares per row. A share
    /// that does not come back does not hold the others back: it gets an
    /// alert by its real name (its organisation's), the job says it, the
    /// share waits for a back-off retry — and the record is gone, so the
    /// node is not dark.
    #[tokio::test]
    async fn one_share_that_does_not_come_back_does_not_block_the_others() {
        let db = pool();
        suspend_for_test(&db, PHASE_RESUMING);
        let record = mark(&db).unwrap().unwrap();
        let fake = Fake { db: Some(db.clone()), left: serving_of(&[row("s2", "kadry", "org-b")], &[]), ..Default::default() };
        let lines = Lines::default();
        let error = run_resume(&db, &fake, &lines, record, "org-a").await.expect_err("partial");
        assert_eq!(error.to_string(), "refusal:sharing_resume_partial");
        assert_eq!(fake.calls(), vec!["shares:Resume", "restore_targets"], "the targets are not held back either");
        assert!(!suspended(&db).unwrap(), "sharing is back on the node");
        let (state, reasons) = &lines.last()[0];
        assert_eq!(state, "incomplete");
        assert_eq!(reasons[0].code, "not_resumed");
        assert_eq!(reasons[0].params["other_shares"], "1", "another organisation's share counted in the job");
        let alerts = store::list_alerts(&db, true).unwrap();
        let alert = alerts.iter().find(|a| a.subject_kind == "share").expect("the share's alert");
        assert_eq!(alert.subject_id, "kadry", "by its real name");
        let p = pending(&db).unwrap().expect("retried later");
        assert_eq!(p.shares, vec![row("s2", "kadry", "org-b")]);
        assert!(p.next_attempt_at > store::now(), "with a back-off");
    }

    /// No retry storm: a resume that fails as a whole puts the record back
    /// `stopped` with a back-off, so the next tick is not due; it raises one
    /// coded node alert; and the retries of left-over shares stop after
    /// `PENDING_MAX_ATTEMPTS`.
    #[tokio::test]
    async fn a_failed_resume_backs_off_and_raises_one_alert() {
        let db = pool();
        let (_dir, main) = main_db(true);
        suspend_for_test(&db, PHASE_RESUMING);
        let record = mark(&db).unwrap().unwrap();
        let fake = Fake { db: Some(db.clone()), fail_shares: true, ..Default::default() };
        run_resume(&db, &fake, &Lines::default(), record, "org-a").await.expect_err("failed");
        let m = mark(&db).unwrap().expect("still stopped");
        assert_eq!((m.phase.as_str(), m.attempts), (PHASE_STOPPED, 1));
        assert!(m.next_attempt_at > store::now());
        assert_eq!(resume_wanted(&main, &db), None, "not due until the back-off passed");
        let open: Vec<_> = store::list_alerts(&db, true).unwrap().into_iter().filter(|a| a.code == "sharing_resume_failed").collect();
        assert_eq!(open.len(), 1);
        // The wait doubles and is capped.
        assert!(backoff(1) < backoff(2) && backoff(2) < backoff(3));
        assert!(backoff(30) <= (chrono::Utc::now() + chrono::Duration::seconds(RETRY_MAX_SECS + 5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true));

        // Retries of left-over shares end.
        let fake = Fake { db: Some(db.clone()), left: serving_of(&[row("s2", "kadry", "org-b")], &[]), ..Default::default() };
        let p = Pending { shares: vec![row("s2", "kadry", "org-b")], attempts: PENDING_MAX_ATTEMPTS - 1, next_attempt_at: String::new() };
        run_retry(&db, &fake, &Lines::default(), p, "org-a").await.expect_err("still out");
        assert_eq!(pending(&db).unwrap(), None, "no retry after the last one; the alert stays");
        // And a retry that brings it back clears it.
        let fake = Fake { db: Some(db.clone()), ..Default::default() };
        write_pending(&db, &Pending { shares: vec![row("s2", "kadry", "org-b")], attempts: 1, next_attempt_at: String::new() }).unwrap();
        run_retry(&db, &fake, &Lines::default(), pending(&db).unwrap().unwrap(), "org-a").await.expect("back");
        assert_eq!(pending(&db).unwrap(), None);
    }

    /// After a restart no sharing job runs: a `stopping` or `resuming`
    /// record is `stopped` again, and resumed once TentaNas is enabled.
    #[test]
    fn a_stop_or_resume_interrupted_by_a_restart_is_resumed() {
        for phase in [PHASE_STOPPING, PHASE_RESUMING] {
            let db = pool();
            suspend_for_test(&db, phase);
            // While the node-wide lock is held (a job of this process runs,
            // or another test's apply) the record is left alone.
            {
                let _held = super::super::shares::apply_mutex().blocking_lock();
                recover_after_restart(&db).unwrap();
                assert_eq!(mark(&db).unwrap().unwrap().phase, phase, "a live job's record is not touched");
            }
            for _ in 0..500 {
                recover_after_restart(&db).unwrap();
                if mark(&db).unwrap().unwrap().phase == PHASE_STOPPED {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert_eq!(mark(&db).unwrap().unwrap().phase, PHASE_STOPPED);
        }
    }

    /// While sharing is stopped every enabled share that would serve and
    /// every enabled target is judged out of service; a BROKEN share keeps
    /// its own error (B2), and a disabled row its own state.
    #[test]
    fn a_stopped_node_judges_every_enabled_row_out_of_service() {
        use super::super::targets::Disposition;
        let on = share("s1", "projekty", "smb", true);
        let off = share("s2", "stare", "smb", false);
        let active = ("active", CodedText::default());
        assert_eq!(share_state_while(true, &on, active.clone()).0, "suspended");
        assert_eq!(share_state_while(true, &on, active.clone()).1.reasons[0].code, SUSPENDED_CODE);
        let broken = ("error", CodedText::uncoded("source path is not mounted"));
        assert_eq!(share_state_while(true, &on, broken).0, "error", "a broken share is not called paused");
        assert_eq!(share_state_while(true, &off, ("disabled", CodedText::default())).0, "disabled");
        assert_eq!(share_state_while(false, &on, active.clone()).0, "active");

        let t = target("t1", "vm-store", "iscsi", true);
        let pending_apply = ("pending", CodedText::default(), Disposition::Apply);
        let judged = target_state_while(true, &t, pending_apply.clone());
        assert_eq!((judged.0, judged.2), ("suspended", Disposition::Remove));
        assert_eq!(target_state_while(false, &t, pending_apply).2, Disposition::Apply);
        let disabled = target("t2", "old", "nvmet", false);
        let kept = ("disabled", CodedText::uncoded("imported without its secret"), Disposition::Remove);
        assert_eq!(target_state_while(true, &disabled, kept).1.text, "imported without its secret");
    }

    /// The approver reads which shares and targets stop BY NAME — the
    /// asking organisation's; another organisation's are counted, never
    /// named — and the counts per protocol cover the whole node.
    #[test]
    fn the_request_names_the_callers_shares_and_targets_and_counts_the_others() {
        let db = pool();
        store::upsert_share(&db, "org-a", &share("s1", "projekty", "smb", true)).unwrap();
        store::upsert_share(&db, "org-a", &share("s2", "media", "nfs", true)).unwrap();
        store::upsert_share(&db, "org-a", &share("s3", "stare", "smb", false)).unwrap();
        store::upsert_share(&db, "org-b", &share("s4", "kadry", "smb", true)).unwrap();
        store::upsert_target(&db, "org-a", &target("t1", "vm-store", "iscsi", true)).unwrap();
        store::upsert_target(&db, "org-b", &target("t2", "bazy", "nvmet", true)).unwrap();
        let plan = plan(&db, "org-a").unwrap();
        assert_eq!(plan.own_shares, vec!["media".to_string(), "projekty".to_string()]);
        assert_eq!(plan.own_targets, vec!["vm-store".to_string()]);
        assert_eq!((plan.other_shares, plan.other_targets), (1, 1));
        assert_eq!((plan.smb, plan.nfs, plan.iscsi, plan.nvmet), (2, 1, 1, 1));
        let detail = stop_detail(&plan, "helios");
        let params = &detail.reasons[0].params;
        assert_eq!(detail.reasons[0].code, "sharing_stop");
        assert_eq!(params["node"], "helios");
        assert_eq!(params["shares"], "media, projekty");
        assert_eq!(params["targets"], "vm-store");
        for foreign in ["kadry", "bazy"] {
            assert!(!detail.text.contains(foreign) && params.values().all(|v| !v.contains(foreign)), "{foreign} is named");
        }
        assert!(super::plan(&db, "").unwrap().own_shares.is_empty());
    }

    /// The step lines of both jobs, keyed so no disk id can ever match one.
    #[test]
    fn the_step_lines_are_keyed_apart_from_disks() {
        let stop = step_lines(STOP_KIND);
        assert_eq!(stop.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(), vec!["shares", "targets", "disable"]);
        assert!(stop.iter().all(|(key, _)| key.starts_with("step:")));
        assert_eq!(step_lines(RESUME_KIND).len(), 2);
    }

    /// Sharing comes back only when TentaNas is ENABLED again.
    #[test]
    fn sharing_resumes_only_once_tentanas_is_enabled_again() {
        let (_dir, main) = main_db(false);
        let db = pool();
        assert_eq!(resume_wanted(&main, &db), None, "nothing is stopped");
        suspend_for_test(&db, PHASE_STOPPED);
        assert_eq!(resume_wanted(&main, &db), None, "stopped, but TentaNas is still disabled");
        main.write().unwrap().execute("UPDATE addons SET is_enabled = 1 WHERE addon_id = 'tentanas-w10'", []).unwrap();
        assert!(matches!(resume_wanted(&main, &db), Some((id, Due::Resume(_))) if id == "tentanas-w10"));
    }

    fn wait_for_job(db: &DbPool, kind: &str) -> tentaflow_protocol::tentanas::NasJob {
        for _ in 0..600 {
            if let Some(job) = store::list_jobs(db, 20).unwrap().into_iter().find(|j| j.kind == kind && j.status != "running") {
                return job;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the {kind} job did not finish");
    }

    /// Critic wave 10, M5: the REAL stop and resume on a real database —
    /// real share and target rows, the real share apply and target sweeps,
    /// the real disable of the platform row — with only the privilege
    /// channel recorded (`broker::test_channel`), so nothing reaches the
    /// host. A share that was serving and whose source is not on this node
    /// (no pool here) is recorded, does not come back, and is named; the
    /// stop and resume otherwise complete.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_real_stop_and_resume_run_end_to_end_on_a_real_database() {
        let db = pool();
        let (_dir, main) = main_db(true);
        let channel = super::super::broker::test_channel::install(&db);
        store::upsert_share(&db, "org-a", &share("s1", "projekty", "nfs", true)).unwrap();
        store::set_share_state(&db, "s1", "active", "", &[]).unwrap();
        store::upsert_share(&db, "org-a", &share("s2", "zepsuty", "smb", true)).unwrap();
        store::set_share_state(&db, "s2", "error", "source path is not mounted", &[]).unwrap();
        store::upsert_target(&db, "org-a", &target("t1", "vm-store", "iscsi", true)).unwrap();
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[7u8; 32]));
        let ops = NodeOps { main_db: main.clone(), db: db.clone(), addon_id: "tentanas-w10".into(), explicit: None, cipher: Some(cipher.clone()) };
        let plan = plan(&db, "org-a").unwrap();
        let body_db = db.clone();
        let stop = StopContext { request_id: "req-7".into(), requested_by: "u-anna".into(), approved_by: "u-piotr".into(), org_id: "org-a".into() };
        super::super::jobs::spawn_steps(&db, STOP_KIND, "helios", "u-piotr", Some("org-a"), move |h| async move {
            let _serial = super::super::shares::apply_mutex().lock().await;
            run_stop(&body_db, &ops, &h, &stop, &plan).await
        })
        .unwrap();
        let job = wait_for_job(&db, STOP_KIND);
        assert_eq!(job.status, "succeeded", "{:?}", job.error);
        let lines = store::job_disks(&db, &job.job_id).unwrap();
        assert!(lines.iter().all(|l| l.state == "done"), "{lines:?}");
        let m = mark(&db).unwrap().expect("stopped");
        assert_eq!((m.phase.as_str(), m.request_id.as_str()), (PHASE_STOPPED, "req-7"));
        assert_eq!(m.serving.shares, vec![row("s1", "projekty", "org-a")], "only the share that was serving");
        assert_eq!(crate::db::repository::get_addon_enabled(&main, "tentanas-w10").unwrap(), Some(false), "TentaNas disabled");
        let audited: String = main.read().unwrap().query_row(
            "SELECT details FROM audit_log WHERE action = 'addon_toggle' ORDER BY id DESC LIMIT 1", [], |r| r.get(0),
        ).unwrap();
        assert!(audited.contains("req-7") && audited.contains("u-anna"), "{audited}");
        let targets = store::list_targets(&db).unwrap();
        assert_eq!(targets[0].state, "suspended", "the real target judgement under the record");
        let shares = store::list_shares(&db).unwrap();
        assert!(shares.iter().all(|s| s.state != "active"), "no share is judged serving: {shares:?}");
        if let Some(exports) = channel.last_payload("nfs_exports_write") {
            assert!(!exports.contains("/tank/"), "no export while stopped: {exports}");
        }

        // Enable again: the restore tick / enable hook's pass resumes.
        crate::db::repository::set_addon_enabled(&main, "tentanas-w10", true).unwrap();
        let mut started = None;
        for _ in 0..200 {
            started = resume_if_due(&main, &db).await;
            if started.is_some() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(started.is_some(), "the resume started");
        let job = wait_for_job(&db, RESUME_KIND);
        assert_eq!(job.error.as_deref(), Some("refusal:sharing_resume_partial"), "the share with no pool here does not come back");
        assert!(!suspended(&db).unwrap(), "the record is gone: nothing else is held back");
        let alert = store::list_alerts(&db, true).unwrap().into_iter().find(|a| a.code == "sharing_share_not_resumed").expect("named alert");
        assert_eq!(alert.subject_id, "projekty");
        assert_eq!(store::list_targets(&db).unwrap()[0].state == "suspended", false, "the target is judged again");
        assert!(pending(&db).unwrap().is_some(), "the share is retried later, with a back-off");
        assert_eq!(resume_wanted(&main, &db), None, "and not before its back-off");
    }
}
