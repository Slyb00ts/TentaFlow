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
pub mod pools;
pub mod rdma;
pub mod scheduler;
pub mod shares;
pub mod snapshots;
pub mod targets;
pub mod zfs;

use anyhow::Result;

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
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
    let targets = open_db(ctx.db, ctx.org_id, ctx.addon_id)
        .and_then(|db| db::list_targets(&db))
        .unwrap_or_default();
    let has = |protocol: &str| targets.iter().any(|t| t.protocol == protocol);
    let mut entries = config_teardown_entries(&|path| match path {
        p if p == tentanas_helper::block::TARGET_CONFIGFS => has("iscsi"),
        p if p == tentanas_helper::block::NVMET_CONFIGFS => has("nvmet"),
        p => std::path::Path::new(p).exists(),
    });
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

/// Native teardown hook (§5.8, in order): stop the loops and the jobs, take
/// the app's config back out of smbd/nfsd and unmount what it mounted, export
/// the pools cleanly, and write the configuration backup outside the instance
/// directory the platform is about to remove.
pub fn native_teardown(ctx: &NativeAppContext) -> Result<()> {
    let pool=open_db(ctx.db,ctx.org_id,ctx.addon_id)?;
    db::block_elastic_teardown(&pool)?;
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
                tokio::task::block_in_place(|| handle.block_on(teardown_steps(&db)));
            }
            Err(_) => tracing::warn!(
                "tentanas teardown: no tokio runtime, services and pools left as they are"
            ),
        },
        Err(e) => tracing::warn!("tentanas teardown: instance database unreadable: {e}"),
    }
    elevation::disarm();
    crate::addon::app_db::close(ctx.addon_id);
    Ok(())
}

async fn teardown_steps(db: &DbPool) {
    // The configuration document is READ first and written last: after the
    // pools are exported `zpool list` shows nothing, and a backup without the
    // pool layouts is exactly the one thing an admin would need it for.
    let document = config_io::export(db).await;

    for line in shares::remove_all(db, None).await {
        tracing::info!("tentanas teardown: {line}");
    }
    // §5.8 step 2, the block half: a target left in the kernel would keep
    // handing a client a raw disk from a node that no longer manages it, and
    // no file on disk would say so.
    for line in targets::remove_all(db, None).await {
        tracing::info!("tentanas teardown: {line}");
    }
    for line in fleet_mounts::unmount_all(db).await {
        tracing::info!("tentanas teardown: {line}");
    }

    // The ARC drop-in is the app's file and the teardown plan promises to take
    // it out; it goes before the pools, while the channel is still needed for
    // the export anyway.
    if std::path::Path::new(tentanas_helper::ARC_MODPROBE_PATH).exists()
        && broker::channel_available(db).await
    {
        let command = tentanas_helper::HelperCommand::ArcLimitClear {};
        match broker::run_privileged(db, &command, None, std::time::Duration::from_secs(30)).await {
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
    if broker::channel_available(db).await {
        for pool in pools::list_rows().await.unwrap_or_default() {
            let command = tentanas_helper::HelperCommand::ZpoolExport {
                pool: pool.name.clone(),
                force: false,
            };
            match broker::run_privileged(db, &command, None, std::time::Duration::from_secs(300))
                .await
            {
                Ok((out, _)) if out.success() => {
                    tracing::info!("tentanas teardown: pool {} exported", pool.name)
                }
                Ok((out, _)) => tracing::warn!(
                    "tentanas teardown: pool {} not exported: {}",
                    pool.name,
                    out.stderr.trim()
                ),
                Err(e) => {
                    tracing::warn!("tentanas teardown: pool {} not exported: {e}", pool.name)
                }
            }
        }
    } else {
        tracing::warn!(
            "tentanas teardown: no privilege channel — the pools stay imported and mounted"
        );
    }

    match document.and_then(|d| config_io::write_backup(&d)) {
        Ok(path) => tracing::info!("tentanas teardown: configuration saved to {}", path.display()),
        Err(e) => tracing::warn!("tentanas teardown: configuration backup failed: {e}"),
    }
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
}
