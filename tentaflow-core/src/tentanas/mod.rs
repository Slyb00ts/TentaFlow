// ===== File: tentanas/mod.rs — TentaNas, the storage application (plan-02) =====
//
// A native app on the app platform: one global instance, every node runs its
// own copy against its own disks and its own `tentanas.db`. The dashboard
// picks a node in the header and every request of the family is forwarded
// to it (`dispatch/app_route.rs`); the fleet list is the only request the
// dashboard's node answers itself.
//
// Layering:
//   broker        the only place a system command is executed
//   elevation     the node's privilege channel (helper or armed password)
//   environment   what the node can do (features, versions, package manager)
//   fleet         the node list of the header and each node's published summary
//   disks         inventory, live I/O, SMART, health, sampler
//   elastic       Elastic Array: one mergerfs union over cache + data disks,
//                 SnapRAID parity over the data disks, and the mover between
//   zfs           shared plumbing of the ZFS layer (tool lookup, -Hp parsing)
//   arc           the ARC counters and the cap the ARC slider writes
//   approvals     the four-eyes gate: red paths parked for a second admin
//   access_log    the file access audit: vfs_full_audit in, tentanas.db out
//   forward       the alert pipeline and the access log leaving the node
//   pools         zpool list/status/iostat, health, the layout wizard
//   rdma          the node's RDMA devices and whether NFS may use them
//   ksmbd         the second SMB backend: SMB Direct on RDMA interfaces only
//   datasets      zfs list/get for filesystems, zvols and their properties
//   snapshots     snapshot list, GFS retention, the automatic snapshot job
//   shares        SMB/NFS shares: config generation, apply, sessions, browser
//   sharing       stopping this node's shares and targets with the disable, and
//                 bringing them back on enable (n18d "zatrzymaj udostępnianie")
//   targets       iSCSI/NVMe-oF block export: configfs, CHAP, ALUA/ANA
//   fleet_mounts  the same share on every node, over NFS, without a secret
//   config_io     configuration export, import plan and import apply
//   scheduler     scrubs, automatic snapshots and SMART tests on a clock
//   keystore      encryption keys of native-ZFS datasets (outside the data dir)
//   jobs          long-running work with a persisted log
//   db            schema and rows of tentanas.db
//
// Uninstall NEVER destroys pools or user data: teardown takes the app's own
// configuration back out of smbd/nfsd, unmounts what it mounted, exports the
// pools cleanly so any system can import them again, and writes the node's
// configuration to the platform's backup directory before the instance
// directory is wiped (§5.8).

pub mod access_log;
pub mod approvals;
pub mod arc;
pub mod broker;
pub mod config_io;
pub mod datasets;
pub mod db;
pub mod disks;
pub mod elastic;
pub mod elevation;
pub mod environment;
pub mod fleet;
pub mod fleet_mounts;
pub mod forward;
pub mod jobs;
pub mod keystore;
pub mod ksmbd;
pub mod log_ids;
pub mod pools;
pub mod rdma;
pub mod scheduler;
pub mod shares;
pub mod sharing;
pub mod snapshots;
pub mod targets;
pub mod zfs;

use anyhow::Result;

use crate::addon::native_apps::{
    teardown_status, DisableConsequence, NativeAppContext, PublishedBlock, TeardownEntry, TeardownNodeInfo,
};
use crate::db::DbPool;

pub const PACKAGE_ID: &str = "tentanas";

/// A sentence the node says in two forms: codes with parameters, which a
/// screen words in the reader's language, and the node's own text — what the
/// log, the database and a tooltip carry. The pattern `store::AlertText`
/// follows for alerts, for the state details and the parked requests that
/// are not alerts (wave 6).
///
/// It reads as its text (`Deref<Target = str>`, `Display`, `== "…"`), so a
/// caller that only logs or compares the sentence does not change, and the
/// codes travel beside it to the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodedText {
    pub text: String,
    pub reasons: Vec<tentaflow_protocol::tentanas::NasHealthReason>,
}

impl CodedText {
    /// One coded sentence.
    pub fn new(code: &str, params: &[(&str, String)], text: impl Into<String>) -> Self {
        Self { text: text.into(), reasons: vec![disks::coded_reason(code, params)] }
    }

    /// A sentence nobody coded (the node stored it, or a tool wrote it): the
    /// screen has only the text to show.
    pub fn uncoded(text: impl Into<String>) -> Self {
        Self { text: text.into(), reasons: Vec::new() }
    }

    /// Two parts of one detail, the way the sentences were always joined
    /// (" · "); an empty part adds nothing.
    pub fn and(mut self, other: CodedText) -> Self {
        if other.text.is_empty() && other.reasons.is_empty() {
            return self;
        }
        if self.text.is_empty() {
            self.text = other.text;
        } else if !other.text.is_empty() {
            self.text = format!("{} · {}", self.text, other.text);
        }
        self.reasons.extend(other.reasons);
        self
    }
}

impl From<&str> for CodedText {
    fn from(text: &str) -> Self {
        Self::uncoded(text)
    }
}

impl From<String> for CodedText {
    fn from(text: String) -> Self {
        Self::uncoded(text)
    }
}

impl From<&String> for CodedText {
    fn from(text: &String) -> Self {
        Self::uncoded(text.as_str())
    }
}

impl std::ops::Deref for CodedText {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for CodedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl PartialEq<&str> for CodedText {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<str> for CodedText {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<String> for CodedText {
    fn eq(&self, other: &String) -> bool {
        &self.text == other
    }
}

impl PartialEq<CodedText> for String {
    fn eq(&self, other: &CodedText) -> bool {
        *self == other.text
    }
}

impl PartialEq<CodedText> for &str {
    fn eq(&self, other: &CodedText) -> bool {
        *self == other.text
    }
}

/// The instance database, opened on first use.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// Whether the node's unattended loops (schedules, mount reconcile) may act.
///
/// §5.8: disabling the app hides the tile and closes the API, but it must NOT
/// cut production storage — services keep serving and the schedules keep
/// running. Schedules run unattended, which only mode A can do, so a disabled
/// instance keeps its loops alive exactly while the passwordless channel
/// exists. An uninstalled instance stops them for good.
pub fn instance_should_run(main_db: &DbPool, db: &DbPool) -> bool {
    match crate::db::repository::get_package_instance(main_db, PACKAGE_ID) {
        Ok(Some((_, true))) => true,
        Ok(Some((_, false))) => elevation::mode(db) == elevation::Mode::Helper,
        _ => false,
    }
}

/// Native init hook: schema, orphaned jobs, the sampler and the two loops.
/// Idempotent — reconcile calls it again on every boot and enable.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    let pool = open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    let orphaned = db::fail_orphaned_jobs(&pool)?;
    // A sharing stop or resume that died with the process (wave 10): its
    // record is `stopped` again, and resumed once TentaNas is enabled.
    sharing::recover_after_restart(&pool)?;
    if orphaned > 0 {
        tracing::info!("tentanas: marked {orphaned} interrupted jobs as failed");
    }
    disks::start_sampler(ctx.db.clone(), ctx.addon_id.to_string(), pool.clone());
    // The Elastic restore queue is registered BEFORE the scheduler starts: its
    // Elastic passes wait for the queue, and they can only see one that
    // already exists.
    elastic::start_restore(ctx.db.clone(), pool.clone(), tentanas_helper::elastic::ElasticOwner {
        org_id: ctx.org_id.to_string(), addon_id: ctx.addon_id.to_string(),
    });
    scheduler::start(ctx.db.clone(), pool.clone());
    // configfs is empty after a reboot, so this is what puts the block targets
    // back — the only thing that does (§3.4, §5.5).
    targets::start_restore(ctx.db.clone(), pool.clone());
    fleet_mounts::start(ctx.db.clone(), ctx.addon_id.to_string(), pool);
    tracing::info!(
        "native app '{}': TentaNas initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

/// Native enable hook (wave 10): TentaNas was switched on — by the admin
/// here, by a replicated enable, or at start as an enabled instance. When
/// this node's sharing was stopped with the disable (n18d "Wyłącz i zatrzymaj
/// udostępnianie…"), it is brought back now (`sharing::resume_if_due`): the
/// shares and targets serve again. Without a privilege channel (mode B, not
/// armed) nothing can be re-applied yet; the target restore loop resumes the
/// moment an admin arms one, and the shares and targets read "stopped" until
/// then.
pub fn native_on_enable(ctx: &NativeAppContext) -> Result<()> {
    let pool = open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    if !sharing::suspended(&pool)? {
        return Ok(());
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("tentanas: no tokio runtime, sharing is resumed by the restore loop");
        return Ok(());
    };
    let main_db = ctx.db.clone();
    handle.spawn(async move {
        if let Some(job) = sharing::resume_if_due(&main_db, &pool).await {
            tracing::info!("tentanas: sharing resumes on this node (job {})", job.job_id);
        }
    });
    Ok(())
}

/// The file-keyed rows of the teardown plan, in the order §5.8 fixes.
/// `present` is passed in so the ORDER is testable without the node actually
/// having any of these files.
///
/// ksmbd comes FIRST, before the Samba include: it is the server holding TCP
/// 445 on the RDMA interfaces, and removing the include is what gives those
/// interfaces back to smbd. The other way round smbd would try to bind a port
/// the second server still owns.
fn config_teardown_entries(present: &dyn Fn(&str) -> bool) -> Vec<TeardownEntry> {
    let rows: [(&'static str, &'static str, &'static str); 9] = [
        (
            tentanas_helper::KSMBD_CONF_PATH,
            "tentanas_ksmbd_config",
            "app-owned ksmbd config serving SMB Direct on the RDMA interfaces (the service is stopped with it)",
        ),
        (
            tentanas_helper::SMB_INCLUDE_PATH,
            "tentanas_smb_config",
            "app-owned SMB share sections and the include line in smb.conf",
        ),
        (
            tentanas_helper::NFS_EXPORTS_PATH,
            "tentanas_nfs_exports",
            "app-owned NFS exports (the shared data itself is untouched)",
        ),
        // After the exports: the watches are on the paths those exports named,
        // so they come off once nothing exports them any more (§5.10).
        (
            tentanas_helper::AUDIT_RULES_PATH,
            "tentanas_audit_rules",
            "app-owned auditd watches on the audited NFS export paths (the host's audit log is kept)",
        ),
        // The block exports (§5.5). Keyed on the configfs roots because that
        // is where the state IS: a target has no file on disk, which is the
        // same fact that makes this app the only thing able to restore one.
        // An uninstall that skipped this would leave a client holding a raw
        // disk from a node that no longer manages it.
        (
            tentanas_helper::block::TARGET_CONFIGFS,
            "tentanas_iscsi_targets",
            "app-created iSCSI targets and their backstores (removed from the kernel; the zvols and their data stay)",
        ),
        (
            tentanas_helper::block::NVMET_CONFIGFS,
            "tentanas_nvmet_targets",
            "app-created NVMe-oF subsystems (removed from the kernel; the zvols and their data stay)",
        ),
        (
            tentanas_helper::NFS_CONF_PATH,
            "tentanas_nfs_conf",
            "app-owned NFS server drop-in enabling the RDMA transport (the listener is closed with it)",
        ),
        (
            tentanas_helper::ARC_MODPROBE_PATH,
            "tentanas_arc_limit",
            "app-owned modprobe drop-in holding the ARC limit (the running cap stays until reboot)",
        ),
        (
            shares::MOUNT_ROOT,
            "tentanas_fleet_mounts",
            "fleet mounts of other nodes' shares (unmounted; remote data untouched)",
        ),
    ];
    rows.into_iter()
        .filter(|(path, _, _)| present(path))
        .map(|(path, kind, description)| TeardownEntry {
            path: path.into(),
            kind,
            description: description.into(),
            removed: true,
            ..Default::default()
        })
        .collect()
}

/// Teardown plan (§5.8): every step of the uninstall as one row, with the
/// `removed` flag the dialog reads. The keystore, the configuration backup and
/// — above all — the pools and their data are KEPT; the helper + sudoers line
/// are listed as left behind because removing them needs a fresh sudo password
/// the Environment tab collects through `ElevationRemoveRequest`. Pure: the
/// uninstall dialog calls it on every open.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    // The two block rows are keyed on the app HAVING targets, not on the
    // configfs tree existing: a node with LIO loaded for something else and no
    // target of ours must not be promised the removal of something that is not
    // there. Every other row is a file this app owns, so its presence is the
    // question.
    let db = open_db(ctx.db, ctx.org_id, ctx.addon_id).ok();
    let targets = db.as_ref().and_then(|db| db::list_targets(db).ok()).unwrap_or_default();
    let shares = db.as_ref().and_then(|db| db::list_shares(db).ok()).unwrap_or_default();
    let has = |protocol: &str| targets.iter().any(|t| t.protocol == protocol);
    let mut entries = config_teardown_entries(&|path| match path {
        p if p == tentanas_helper::block::TARGET_CONFIGFS => has("iscsi"),
        p if p == tentanas_helper::block::NVMET_CONFIGFS => has("nvmet"),
        p => std::path::Path::new(p).exists(),
    });
    // The rows that take shares and targets out say HOW MANY this node has
    // (n18a: "usuń udostępnienia (2× SMB, 2× NFS, iSCSI, NVMe-oF)"). Counts
    // only: the admin uninstalling reads the whole node, and another
    // organisation's share is never named in the plan.
    let shares_of = |protocol: &str| shares.iter().filter(|s| s.protocol == protocol).count() as i64;
    let targets_of = |protocol: &str| targets.iter().filter(|t| t.protocol == protocol).count() as i64;
    for entry in entries.iter_mut() {
        let n = match entry.kind {
            "tentanas_smb_config" => shares_of("smb"),
            "tentanas_nfs_exports" => shares_of("nfs"),
            "tentanas_iscsi_targets" => targets_of("iscsi"),
            "tentanas_nvmet_targets" => targets_of("nvmet"),
            _ => continue,
        };
        entry.count_vars.insert("n".to_string(), n);
    }
    // The teardown REFUSES while Elastic Arrays are under this instance's
    // supervision (`db::block_elastic_teardown`): removing the instance would
    // lose their configuration and their supervision. Said up front, for this
    // node, so the dialog does not offer an uninstall the node will refuse.
    if let Some(block) = db.as_ref().and_then(teardown_blocker) {
        entries.push(block);
    }
    // The pools are exported, never destroyed — the whole point of §5.8. The
    // row exists so the dialog can say so out loud.
    entries.push(TeardownEntry {
        path: tentanas_helper::MOUNT_ROOT.into(),
        kind: "tentanas_pools",
        description: "ZFS pools are exported cleanly, never destroyed: the data stays on the disks".into(),
        removed: false,
        ..Default::default()
    });
    entries.push(TeardownEntry {
        path: crate::paths::tentaflow_home().join("app-backups"),
        kind: "tentanas_config_backup",
        description: "configuration export written before the wipe (kept)".into(),
        removed: false,
        ..Default::default()
    });
    entries.push(TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "tentanas_data_dir",
        description: "instance data directory (tentanas.db: disk history, alerts, jobs, shares)".into(),
        removed: true,
        ..Default::default()
    });
    // The keystore lives outside the data dir precisely so this wipe cannot
    // reach it: the encrypted datasets stay on the pools, so their keys must
    // stay too, and deleting them is a separate deliberate act.
    let keystore = keystore::store_path(ctx.addon_id);
    if keystore.exists() {
        entries.push(TeardownEntry {
            path: keystore,
            kind: "tentanas_keystore",
            description: "ZFS dataset encryption keys (kept: the datasets survive uninstall)".into(),
            removed: false,
            ..Default::default()
        });
    }
    if std::path::Path::new(tentanas_helper::HELPER_INSTALL_PATH).exists() {
        entries.push(TeardownEntry {
            path: tentanas_helper::HELPER_INSTALL_PATH.into(),
            kind: "tentanas_helper",
            description: "privilege helper (remove with a sudo password from the Environment tab)".into(),
            removed: false,
            ..Default::default()
        });
    }
    if std::path::Path::new(tentanas_helper::SUDOERS_INSTALL_PATH).exists() {
        entries.push(TeardownEntry {
            path: tentanas_helper::SUDOERS_INSTALL_PATH.into(),
            kind: "tentanas_sudoers",
            description: "sudoers rule for the privilege helper".into(),
            removed: false,
            ..Default::default()
        });
    }
    Ok(entries)
}

/// The plan entry that makes this node's teardown refuse, if any: Elastic
/// Arrays under this instance's supervision (`db::block_elastic_teardown`
/// refuses on the same table).
fn teardown_blocker(db: &DbPool) -> Option<TeardownEntry> {
    let arrays = db::elastic_array_owners(db).map(|a| a.len()).unwrap_or(0);
    if arrays == 0 {
        return None;
    }
    let mut count_vars = std::collections::BTreeMap::new();
    count_vars.insert("n".to_string(), arrays as i64);
    Some(TeardownEntry {
        path: tentanas_helper::MOUNT_ROOT.into(),
        kind: "tentanas_elastic_arrays",
        description: format!("{arrays} Elastic Array(s) under this instance's supervision — dissolve them first").into(),
        removed: false,
        blocks: true,
        count_vars,
    })
}

/// Publishes what this node's teardown would refuse (the platform's
/// `record_teardown_blocks`), so an uninstall started on another node is
/// refused before it replicates while this one is offline. A node whose
/// database cannot be read publishes nothing new — the last record stands.
pub fn publish_teardown_blocks(main_db: &DbPool, addon_id: &str, db: &DbPool) {
    if db::elastic_array_owners(db).is_err() {
        return;
    }
    let blocks: Vec<PublishedBlock> = teardown_blocker(db)
        .into_iter()
        .map(|e| PublishedBlock { kind: e.kind.to_string(), count_vars: e.count_vars })
        .collect();
    crate::addon::native_apps::record_teardown_blocks(main_db, addon_id, &blocks);
}

/// n18a's mode chip and backup column: mode A (the helper) tears down
/// unattended; anything else needs the admin's sudo password on that node
/// first (`native_arm_teardown`), or its pools stay imported. The backup is
/// the configuration export `config_io::write_backup` writes, named after
/// the host (`config_io::filename`).
pub fn native_teardown_node_info(ctx: &NativeAppContext) -> TeardownNodeInfo {
    let helper = open_db(ctx.db, ctx.org_id, ctx.addon_id)
        .map(|db| elevation::mode(&db) == elevation::Mode::Helper)
        .unwrap_or(false);
    let host = crate::mesh::node_info_collector::local_hostname();
    let host: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    TeardownNodeInfo {
        privilege: if helper { "helper" } else { "password" },
        // Without a host name the file would be named after the node id: the
        // column then says only that a backup is written.
        backup_file: if host.is_empty() || host == "unknown" {
            String::new()
        } else {
            format!("app-backups/tentanas-{host}-….json")
        },
    }
}

/// How long a password given for a teardown is held: long enough for the
/// removal to reach this node and for its pools to export. A dialog closed in
/// a way that sends no disarm (a lost browser tab) cannot leave it longer.
const TEARDOWN_ARM_SECS: u64 = 15 * 60;

/// The mode-B teardown password (n18a), held for ONE purpose (wave-9b critic,
/// round 2, MAJOR B): only `native_teardown` of the same instance takes it,
/// and taking it consumes it. It is NOT the node's armed elevation slot —
/// no other privileged request, no schedule and no loop can use it — and
/// arming it never changes the node's stored privilege mode.
pub(crate) mod teardown_hold {
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use crate::profiling::collectors::elevation::ElevationToken;

    struct Held {
        addon_id: String,
        token: Arc<ElevationToken>,
        until: Instant,
    }

    fn slot() -> &'static Mutex<Option<Held>> {
        static SLOT: OnceLock<Mutex<Option<Held>>> = OnceLock::new();
        SLOT.get_or_init(|| Mutex::new(None))
    }

    /// Holds `token` for the teardown of `addon_id`, replacing any earlier one.
    pub fn put(addon_id: &str, token: Arc<ElevationToken>, ttl: Duration) {
        *slot().lock().unwrap_or_else(|p| p.into_inner()) =
            Some(Held { addon_id: addon_id.to_string(), token, until: Instant::now() + ttl });
    }

    /// Takes the token for `addon_id` — once. An expired one, or one held
    /// for another instance, is not handed out (an expired one is dropped).
    pub fn take(addon_id: &str) -> Option<Arc<ElevationToken>> {
        let mut guard = slot().lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(h) if h.addon_id == addon_id && h.until > Instant::now() => guard.take().map(|h| h.token),
            Some(h) if h.until <= Instant::now() => {
                *guard = None;
                None
            }
            _ => None,
        }
    }

    /// Drops whatever is held for `addon_id` (every exit before the teardown).
    pub fn drop_for(addon_id: &str) {
        let mut guard = slot().lock().unwrap_or_else(|p| p.into_inner());
        if guard.as_ref().is_some_and(|h| h.addon_id == addon_id) {
            *guard = None;
        }
    }

    /// Whether a token is held for `addon_id` (tests and the plan).
    pub fn held(addon_id: &str) -> bool {
        slot()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|h| h.addon_id == addon_id && h.until > Instant::now())
    }
}

/// Holds the admin's sudo password for THIS instance's teardown (n18a, mode
/// B): validated by sudo first, then kept in `teardown_hold` — consumed by
/// `native_teardown`, dropped by `native_disarm_teardown` or after
/// `TEARDOWN_ARM_SECS`. The node's elevation mode and its armed slot are not
/// touched. Returns until when (RFC 3339).
pub fn native_arm_teardown(ctx: &NativeAppContext, password: String) -> Result<String> {
    let db = open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    if elevation::mode(&db) == elevation::Mode::Helper {
        return Ok(String::new());
    }
    let token = std::sync::Arc::new(crate::profiling::collectors::elevation::ElevationToken::new_sudo(password));
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("no runtime to validate the password on"))?;
    tokio::task::block_in_place(|| handle.block_on(crate::profiling::elevation_runner::ElevationRunner::validate_sudo(&token)))
        .map_err(|e| anyhow::anyhow!("sudo validation failed: {e}"))?;
    teardown_hold::put(ctx.addon_id, token, std::time::Duration::from_secs(TEARDOWN_ARM_SECS));
    Ok((chrono::Utc::now() + chrono::Duration::seconds(TEARDOWN_ARM_SECS as i64))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// Drops the teardown password (`teardown_hold`) the flow did not use.
pub fn native_disarm_teardown(ctx: &NativeAppContext) {
    teardown_hold::drop_for(ctx.addon_id);
}

/// Native teardown hook (§5.8, in order): stop the loops and the jobs, take
/// the app's config back out of smbd/nfsd and unmount what it mounted, export
/// the pools cleanly, and write the configuration backup outside the instance
/// directory the platform is about to remove.
pub fn native_teardown(ctx: &NativeAppContext) -> Result<()> {
    // Taken FIRST, whatever follows: every way out of this hook — a refusal,
    // an unreadable database, the end — drops the password with `hold`
    // (wave-9b critic, round 2, MAJOR B).
    let hold = teardown_hold::take(ctx.addon_id);
    let pool=open_db(ctx.db,ctx.org_id,ctx.addon_id)?;
    teardown_status::phase(ctx.addon_id, "tentanas_elastic_check");
    db::block_elastic_teardown(&pool)?;
    teardown_status::phase(ctx.addon_id, "tentanas_stop");
    scheduler::stop();
    // Before anything else: the restore loop would put a target back into the
    // kernel half a second after the teardown took it out.
    targets::stop();
    fleet_mounts::stop();
    let cancelled = jobs::cancel_all();
    if cancelled > 0 {
        tracing::info!("tentanas teardown: cancelled {cancelled} running jobs");
    }
    match open_db(ctx.db, ctx.org_id, ctx.addon_id) {
        Ok(db) => match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                // The privileged steps are async and the hook is not; the
                // uninstall is a rare, deliberate action, so blocking this
                // worker for the length of it is the honest trade.
                let addon_id = ctx.addon_id;
                let explicit = hold.as_deref();
                tokio::task::block_in_place(|| handle.block_on(teardown_steps(&db, addon_id, explicit)));
            }
            Err(_) => {
                tracing::warn!("tentanas teardown: no tokio runtime, services and pools left as they are");
                teardown_status::warn(ctx.addon_id, "tentanas_services_left");
                teardown_status::warn(ctx.addon_id, "tentanas_pools_not_exported");
            }
        },
        Err(e) => {
            tracing::warn!("tentanas teardown: instance database unreadable: {e}");
            teardown_status::warn(ctx.addon_id, "tentanas_services_left");
            teardown_status::warn(ctx.addon_id, "tentanas_pools_not_exported");
        }
    }
    elevation::disarm();
    crate::addon::app_db::close(ctx.addon_id);
    Ok(())
}

/// The privileged half of the teardown. Each step reports itself to the
/// uninstall dialog (`teardown_status::phase`), and what it could not do —
/// pools left imported, a backup not written — is reported as a warning, so
/// the node's row does not read a plain "done" over pools that stayed.
async fn teardown_steps(
    db: &DbPool,
    addon_id: &str,
    explicit: Option<&crate::profiling::collectors::elevation::ElevationToken>,
) {
    // The configuration document is READ first and written last: after the
    // pools are exported `zpool list` shows nothing, and a backup without the
    // pool layouts is exactly the one thing an admin would need it for.
    let document = config_io::export(db).await;
    teardown_status::phase(addon_id, "tentanas_services");

    for line in shares::remove_all(db, explicit).await {
        tracing::info!("tentanas teardown: {line}");
    }
    // §5.8 step 2, the block half: a target left in the kernel would keep
    // handing a client a raw disk from a node that no longer manages it, and
    // no file on disk would say so.
    for line in targets::remove_all(db, explicit).await {
        tracing::info!("tentanas teardown: {line}");
    }
    for line in fleet_mounts::unmount_all(db, explicit).await {
        tracing::info!("tentanas teardown: {line}");
    }

    // The ARC drop-in is the app's file and the teardown plan promises to take
    // it out; it goes before the pools, while the channel is still needed for
    // the export anyway.
    if std::path::Path::new(tentanas_helper::ARC_MODPROBE_PATH).exists()
        && (explicit.is_some() || broker::channel_available(db).await)
    {
        let command = tentanas_helper::HelperCommand::ArcLimitClear {};
        match broker::run_privileged(db, &command, explicit, std::time::Duration::from_secs(30)).await {
            Ok((out, _)) if out.success() => {
                tracing::info!("tentanas teardown: ARC modprobe drop-in removed")
            }
            Ok((out, _)) => tracing::warn!(
                "tentanas teardown: ARC drop-in not removed: {}",
                out.stderr.trim()
            ),
            Err(e) => tracing::warn!("tentanas teardown: ARC drop-in not removed: {e}"),
        }
    }

    // §5.8 step 3: a clean export leaves the pools importable by anything —
    // a fresh TentaNas, TrueNAS or a plain `zpool import`. Without a privilege
    // channel nothing is done at all: half-exported pools would be worse than
    // pools that are simply still imported.
    teardown_status::phase(addon_id, "tentanas_pools");
    if explicit.is_some() || broker::channel_available(db).await {
        for pool in pools::list_rows().await.unwrap_or_default() {
            let command = tentanas_helper::HelperCommand::ZpoolExport {
                pool: pool.name.clone(),
                force: false,
            };
            match broker::run_privileged(db, &command, explicit, std::time::Duration::from_secs(300))
                .await
            {
                Ok((out, _)) if out.success() => {
                    tracing::info!("tentanas teardown: pool {} exported", pool.name)
                }
                Ok((out, _)) => {
                    tracing::warn!(
                        "tentanas teardown: pool {} not exported: {}",
                        pool.name,
                        out.stderr.trim()
                    );
                    teardown_status::warn(addon_id, "tentanas_pools_not_exported");
                }
                Err(e) => {
                    tracing::warn!("tentanas teardown: pool {} not exported: {e}", pool.name);
                    teardown_status::warn(addon_id, "tentanas_pools_not_exported");
                }
            }
        }
    } else {
        tracing::warn!(
            "tentanas teardown: no privilege channel — the pools stay imported and mounted"
        );
        if !db::known_pools(db).unwrap_or_default().is_empty() {
            teardown_status::warn(addon_id, "tentanas_pools_not_exported");
        }
    }

    teardown_status::phase(addon_id, "tentanas_backup");
    match document.and_then(|d| config_io::write_backup(&d)) {
        Ok(path) => tracing::info!("tentanas teardown: configuration saved to {}", path.display()),
        Err(e) => {
            tracing::warn!("tentanas teardown: configuration backup failed: {e}");
            teardown_status::warn(addon_id, "tentanas_backup_failed");
        }
    }
}

/// What disabling TentaNas does on THIS node (n18d), from its real state.
///
/// §5.8: disabling closes the panel and the API; the storage keeps serving.
/// What else keeps running depends on the privilege channel, exactly as
/// `instance_should_run` decides it: the unattended loops (schedules, the
/// mover, the target restore after a reboot, alert forwarding, the access
/// log, fleet mounts) go on only in mode A (the helper), because nothing else
/// can act without a person present. In mode B they stop with the switch.
///
/// Names: pools are the node's hardware and every admin sees them; an
/// Elastic Array belongs to an organisation, so only the asking
/// organisation's arrays are named and the others are counted.
pub fn native_disable_consequences(ctx: &NativeAppContext, viewer_org: &str) -> Result<Vec<DisableConsequence>> {
    let db = open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    let running = jobs::running().lock().unwrap_or_else(|p| p.into_inner()).len() as i64;
    disable_consequences_of(&db, viewer_org, running)
}

/// `native_disable_consequences` over the node database itself, with the
/// number of jobs this process runs.
fn disable_consequences_of(db: &DbPool, viewer_org: &str, running: i64) -> Result<Vec<DisableConsequence>> {
    let db = db.clone();
    let helper = elevation::mode(&db) == elevation::Mode::Helper;
    let counts = |pairs: &[(&str, i64)]| -> std::collections::BTreeMap<String, i64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    };
    let mut out = vec![DisableConsequence {
        kind: "tentanas_api_closed",
        effect: "stops",
        ..Default::default()
    }];

    let shares = db::list_shares(&db)?;
    let serving = |protocol: &str| shares.iter().filter(|s| s.enabled && s.protocol == protocol).count() as i64;
    // One sentence per protocol, and none for a protocol with nothing on
    // it: "0 udziałów SMB" is not a consequence (critic wave 9b, MINOR 4).
    for (kind, n) in [("tentanas_smb_shares_continue", serving("smb")), ("tentanas_nfs_shares_continue", serving("nfs"))] {
        if n > 0 {
            out.push(DisableConsequence { kind, effect: "continues", count_vars: counts(&[("n", n)]), ..Default::default() });
        }
    }

    let targets = db::list_targets(&db)?;
    let exported = |protocol: &str| targets.iter().filter(|t| t.enabled && t.protocol == protocol).count() as i64;
    // configfs is empty after a reboot and only the restore loop puts the
    // targets back (`targets::start_restore`) — which runs only while the
    // instance should run.
    let targets_kind = |iscsi: bool| match (iscsi, helper) {
        (true, true) => "tentanas_iscsi_targets_continue",
        (true, false) => "tentanas_iscsi_targets_until_reboot",
        (false, true) => "tentanas_nvmet_targets_continue",
        (false, false) => "tentanas_nvmet_targets_until_reboot",
    };
    for (iscsi, n) in [(true, exported("iscsi")), (false, exported("nvmet"))] {
        if n > 0 {
            out.push(DisableConsequence { kind: targets_kind(iscsi), effect: "continues", count_vars: counts(&[("n", n)]), ..Default::default() });
        }
    }

    let enabled_pool_tasks = |task: db::PoolTask| {
        db::list_pool_schedules(&db, task).map(|rows| rows.iter().filter(|r| r.enabled).count() as i64)
    };
    let scrubs = enabled_pool_tasks(db::PoolTask::Scrub)? + enabled_pool_tasks(db::PoolTask::Trim)?;
    let snapshots = db::list_snapshot_schedules(&db)?.iter().filter(|s| s.enabled).count() as i64;
    let smart = i64::from(db::smart_schedule(&db)?.enabled);
    let parity = [db::ElasticTask::Sync, db::ElasticTask::Scrub]
        .into_iter()
        .map(|task| db::list_elastic_schedules(&db, task).map(|rows| rows.iter().filter(|r| r.enabled).count() as i64))
        .sum::<Result<i64>>()?;
    let schedules = scrubs + snapshots + smart + parity;
    if schedules > 0 {
        out.push(DisableConsequence {
            kind: if helper { "tentanas_schedules_continue" } else { "tentanas_schedules_stop" },
            effect: if helper { "continues" } else { "stops" },
            count_vars: counts(&[("n", schedules), ("scrub", scrubs), ("snapshot", snapshots), ("smart", smart), ("parity", parity)]),
            ..Default::default()
        });
    }
    // The loops with no schedule of their own: alert forwarding, the access
    // log collector, the automatic mover, fleet mounts.
    out.push(DisableConsequence {
        kind: if helper { "tentanas_background_continue" } else { "tentanas_background_stop" },
        effect: if helper { "continues" } else { "stops" },
        ..Default::default()
    });

    let mounted: Vec<(String, String, String)> =
        db::elastic_array_owners(&db)?.into_iter().filter(|(_, _, state)| state == "active").collect();
    if !mounted.is_empty() {
        let names: Vec<String> = mounted
            .iter()
            .filter(|(_, org, _)| !viewer_org.is_empty() && org == viewer_org)
            .map(|(name, _, _)| name.clone())
            .collect();
        let others = mounted.len() as i64 - names.len() as i64;
        if !names.is_empty() {
            out.push(DisableConsequence {
                kind: "tentanas_arrays_mounted",
                effect: "kept",
                count_vars: counts(&[("n", names.len() as i64)]),
                names,
            });
        }
        // Another organisation's arrays are counted, never named.
        if others > 0 {
            out.push(DisableConsequence {
                kind: "tentanas_arrays_mounted_other",
                effect: "kept",
                count_vars: counts(&[("n", others)]),
                ..Default::default()
            });
        }
    }
    let mut pools: Vec<String> = db::known_pools(&db)?.into_iter().map(|(name, _)| name).collect();
    pools.sort();
    if !pools.is_empty() {
        out.push(DisableConsequence {
            kind: "tentanas_pools_imported",
            effect: "kept",
            count_vars: counts(&[("n", pools.len() as i64)]),
            names: pools,
        });
    }
    if running > 0 {
        out.push(DisableConsequence {
            kind: "tentanas_jobs_continue",
            effect: "continues",
            count_vars: counts(&[("n", running)]),
            ..Default::default()
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ksmbd_is_torn_down_before_the_samba_include() {
        let all = config_teardown_entries(&|_| true);
        let kinds: Vec<&str> = all.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                "tentanas_ksmbd_config",
                "tentanas_smb_config",
                "tentanas_nfs_exports",
                "tentanas_audit_rules",
                "tentanas_iscsi_targets",
                "tentanas_nvmet_targets",
                "tentanas_nfs_conf",
                "tentanas_arc_limit",
                "tentanas_fleet_mounts",
            ]
        );
        // Every file-keyed row is removed; nothing here is a "kept" note.
        assert!(all.iter().all(|e| e.removed));
        assert_eq!(
            all[0].path,
            std::path::PathBuf::from(tentanas_helper::KSMBD_CONF_PATH)
        );

        // A node that never served SMB Direct has no ksmbd row at all, and the
        // rest keeps its order.
        let without = config_teardown_entries(&|path| path != tentanas_helper::KSMBD_CONF_PATH);
        assert_eq!(without.first().map(|e| e.kind), Some("tentanas_smb_config"));
        assert!(config_teardown_entries(&|_| false).is_empty());
    }

    /// Every code a `CodedText` of the node is built with, read from the
    /// source: `CodedText::new("<code>"` and `coded_reason("<code>"` calls.
    fn codes_in(source: &str, call: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut rest = source;
        while let Some(at) = rest.find(call) {
            rest = &rest[at + call.len()..];
            let trimmed = rest.trim_start();
            if let Some(quoted) = trimmed.strip_prefix('"') {
                if let Some(end) = quoted.find('"') {
                    out.insert(quoted[..end].to_string());
                }
            }
        }
        out
    }

    fn read(path: &str) -> String {
        std::fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))).expect(path)
    }

    /// Wave 6: a coded sentence the node sends and no screen words would
    /// fall back to the node's English on every screen, silently. Every code
    /// the node builds for the Elastic state, a target's state, the kernel
    /// support and a parked request has an entry in the screen that words it
    /// (`['<code>', …]` in its word table).
    #[test]
    fn every_coded_sentence_the_node_sends_has_words_on_its_screen() {
        let cases = [
            (
                "src/tentanas/elastic.rs",
                "CodedText::new(",
                vec!["www/js/modules/tentanas/elastic-detail.js", "www/js/modules/tentanas/feature-words.js"],
            ),
            (
                "src/tentanas/targets.rs",
                "CodedText::new(",
                vec![
                    "www/js/modules/tentanas/targets.js",
                    "www/js/modules/tentanas/target-wizard.js",
                    "www/js/modules/tentanas/feature-words.js",
                ],
            ),
            // Wave 7: the Environment rows (`NasEnvironment::feature_reasons`).
            ("src/tentanas/environment.rs", "CodedText::new(", vec!["www/js/modules/tentanas/feature-words.js"]),
            ("src/tentanas/rdma.rs", "CodedText::new(", vec!["www/js/modules/tentanas/feature-words.js"]),
            ("src/tentanas/ksmbd.rs", "CodedText::new(", vec!["www/js/modules/tentanas/feature-words.js"]),
            ("src/dispatch/tentanas.rs", "CodedText::new(", vec!["www/js/modules/tentanas/approvals.js"]),
        ];
        for (rust, call, screens) in cases {
            let codes = codes_in(&read(rust), call);
            assert!(codes.len() >= 5, "{rust}: the scan found {codes:?}");
            let words: String = screens.iter().map(|s| read(s)).collect();
            for code in &codes {
                assert!(words.contains(&format!("['{code}',")), "{rust} sends '{code}', and {screens:?} have no words for it");
            }
        }
        // The folder usage reasons (n11 "Użycie") are bare reasons too.
        let elastic_js = read("www/js/modules/tentanas/elastic-detail.js");
        let usage: Vec<String> = codes_in(&read("src/tentanas/elastic.rs"), "coded_reason(")
            .into_iter()
            .filter(|code| code.starts_with("folder_usage_"))
            .collect();
        assert_eq!(usage.len(), 6, "the scan found {usage:?}");
        for code in &usage {
            assert!(elastic_js.contains(&format!("['{code}',")), "elastic.rs sends '{code}', and elastic-detail.js has no words for it");
        }
        // The config import's target reasons and the rdma reasons are built
        // as bare reasons.
        let targets_js = read("www/js/modules/tentanas/targets.js") + &read("www/js/modules/tentanas/target-wizard.js");
        for rust in ["src/tentanas/config_io.rs", "src/tentanas/targets.rs"] {
            for code in codes_in(&read(rust), "coded_reason(") {
                assert!(targets_js.contains(&format!("['{code}',")), "{rust} sends '{code}' without words");
            }
        }
        // The codes the database writes with an array's stored sentence.
        let elastic_js = read("www/js/modules/tentanas/elastic-detail.js");
        for code in ["operation_failed", "supervision_lost"] {
            assert!(read("src/tentanas/db.rs").contains(code), "db.rs no longer writes '{code}'");
            assert!(elastic_js.contains(&format!("['{code}',")), "the array screens have no words for '{code}'");
        }
        // The Elastic preview's warnings.
        let wizard = read("www/js/modules/tentanas/pool-wizard.js");
        let elastic = read("src/tentanas/elastic.rs");
        let start = elastic.find("pub fn layout_warning_codes").expect("the warning codes");
        let end = start + elastic[start..].find("\n}\n").expect("its end");
        for code in codes_in(&elastic[start..end], "coded_reason(") {
            assert!(wizard.contains(&format!("['{code}',")), "the preview has no words for '{code}'");
        }
    }

    /// n18d: what disabling does on this node comes from the node's real
    /// state, and it depends on the privilege channel exactly as
    /// `instance_should_run` does — mode B stops the schedules and leaves
    /// the targets to the next reboot, mode A keeps both. The asking
    /// organisation's arrays are named, another organisation's only counted.
    #[test]
    fn disabling_names_the_real_consequences_on_this_node() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        db::migrate(&conn).expect("migrate");
        let pool: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        pool.write().expect("write").execute_batch(
            "INSERT INTO nas_shares (share_id,name,protocol,source_path,enabled,created_at,updated_at,org_id) VALUES
               ('s1','projekty','smb','/mnt/tank/projekty',1,'2026-09-01T00:00:00Z','2026-09-01T00:00:00Z','org-a'),
               ('s2','kadry','smb','/mnt/tank/kadry',1,'2026-09-01T00:00:00Z','2026-09-01T00:00:00Z','org-b'),
               ('s3','media','nfs','/mnt/tank/media',1,'2026-09-01T00:00:00Z','2026-09-01T00:00:00Z','org-a'),
               ('s4','stare','smb','/mnt/tank/stare',0,'2026-09-01T00:00:00Z','2026-09-01T00:00:00Z','org-a');
             INSERT INTO nas_targets (target_id,name,protocol,wwn,enabled,created_at,updated_at) VALUES
               ('t1','vm-store','iscsi','iqn.2026-09.pl.euvic:helios.vm-store',1,'2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');
             INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-a','org-a','nas','alpha','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z'),
               ('arr-b','org-b','nas','bravo','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');
             INSERT INTO nas_scrub_schedules (pool,enabled,schedule_json) VALUES
               ('tank',1,'{\"every\":\"weekly\",\"hour\":2,\"minute\":0,\"weekday\":0,\"day\":1}'),
               ('fast',0,'{\"every\":\"weekly\",\"hour\":2,\"minute\":0,\"weekday\":0,\"day\":1}');",
        ).expect("fixture");
        db::remember_pools(
            &pool,
            &std::collections::HashMap::from([("tank".to_string(), "1".to_string()), ("fast".to_string(), "2".to_string())]),
        )
        .expect("pools");

        let by_kind = |list: &[DisableConsequence], kind: &str| list.iter().find(|c| c.kind == kind).cloned();
        let b = disable_consequences_of(&pool, "org-a", 2).expect("mode B");
        assert!(by_kind(&b, "tentanas_api_closed").is_some());
        assert_eq!(by_kind(&b, "tentanas_smb_shares_continue").expect("smb").count_vars["n"], 2, "only the enabled shares serve");
        assert_eq!(by_kind(&b, "tentanas_nfs_shares_continue").expect("nfs").count_vars["n"], 1);
        assert_eq!(by_kind(&b, "tentanas_iscsi_targets_until_reboot").expect("targets, mode B").count_vars["n"], 1);
        assert!(by_kind(&b, "tentanas_nvmet_targets_until_reboot").is_none(), "no sentence about zero NVMe-oF targets");
        let schedules = by_kind(&b, "tentanas_schedules_stop").expect("schedules stop in mode B");
        assert_eq!(schedules.count_vars["n"], 1, "the disabled schedule is not counted");
        assert_eq!(schedules.effect, "stops");
        assert!(by_kind(&b, "tentanas_background_stop").is_some());
        assert_eq!(by_kind(&b, "tentanas_arrays_mounted").expect("own arrays").names, vec!["alpha"]);
        assert_eq!(by_kind(&b, "tentanas_arrays_mounted_other").expect("the others").count_vars["n"], 1);
        assert_eq!(by_kind(&b, "tentanas_pools_imported").expect("pools").names, vec!["fast", "tank"]);
        assert_eq!(by_kind(&b, "tentanas_jobs_continue").expect("jobs").count_vars["n"], 2);
        let text = format!("{b:?}");
        assert!(!text.contains("bravo"), "another organisation's array is never named: {text}");

        elevation::set_mode(&pool, elevation::Mode::Helper).expect("mode A");
        let a = disable_consequences_of(&pool, "org-a", 0).expect("mode A");
        assert!(by_kind(&a, "tentanas_iscsi_targets_continue").is_some());
        assert!(by_kind(&a, "tentanas_schedules_continue").is_some());
        assert!(by_kind(&a, "tentanas_background_continue").is_some());
        assert!(by_kind(&a, "tentanas_schedules_stop").is_none());
        assert!(by_kind(&a, "tentanas_jobs_continue").is_none(), "no job runs, nothing to say");
    }

    /// What a node publishes for the others to know while it is offline is
    /// exactly what makes its teardown refuse: its Elastic Arrays.
    #[test]
    fn the_published_blocker_is_the_arrays_under_supervision() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        db::migrate(&conn).expect("migrate");
        let pool: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        assert!(teardown_blocker(&pool).is_none(), "no array, nothing refuses");
        pool.write().expect("write").execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-a','org-a','nas','alpha','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');",
        ).expect("array");
        let block = teardown_blocker(&pool).expect("the array refuses the teardown");
        assert!(block.blocks);
        assert_eq!(block.kind, "tentanas_elastic_arrays");
        assert_eq!(block.count_vars["n"], 1);
        assert!(db::block_elastic_teardown(&pool).is_err(), "and the teardown itself refuses on the same table");
    }

    fn token(secret: &str) -> std::sync::Arc<crate::profiling::collectors::elevation::ElevationToken> {
        std::sync::Arc::new(crate::profiling::collectors::elevation::ElevationToken::new_sudo(secret.to_string()))
    }

    /// MAJOR B of round 2: the teardown password is held for ONE instance's
    /// teardown, taken once, dropped on demand, and gone when it expires —
    /// and it is not the node's armed slot, so nothing else can use it.
    #[test]
    fn the_teardown_password_is_one_shot_and_bound_to_its_instance() {
        let id = "tentanas-hold0001";
        teardown_hold::put(id, token("test-secret-not-real"), std::time::Duration::from_secs(60));
        assert!(elevation::armed_token().is_none(), "the node's armed slot is untouched");
        assert!(teardown_hold::take("tentanas-other001").is_none(), "another instance cannot take it");
        assert!(teardown_hold::take(id).is_some(), "the teardown takes it");
        assert!(teardown_hold::take(id).is_none(), "once");

        teardown_hold::put(id, token("test-secret-not-real"), std::time::Duration::from_secs(60));
        native_disarm_teardown(&NativeAppContext {
            db: &crate::dispatch::state::AppState::for_test().db,
            addon_id: id,
            org_id: crate::services::org::DEFAULT_ORG_ID,
            data_dir: std::env::temp_dir(),
        });
        assert!(!teardown_hold::held(id), "a disarm drops it");

        teardown_hold::put(id, token("test-secret-not-real"), std::time::Duration::from_millis(0));
        assert!(teardown_hold::take(id).is_none(), "an expired one is not handed out");
    }

    /// Every way out of the teardown hook drops the password — here the
    /// earliest one (the instance database cannot be opened), which returns
    /// before any privileged step (critic: the refusing path skipped it).
    #[test]
    fn a_teardown_that_ends_early_still_consumes_the_password() {
        let id = "tentanas-hold0002";
        let state = crate::dispatch::state::AppState::for_test();
        teardown_hold::put(id, token("test-secret-not-real"), std::time::Duration::from_secs(60));
        let ctx = NativeAppContext {
            db: &state.db,
            addon_id: id,
            org_id: crate::services::org::DEFAULT_ORG_ID,
            data_dir: std::env::temp_dir(),
        };
        assert!(native_teardown(&ctx).is_err(), "no such instance: the hook ends at once");
        assert!(!teardown_hold::held(id), "and the password went with it");
    }
}

