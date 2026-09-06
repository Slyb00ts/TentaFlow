// =============================================================================
// File: tentanas/config_io.rs — configuration export and import (plan-02 §5.8).
//
//       The export is the node's DESIRED state, not a backup: pool layouts and
//       the disks they were built from, datasets with their locally set
//       properties, shares, share users and schedules. It never carries data
//       and never carries a secret — no Samba password, no encryption key —
//       so it is safe to hand to anyone who may see the config, and a restored
//       user has to be given a password again.
//
//       The import is a two-step: `plan` diffs the document against the live
//       node and says what each entry would do, `apply` runs the plan as a job
//       in dependency order (pools, datasets, shares, users, schedules),
//       skipping every entry the plan called a conflict.
//
//       The uninstall writes an export automatically, OUTSIDE the instance
//       directory, before the platform wipes it (§5.8 step 5) — the one copy
//       of the configuration that survives the app.
// =============================================================================

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tentaflow_protocol::tentanas::{
    NasBlockCapabilities, NasConfigImportItem, NasNfsOptions, NasSchedule, NasSmartSchedule,
    NasSmbOptions, NasSnapshotSchedule, NasTargetLun, NasTargetPortGroup, NasTargetPortal,
};
use tentanas_helper::HelperCommand;

use super::db::{self as store, ShareRow};
use crate::crypto::SettingsCipher;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;

/// Bumped when a field changes meaning. An import refuses a schema it does not
/// know rather than guessing at half of it.
pub const SCHEMA: u32 = 1;

const STEP_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub schema: u32,
    pub exported_at: String,
    pub node_id: String,
    pub node_name: String,
    pub pools: Vec<PoolConfig>,
    pub datasets: Vec<DatasetConfig>,
    pub shares: Vec<ShareConfig>,
    pub share_users: Vec<ShareUserConfig>,
    pub schedules: SchedulesConfig,
    /// Block targets (§5.5). Defaulted rather than schema-bumped: a document
    /// exported before targets existed simply has none, and no field of the
    /// older shape changed meaning — which is the only thing `SCHEMA` is for.
    #[serde(default)]
    pub targets: Vec<TargetConfig>,
}

/// A pool as it can be recognized again on other hardware: the GUID imports
/// it, the serials say whether its disks are actually here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolConfig {
    pub name: String,
    pub guid: String,
    pub layout: String,
    pub disks: Vec<DiskRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiskRef {
    /// The stable `/dev/disk/by-id` path the pool was built with.
    pub path: String,
    pub serial: String,
}

/// Only LOCALLY set properties travel: an inherited value is the parent's
/// business and re-setting it here would pin it forever.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatasetConfig {
    pub name: String,
    pub kind: String,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareConfig {
    pub name: String,
    pub protocol: String,
    pub source_path: String,
    pub enabled: bool,
    pub fleet_mount: bool,
    pub smb: Option<NasSmbOptions>,
    pub nfs: Option<NasNfsOptions>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareUserConfig {
    pub name: String,
    pub description: String,
}

/// One block target in the exported document.
///
/// The authentication METHOD and the user names travel; the CHAP and
/// DH-HMAC-CHAP secrets never do (§5.8: "Sekrety (CHAP) nie wchodzą do
/// eksportu — do ponownego wpisania"). An imported authenticated target is
/// therefore created DISABLED, so a restore can never bring a target back with
/// its authentication silently switched off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetConfig {
    pub name: String,
    pub protocol: String,
    pub wwn: String,
    pub enabled: bool,
    pub luns: Vec<NasTargetLun>,
    pub portals: Vec<NasTargetPortal>,
    pub port_groups: Vec<NasTargetPortGroup>,
    #[serde(default)]
    pub initiators: Vec<String>,
    pub auth_method: String,
    #[serde(default)]
    pub auth_username: String,
    #[serde(default)]
    pub auth_mutual_username: String,
    #[serde(default)]
    pub dhchap_hash: String,
    #[serde(default)]
    pub dhchap_dhgroup: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulesConfig {
    pub scrub: Vec<PoolTaskConfig>,
    pub snapshot: Vec<NasSnapshotSchedule>,
    pub smart: NasSmartSchedule,
    /// The recurring `zpool trim` of §5.10. Defaulted: a document exported
    /// before the schedule existed simply has no trims to restore.
    #[serde(default)]
    pub trim: Vec<PoolTaskConfig>,
}

/// One recurring pool task in the exported document — the scrub and the trim
/// have the same three facts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolTaskConfig {
    pub pool: String,
    pub enabled: bool,
    pub schedule: NasSchedule,
}

// =============================================================================
// export
// =============================================================================

/// This node's own name. Shared with the target layer, which builds the IQN /
/// NQN out of it — the node id is a UUID and would make a WWN nobody can read.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The locally set properties of every dataset, keyed by dataset name. Only
/// the names the privilege catalog is willing to write survive: an import that
/// hit an unknown property would fail on it and stop halfway.
async fn local_properties() -> BTreeMap<String, BTreeMap<String, String>> {
    let text = match super::zfs::zfs(&[
        "get", "-Hp", "-s", "local", "-t", "filesystem,volume", "all",
    ])
    .await
    {
        Ok(t) => t,
        Err(_) => return BTreeMap::new(),
    };
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (name, property) in super::datasets::parse_get(&text) {
        if tentanas_helper::validate_dataset_property(&property.name, &property.value).is_ok() {
            out.entry(name)
                .or_default()
                .insert(property.name, property.value);
        }
    }
    out
}

/// The node's whole desired state.
pub async fn export(db: &DbPool) -> Result<ConfigDocument> {
    let pools = super::pools::collect(db).await.unwrap_or_default();
    let inventory = super::disks::snapshot().0;
    let serial_of = |name: &str| -> String {
        inventory
            .iter()
            .find(|d| d.name == name || d.disk_id == name)
            .map(|d| d.serial.clone())
            .unwrap_or_default()
    };
    let pools = pools
        .into_iter()
        .map(|p| PoolConfig {
            name: p.name,
            guid: p.guid,
            layout: p.layout,
            disks: p
                .vdevs
                .iter()
                .flat_map(|v| v.disks.iter())
                .map(|d| DiskRef {
                    path: super::zfs::stable_device_path(&d.name),
                    serial: serial_of(&d.name),
                })
                .collect(),
        })
        .collect();

    let properties = local_properties().await;
    let datasets = super::datasets::list("")
        .await
        .unwrap_or_default()
        .into_iter()
        // A pool root is created by `zpool create`, not by the import.
        .filter(|d| d.name.contains('/'))
        .map(|d| DatasetConfig {
            properties: properties.get(&d.name).cloned().unwrap_or_default(),
            kind: d.kind,
            name: d.name,
        })
        .collect();

    let shares = store::list_shares(db)?
        .into_iter()
        .map(|s| ShareConfig {
            name: s.name,
            protocol: s.protocol,
            source_path: s.source_path,
            enabled: s.enabled,
            fleet_mount: s.fleet_mount,
            smb: s.smb,
            nfs: s.nfs,
        })
        .collect();

    let share_users = store::list_share_users(db)?
        .into_iter()
        .map(|u| ShareUserConfig {
            name: u.name,
            description: u.description,
        })
        .collect();

    let pool_tasks = |task: store::PoolTask| -> Result<Vec<PoolTaskConfig>> {
        Ok(store::list_pool_schedules(db, task)?
            .into_iter()
            .map(|s| PoolTaskConfig {
                pool: s.pool,
                enabled: s.enabled,
                schedule: s.schedule,
            })
            .collect())
    };
    let scrub = pool_tasks(store::PoolTask::Scrub)?;
    let trim = pool_tasks(store::PoolTask::Trim)?;

    // The four secret columns of `nas_targets` are not read here at all: the
    // export carries the method and the user names, never a key.
    let targets = store::list_targets(db)?
        .into_iter()
        .map(|t| TargetConfig {
            name: t.name,
            protocol: t.protocol,
            wwn: t.wwn,
            enabled: t.enabled,
            luns: t.luns,
            portals: t.portals,
            port_groups: t.port_groups,
            initiators: t.initiators,
            auth_method: t.auth_method,
            auth_username: t.auth_username,
            auth_mutual_username: t.auth_mutual_username,
            dhchap_hash: t.dhchap_hash,
            dhchap_dhgroup: t.dhchap_dhgroup,
        })
        .collect();

    Ok(ConfigDocument {
        schema: SCHEMA,
        exported_at: store::now(),
        node_id: super::fleet_mounts::local_node_id(),
        node_name: hostname(),
        pools,
        datasets,
        shares,
        share_users,
        targets,
        schedules: SchedulesConfig {
            scrub,
            snapshot: store::list_snapshot_schedules(db)?,
            smart: store::smart_schedule(db)?,
            trim,
        },
    })
}

/// `tentanas-<node>-<YYYYMMDD-HHMMSS>.json` — the name the download and the
/// uninstall backup share.
pub fn filename(document: &ConfigDocument) -> String {
    let node = if document.node_name.is_empty() {
        document.node_id.as_str()
    } else {
        document.node_name.as_str()
    };
    let node: String = node
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!(
        "tentanas-{node}-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    )
}

/// Writes the export where the instance wipe cannot reach it (§5.8 step 5).
/// 0600 because the document describes the node's whole storage layout.
pub fn write_backup(document: &ConfigDocument) -> Result<std::path::PathBuf> {
    let dir = crate::paths::tentaflow_home().join("app-backups");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(filename(document));
    std::fs::write(&path, serde_json::to_string_pretty(document)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

// =============================================================================
// import plan
// =============================================================================

pub fn parse(json: &str) -> Result<ConfigDocument> {
    let document: ConfigDocument =
        serde_json::from_str(json).map_err(|e| anyhow!("the file is not a TentaNas export: {e}"))?;
    if document.schema != SCHEMA {
        return Err(anyhow!(
            "export schema {} cannot be read by this version (expected {SCHEMA})",
            document.schema
        ));
    }
    Ok(document)
}

/// What the live node looks like, so the diff is one pure function over it.
pub struct LiveState {
    pub pools: Vec<(String, String)>,
    /// Serials of every disk this node can see right now.
    pub serials: Vec<String>,
    pub datasets: Vec<String>,
    pub pool_mountpoints: Vec<String>,
    pub shares: Vec<ShareRow>,
    pub targets: Vec<store::TargetRow>,
    pub share_users: Vec<String>,
    pub scrub_pools: Vec<String>,
    pub trim_pools: Vec<String>,
    pub snapshot_datasets: Vec<String>,
    pub smart_enabled: bool,
}

pub async fn live_state(db: &DbPool) -> Result<LiveState> {
    let datasets = super::datasets::list("").await.unwrap_or_default();
    Ok(LiveState {
        pools: super::pools::list_rows()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.name, p.guid))
            .collect(),
        serials: super::disks::snapshot()
            .0
            .into_iter()
            .map(|d| d.serial)
            .filter(|s| !s.is_empty())
            .collect(),
        pool_mountpoints: datasets
            .iter()
            .filter(|d| !d.name.contains('/'))
            .filter_map(|d| d.mountpoint.clone())
            .collect(),
        datasets: datasets.into_iter().map(|d| d.name).collect(),
        shares: store::list_shares(db)?,
        targets: store::list_targets(db)?,
        share_users: store::list_share_users(db)?
            .into_iter()
            .map(|u| u.name)
            .collect(),
        scrub_pools: store::list_pool_schedules(db, store::PoolTask::Scrub)?
            .into_iter()
            .map(|s| s.pool)
            .collect(),
        trim_pools: store::list_pool_schedules(db, store::PoolTask::Trim)?
            .into_iter()
            .map(|s| s.pool)
            .collect(),
        snapshot_datasets: store::list_snapshot_schedules(db)?
            .into_iter()
            .map(|s| s.dataset)
            .collect(),
        smart_enabled: store::smart_schedule(db)?.enabled,
    })
}

fn item(kind: &str, name: &str, action: &str, detail: impl Into<String>) -> NasConfigImportItem {
    NasConfigImportItem {
        kind: kind.to_string(),
        name: name.to_string(),
        action: action.to_string(),
        detail: detail.into(),
    }
}

/// The diff. Pure over `live`, so a fixture exercises every branch.
pub fn plan(document: &ConfigDocument, live: &LiveState) -> (Vec<NasConfigImportItem>, Vec<String>) {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    if document.node_id != super::fleet_mounts::local_node_id() {
        warnings.push(format!(
            "the export was taken on '{}' — check that the disks of that node are attached here",
            if document.node_name.is_empty() {
                document.node_id.as_str()
            } else {
                document.node_name.as_str()
            }
        ));
    }

    for pool in &document.pools {
        if live
            .pools
            .iter()
            .any(|(name, guid)| *name == pool.name || (!guid.is_empty() && *guid == pool.guid))
        {
            items.push(item("pool", &pool.name, "skip", "already imported"));
            continue;
        }
        let missing: Vec<&str> = pool
            .disks
            .iter()
            .filter(|d| !d.serial.is_empty() && !live.serials.contains(&d.serial))
            .map(|d| d.serial.as_str())
            .collect();
        if missing.is_empty() && !pool.disks.is_empty() {
            items.push(item(
                "pool",
                &pool.name,
                "import",
                format!("{} disks present, imported by GUID", pool.disks.len()),
            ));
        } else {
            items.push(item(
                "pool",
                &pool.name,
                "missing",
                format!("{} of {} disks not found: {}", missing.len(), pool.disks.len(), missing.join(", ")),
            ));
        }
    }

    // A dataset whose pool the plan is not going to import cannot be created.
    let pools_after: Vec<String> = live
        .pools
        .iter()
        .map(|(name, _)| name.clone())
        .chain(
            items
                .iter()
                .filter(|i| i.kind == "pool" && (i.action == "import" || i.action == "skip"))
                .map(|i| i.name.clone()),
        )
        .collect();
    for dataset in &document.datasets {
        let pool = dataset.name.split('/').next().unwrap_or_default();
        if live.datasets.contains(&dataset.name) {
            items.push(item("dataset", &dataset.name, "skip", "already exists"));
        } else if !pools_after.iter().any(|p| p == pool) {
            items.push(item(
                "dataset",
                &dataset.name,
                "conflict",
                format!("pool '{pool}' is not available on this node"),
            ));
        } else {
            items.push(item(
                "dataset",
                &dataset.name,
                "create",
                format!("{} with {} properties", dataset.kind, dataset.properties.len()),
            ));
        }
    }

    for user in &document.share_users {
        if live.share_users.contains(&user.name) {
            items.push(item("share_user", &user.name, "skip", "already exists"));
        } else {
            items.push(item(
                "share_user",
                &user.name,
                "create",
                "the password has to be set again — exports carry no secrets",
            ));
        }
    }

    for share in &document.shares {
        if let Some(existing) = live.shares.iter().find(|s| s.name == share.name) {
            let detail = if existing.source_path == share.source_path {
                "already exists".to_string()
            } else {
                format!("kept: it already points at {}", existing.source_path)
            };
            items.push(item("share", &share.name, "skip", detail));
            continue;
        }
        if let Some(other) = live
            .shares
            .iter()
            .find(|s| s.source_path == share.source_path)
        {
            items.push(item(
                "share",
                &share.name,
                "conflict",
                format!("'{}' already exports {}", other.name, share.source_path),
            ));
            continue;
        }
        let under_pool = live
            .pool_mountpoints
            .iter()
            .any(|m| share.source_path == *m || share.source_path.starts_with(&format!("{m}/")));
        if under_pool {
            items.push(item("share", &share.name, "create", &share.protocol));
        } else {
            items.push(item(
                "share",
                &share.name,
                "conflict",
                format!("{} is not under a pool of this node", share.source_path),
            ));
        }
    }

    // Read once for the whole loop: enumerating interfaces is a syscall walk,
    // and the preview is on a request path.
    let local_interfaces = super::targets::interfaces();
    for target in &document.targets {
        if live.targets.iter().any(|t| t.name == target.name) {
            items.push(item("target", &target.name, "skip", "already exists"));
            continue;
        }
        if let Some(other) = live.targets.iter().find(|t| t.wwn == target.wwn) {
            // The WWN is what an initiator addresses the disk by; two targets
            // claiming one is also two rows fighting over a configfs object.
            items.push(item(
                "target",
                &target.name,
                "conflict",
                format!("'{}' already publishes {}", other.name, target.wwn),
            ));
            continue;
        }
        let missing: Vec<&str> = target
            .luns
            .iter()
            .filter(|l| !live.datasets.contains(&l.source))
            .map(|l| l.source.as_str())
            .collect();
        if !missing.is_empty() {
            items.push(item(
                "target",
                &target.name,
                "conflict",
                format!("this node has no {}", missing.join(", ")),
            ));
            continue;
        }
        if let Some(other) = live.targets.iter().find(|t| {
            t.luns
                .iter()
                .any(|l| target.luns.iter().any(|n| n.source == l.source))
        }) {
            items.push(item(
                "target",
                &target.name,
                "conflict",
                format!("'{}' already exports that volume", other.name),
            ));
            continue;
        }
        let mut detail = if target.auth_method == "none" {
            format!("{} · no authentication", target.protocol)
        } else {
            // §5.8: no secret is in the export, so an authenticated target
            // comes back disabled rather than open.
            format!(
                "{} · {} — created disabled, the secret has to be set again",
                target.protocol, target.auth_method
            )
        };
        // A portal is an ADDRESS, and the document carries the EXPORTING
        // node's. On any other node that address belongs to nobody, so the
        // target lands frozen with a drift alert the moment it is judged —
        // and the preview has to say so, because "create · iscsi · no
        // authentication" reads like a target that will serve.
        //
        // The import does NOT pick a local address for it. That would be the
        // automatic re-plumbing the owner's decision rules out, wearing an
        // import for a hat: a raw disk would appear on whatever network this
        // node happens to have, chosen by nobody.
        //
        // The every-interface portal is the EXCEPTION and it goes first,
        // because the sentence below is false about it in the loudest possible
        // direction. A portal with no interface has address `0.0.0.0`, which
        // belongs to no interface of any node — so it used to fall into the
        // "not an address of this node" bucket and produce both a nonsense
        // string ("0.0.0.0 on ") and a promise that the target would sit
        // frozen and wait. It does not: `target_state` skips a portal with no
        // interface, the target is judged appliable, and it comes up serving a
        // raw disk on EVERY network of the importing node. That is the exact
        // thing §5.5(a) exists to prevent, and it deserves its own sentence.
        if target.portals.iter().any(|p| p.interface.is_empty()) {
            detail.push_str(
                " · the portal is 'every interface' (0.0.0.0) — on this node that is every \
                 network it is attached to, which is not the network it was chosen on, so it is \
                 imported DISABLED and an admin has to pick an interface and enable it",
            );
        }
        let elsewhere: Vec<String> = target
            .portals
            .iter()
            .filter(|p| !p.interface.is_empty() && !p.address.is_empty())
            .filter(|p| {
                !super::targets::bindable_addresses(&local_interfaces, &p.interface)
                    .iter()
                    .any(|a| *a == p.address)
            })
            .map(|p| format!("{} on {}", p.address, p.interface))
            .collect();
        if !elsewhere.is_empty() {
            detail.push_str(&format!(
                " · the portal ({}) is not an address of this node — the target will report the \
                 drift and wait for an admin to re-pick the interface in the wizard",
                elsewhere.join(", ")
            ));
        }
        items.push(item("target", &target.name, "create", detail));
    }

    for (label, tasks, live_pools) in [
        ("scrub", &document.schedules.scrub, &live.scrub_pools),
        ("trim", &document.schedules.trim, &live.trim_pools),
    ] {
        for task in tasks {
            let action = if live_pools.contains(&task.pool) {
                "update"
            } else {
                "create"
            };
            items.push(item(
                "schedule",
                &format!("{label} {}", task.pool),
                action,
                task.schedule.every.clone(),
            ));
        }
    }
    for snapshot in &document.schedules.snapshot {
        let action = if live.snapshot_datasets.contains(&snapshot.dataset) {
            "update"
        } else {
            "create"
        };
        items.push(item(
            "schedule",
            &format!("snapshot {}", snapshot.dataset),
            action,
            snapshot.schedule.every.clone(),
        ));
    }
    if document.schedules.smart.enabled {
        items.push(item(
            "schedule",
            "smart",
            if live.smart_enabled { "update" } else { "create" },
            format!(
                "short {} · long {}",
                document.schedules.smart.short.every, document.schedules.smart.long.every
            ),
        ));
    }
    (items, warnings)
}

/// What the plan would REPLACE rather than add: the rows whose action is
/// 'update' are existing schedules the import overwrites. §5.10 sends exactly
/// an overwriting import through four eyes — an import that only creates what
/// is missing takes nothing away from anyone.
pub fn overwritten(items: &[NasConfigImportItem]) -> Vec<String> {
    items
        .iter()
        .filter(|i| i.action == "update")
        .map(|i| i.name.clone())
        .collect()
}

// =============================================================================
// import apply
// =============================================================================

/// Runs the plan. Every step logs what it did; a conflict or a missing pool is
/// logged and skipped, so one unavailable pool never stops the shares that do
/// not depend on it.
pub async fn apply(
    handle: &super::jobs::JobHandle,
    main_db: &DbPool,
    addon_id: &str,
    document: ConfigDocument,
    explicit: Option<&ElevationToken>,
) -> Result<()> {
    let db = handle.db().clone();
    let live = live_state(&db).await?;
    apply_with(handle, main_db, addon_id, document, explicit, live, None).await
}

/// The same, with the node's LIVE STATE injected — the seam every other
/// decision in this slice already has (`installed`, `in_kernel`, `retries`,
/// `preview_in`).
///
/// Without it `apply` was unreachable from any test: it read its own live
/// state, which means shelling out to `zfs` and `zpool`, and on a machine
/// without them every target in the document is judged "conflict — its zvol is
/// not on this node" and the import loop `continue`s past everything. So the
/// whole body — including the host-collision report it ends with — had never
/// once been executed by a test.
pub async fn apply_with(
    handle: &super::jobs::JobHandle,
    main_db: &DbPool,
    addon_id: &str,
    document: ConfigDocument,
    explicit: Option<&ElevationToken>,
    live: LiveState,
    block_caps: Option<NasBlockCapabilities>,
) -> Result<()> {
    let db = handle.db().clone();
    let (items, warnings) = plan(&document, &live);
    for warning in warnings {
        handle.log(format!("warning: {warning}"));
    }
    let action_of = |kind: &str, name: &str| -> String {
        items
            .iter()
            .find(|i| i.kind == kind && i.name == name)
            .map(|i| i.action.clone())
            .unwrap_or_else(|| "skip".to_string())
    };
    let total = items.len().max(1);
    let mut done = 0usize;
    let step = |handle: &super::jobs::JobHandle, done: &mut usize| {
        *done += 1;
        handle.progress(((*done * 100) / total).min(99) as u8);
    };

    for pool in &document.pools {
        if action_of("pool", &pool.name) != "import" {
            handle.log(format!("pool {}: skipped", pool.name));
            step(handle, &mut done);
            continue;
        }
        let command = HelperCommand::ZpoolImport {
            guid: pool.guid.clone(),
            new_name: String::new(),
            force: false,
        };
        match super::jobs::run_step(handle, &command, explicit, STEP_TIMEOUT).await {
            Ok(_) => handle.log(format!("pool {}: imported", pool.name)),
            Err(e) => handle.log(format!("pool {}: import failed: {e}", pool.name)),
        }
        step(handle, &mut done);
    }

    for dataset in &document.datasets {
        if action_of("dataset", &dataset.name) != "create" {
            handle.log(format!("dataset {}: skipped", dataset.name));
            step(handle, &mut done);
            continue;
        }
        let kind = if dataset.kind == "volume" {
            tentanas_helper::DatasetKind::Volume
        } else {
            tentanas_helper::DatasetKind::Filesystem
        };
        let volsize = dataset.properties.get("volsize").cloned().unwrap_or_default();
        let properties: Vec<(String, String)> = dataset
            .properties
            .iter()
            .filter(|(k, _)| k.as_str() != "volsize")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let command = HelperCommand::ZfsCreate {
            name: dataset.name.clone(),
            kind,
            volsize,
            sparse: false,
            properties,
            // An encrypted dataset needs its key, and the export deliberately
            // carries none: it is recreated plain and the admin re-encrypts.
            encryption: false,
        };
        match super::jobs::run_step(handle, &command, explicit, STEP_TIMEOUT).await {
            Ok(_) => handle.log(format!("dataset {}: created", dataset.name)),
            Err(e) => handle.log(format!("dataset {}: create failed: {e}", dataset.name)),
        }
        step(handle, &mut done);
    }

    for user in &document.share_users {
        if action_of("share_user", &user.name) == "create" {
            store::upsert_share_user(&db, &user.name, &user.description)?;
            handle.log(format!(
                "share user {}: created — set a password before it can connect",
                user.name
            ));
        } else {
            handle.log(format!("share user {}: skipped", user.name));
        }
        step(handle, &mut done);
    }

    let mut shares_changed = false;
    for share in &document.shares {
        if action_of("share", &share.name) != "create" {
            handle.log(format!("share {}: skipped", share.name));
            step(handle, &mut done);
            continue;
        }
        let now = store::now();
        let row = ShareRow {
            share_id: uuid::Uuid::now_v7().to_string(),
            name: share.name.clone(),
            protocol: share.protocol.clone(),
            source_path: share.source_path.clone(),
            dataset: None,
            enabled: share.enabled,
            fleet_mount: share.fleet_mount,
            smb: share.smb.clone(),
            nfs: share.nfs.clone(),
            state: "disabled".to_string(),
            state_detail: String::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        store::upsert_share(&db, &row)?;
        shares_changed = true;
        handle.log(format!("share {}: created", share.name));
        step(handle, &mut done);
    }

    let mut targets_changed = false;
    // The catalog's own rules, applied to an imported row BEFORE it is
    // written. Without this a target whose transport this node cannot serve
    // (iSER on a node with no `ib_isert`) got past the preview and failed
    // inside `apply_one` — after every row of the document was already in the
    // database, taking the whole import job down with a message about one
    // target. Judged once here: the probe is the same one the wizard is
    // judged against.
    // Injected for the same reason `live` is: probing means `modprobe`
    // listings and a `zfs list`, so a test on a machine with neither would see
    // "this node cannot serve NVMe-oF" and every target refused before the
    // loop got anywhere.
    let block_caps = if let Some(caps) = block_caps {
        caps
    } else if document.targets.is_empty() {
        Default::default()
    } else {
        let features = super::environment::cached_or_probe(&db)
            .await
            .map(|e| e.features)
            .unwrap_or_default();
        let datasets = super::datasets::list("").await.unwrap_or_default();
        super::targets::capabilities(&features, &datasets, &[])
    };
    // Read ONCE and grown as rows land, not re-read per document target: the
    // list only changes here, and the query was inside the loop — O(n²)
    // database reads for a document with n targets.
    let mut imported = store::list_targets(&db).unwrap_or_default();
    for target in &document.targets {
        if action_of("target", &target.name) != "create" {
            handle.log(format!("target {}: skipped", target.name));
            step(handle, &mut done);
            continue;
        }
        let now = store::now();
        // An authenticated target has no secret in the export, so it comes
        // back DISABLED. Enabling it with an empty secret would turn a target
        // that used to require CHAP into one that requires nothing (§5.8).
        let authenticated = target.auth_method != "none";
        // §5.5(a): a portal on EVERY interface is a deliberate decision, and
        // the decision that was taken belongs to the node the document came
        // from. On this node "every interface" is a different set of networks
        // — quite possibly including the LAN — so the target arrives DISABLED
        // and an admin picks an interface and enables it, exactly the way an
        // authenticated one waits for its secret. Importing it enabled would
        // hand a raw disk to every network of this node on the strength of a
        // decision nobody took here.
        let all_interfaces = target.portals.iter().any(|p| p.interface.is_empty());
        let row = store::TargetRow {
            target_id: uuid::Uuid::now_v7().to_string(),
            name: target.name.clone(),
            protocol: target.protocol.clone(),
            wwn: target.wwn.clone(),
            enabled: target.enabled && !authenticated && !all_interfaces,
            luns: target.luns.clone(),
            portals: target.portals.clone(),
            port_groups: target.port_groups.clone(),
            initiators: target.initiators.clone(),
            auth_method: target.auth_method.clone(),
            auth_username: target.auth_username.clone(),
            auth_secret: String::new(),
            auth_mutual_username: target.auth_mutual_username.clone(),
            auth_mutual_secret: String::new(),
            dhchap_hash: target.dhchap_hash.clone(),
            dhchap_dhgroup: target.dhchap_dhgroup.clone(),
            state: "disabled".to_string(),
            state_detail: if authenticated {
                "the authentication secret has to be entered again after an import".to_string()
            } else if all_interfaces {
                "this target was exported on every interface (0.0.0.0) — pick an interface \
                 of this node before enabling it"
                    .to_string()
            } else {
                String::new()
            },
            created_at: now.clone(),
            updated_at: now,
        };
        // `confirm_all_interfaces` is true because that decision is re-taken
        // above by importing such a target DISABLED — §5.5(a) is enforced by
        // the row arriving switched off, not by refusing to describe it.
        // The catalog's own rules, on the two attributes an import copies
        // verbatim from a file somebody may have edited.
        //
        // `validate_options` runs on the next line, but it renders the spec
        // with PLACEHOLDER credentials, and a host with no key skips the
        // hash/DH-group rules entirely (`validate_nvmet`) — so an imported
        // `dhchap_hash: "sha1"` sailed through here and became a row that
        // failed at APPLY, as a catalog string in a job log, instead of "not
        // imported — …" at the moment the row was created. That is the
        // principle `host_allowlist_conflict` is built on, applied to the one
        // other field an import takes on trust.
        let bad_dhchap = (row.protocol == "nvmet"
            && !row.dhchap_hash.is_empty()
            && !tentanas_helper::block::DHCHAP_HASHES.contains(&row.dhchap_hash.as_str()))
            .then(|| format!("'{}' is not a DH-HMAC-CHAP hash nvmet accepts", row.dhchap_hash))
            .or_else(|| {
                (row.protocol == "nvmet"
                    && !row.dhchap_dhgroup.is_empty()
                    && !tentanas_helper::block::DHCHAP_DHGROUPS
                        .contains(&row.dhchap_dhgroup.as_str()))
                .then(|| {
                    format!(
                        "'{}' is not a DH-HMAC-CHAP DH group nvmet accepts",
                        row.dhchap_dhgroup
                    )
                })
            });
        if let Some(reason) = bad_dhchap {
            handle.log(format!("target {}: not imported — {reason}", target.name));
            step(handle, &mut done);
            continue;
        }
        if let Err(e) = super::targets::validate_options(&row, &imported, &block_caps, true) {
            handle.log(format!("target {}: not imported — {e}", target.name));
            step(handle, &mut done);
            continue;
        }
        store::upsert_target(&db, &row)?;
        imported.push(row.clone());
        targets_changed = true;
        handle.log(format!(
            "target {}: created{}",
            target.name,
            if authenticated {
                " (disabled until its secret is set)"
            } else if all_interfaces {
                " (disabled — it was exported on every interface; pick one on this node)"
            } else {
                ""
            }
        ));
        step(handle, &mut done);
    }

    // Host-object collisions among the rows this node now holds, reported ONCE
    // and only after every row has landed.
    //
    // Reported, not refused: none of these rows is in the kernel yet, so the
    // node would take every one of these saves and it is the second APPLY that
    // fails. Refusing here would break the property that lets a second check
    // exist beside `block::host_verdict` at all.
    //
    // AFTER the loop, and over the whole set, because the per-row version was
    // ORDER-DEPENDENT: an exported row carries no secret (§5.8 cannot carry
    // one), the save-time check skips a keyless `dhchap` sibling on purpose,
    // and so the pair was announced only when the authenticated row happened
    // to be written second. In the other order — the one an export of that
    // same pair produces — the import said nothing at all.
    for (name, warning) in super::targets::host_conflicts_in(&imported) {
        handle.log(format!("target {name}: {warning}"));
    }

    for (task, label, rows) in [
        (store::PoolTask::Scrub, "scrub", &document.schedules.scrub),
        (store::PoolTask::Trim, "trim", &document.schedules.trim),
    ] {
        for row in rows {
            let next = row
                .enabled
                .then(|| super::scheduler::next_run_utc(&row.schedule, chrono::Local::now()))
                .flatten();
            store::set_pool_schedule(&db, task, &row.pool, row.enabled, &row.schedule, next.as_deref())?;
            handle.log(format!("{label} schedule for {}: written", row.pool));
            step(handle, &mut done);
        }
    }
    for snapshot in &document.schedules.snapshot {
        // The same §5.10 rule the protocol handler enforces: an imported
        // document is a file somebody may have edited, and a schedule whose
        // retention cannot hold its own protection would take snapshots this
        // node can then never prune.
        if let Some((tier, days)) = super::snapshots::protection_shortfall(snapshot) {
            return Err(anyhow!(
                "snapshot schedule for {}: the '{tier}' retention keeps {days} days, less than the {} days of protection it hands out",
                snapshot.dataset,
                snapshot.protect_days
            ));
        }
        let next = snapshot
            .enabled
            .then(|| super::scheduler::next_run_utc(&snapshot.schedule, chrono::Local::now()))
            .flatten();
        store::upsert_snapshot_schedule(&db, snapshot, next.as_deref())?;
        handle.log(format!("snapshot schedule for {}: written", snapshot.dataset));
        step(handle, &mut done);
    }
    if document.schedules.smart.enabled {
        let now = chrono::Local::now();
        let smart = NasSmartSchedule {
            next_short_at: super::scheduler::next_run_utc(&document.schedules.smart.short, now),
            next_long_at: super::scheduler::next_run_utc(&document.schedules.smart.long, now),
            ..document.schedules.smart.clone()
        };
        store::set_smart_schedule(&db, &smart)?;
        handle.log("SMART schedule: written");
        step(handle, &mut done);
    }

    if shares_changed {
        for line in super::shares::apply(&db, main_db, addon_id, explicit).await? {
            handle.log(line);
        }
    }
    if targets_changed {
        // Only the unauthenticated ones are enabled, so this reconcile puts
        // exactly those into the kernel; the rest wait for their secret.
        let cipher = SettingsCipher::new(&crate::crypto::load_or_create_master_key()?);
        // Node-wide: an import can create, change and drop several targets at
        // once, so there is no single row to scope to.
        for line in super::targets::apply(&db, &cipher, explicit, None).await? {
            handle.log(line);
        }
    }
    handle.progress(100);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> ConfigDocument {
        ConfigDocument {
            schema: SCHEMA,
            exported_at: "2026-09-01T14:00:00Z".into(),
            node_id: "helios".into(),
            node_name: "helios".into(),
            pools: vec![
                PoolConfig {
                    name: "tank".into(),
                    guid: "111".into(),
                    layout: "raidz2".into(),
                    disks: vec![
                        DiskRef {
                            path: "/dev/disk/by-id/ata-A".into(),
                            serial: "ZR9AB12K".into(),
                        },
                        DiskRef {
                            path: "/dev/disk/by-id/ata-B".into(),
                            serial: "ZR18AB3F".into(),
                        },
                    ],
                },
                PoolConfig {
                    name: "backup".into(),
                    guid: "222".into(),
                    layout: "mirror".into(),
                    disks: vec![DiskRef {
                        path: "/dev/disk/by-id/ata-C".into(),
                        serial: "GONE1".into(),
                    }],
                },
            ],
            datasets: vec![
                DatasetConfig {
                    name: "tank/projekty".into(),
                    kind: "filesystem".into(),
                    properties: BTreeMap::from([("compression".to_string(), "zstd".to_string())]),
                },
                DatasetConfig {
                    name: "backup/old".into(),
                    kind: "filesystem".into(),
                    properties: BTreeMap::new(),
                },
            ],
            shares: vec![
                ShareConfig {
                    name: "projekty".into(),
                    protocol: "smb".into(),
                    source_path: "/mnt/tank/projekty".into(),
                    enabled: true,
                    fleet_mount: true,
                    smb: Some(NasSmbOptions::default()),
                    nfs: None,
                },
                ShareConfig {
                    name: "archiwum".into(),
                    protocol: "nfs".into(),
                    source_path: "/mnt/tank/backups".into(),
                    enabled: true,
                    fleet_mount: false,
                    smb: None,
                    nfs: Some(NasNfsOptions::default()),
                },
                ShareConfig {
                    name: "elsewhere".into(),
                    protocol: "smb".into(),
                    source_path: "/mnt/other/x".into(),
                    enabled: true,
                    fleet_mount: false,
                    smb: Some(NasSmbOptions::default()),
                    nfs: None,
                },
            ],
            share_users: vec![
                ShareUserConfig {
                    name: "anna".into(),
                    description: String::new(),
                },
                ShareUserConfig {
                    name: "jan".into(),
                    description: String::new(),
                },
            ],
            targets: vec![
                // Authenticated: no secret is in the export, so the import has
                // to bring it back disabled.
                TargetConfig {
                    name: "vm-store".into(),
                    protocol: "iscsi".into(),
                    wwn: "iqn.2026-09.local.tentaflow:helios.vm-store".into(),
                    enabled: true,
                    luns: vec![NasTargetLun {
                        index: 0,
                        source: "tank/vm-store".into(),
                        device_path: "/dev/zvol/tank/vm-store".into(),
                        size_bytes: 2_199_023_255_552,
                        thin: true,
                        uuid: "u1".into(),
                        group_id: 1,
                        source_kind: "zvol".into(),
                    }],
                    portals: vec![NasTargetPortal {
                        interface: "storage0".into(),
                        address: "10.10.0.5".into(),
                        port: 3260,
                        transport: "tcp".into(),
                    }],
                    port_groups: vec![NasTargetPortGroup {
                        group_id: 1,
                        state: "optimized".into(),
                        preferred: false,
                    }],
                    initiators: vec!["iqn.1998-01.com.vmware:esx01".into()],
                    auth_method: "mutual-chap".into(),
                    auth_username: "vmware01".into(),
                    auth_mutual_username: "helios".into(),
                    ..Default::default()
                },
                // Its zvol is not on this node.
                TargetConfig {
                    name: "scratch".into(),
                    protocol: "nvmet".into(),
                    wwn: "nqn.2026-09.local.tentaflow:helios.scratch".into(),
                    enabled: true,
                    luns: vec![NasTargetLun {
                        index: 1,
                        source: "fast/scratch".into(),
                        device_path: "/dev/zvol/fast/scratch".into(),
                        size_bytes: 536_870_912_000,
                        thin: true,
                        uuid: "u2".into(),
                        group_id: 1,
                        source_kind: "zvol".into(),
                    }],
                    portals: vec![NasTargetPortal {
                        interface: "storage0".into(),
                        address: "10.10.0.5".into(),
                        port: 4420,
                        transport: "tcp".into(),
                    }],
                    port_groups: vec![NasTargetPortGroup {
                        group_id: 1,
                        state: "optimized".into(),
                        preferred: false,
                    }],
                    auth_method: "none".into(),
                    ..Default::default()
                },
            ],
            schedules: SchedulesConfig {
                scrub: vec![PoolTaskConfig {
                    pool: "tank".into(),
                    enabled: true,
                    schedule: NasSchedule {
                        every: "weekly".into(),
                        ..Default::default()
                    },
                }],
                snapshot: vec![NasSnapshotSchedule {
                    dataset: "tank/projekty".into(),
                    enabled: true,
                    schedule: NasSchedule {
                        every: "15m".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                trim: Vec::new(),
                smart: NasSmartSchedule {
                    enabled: true,
                    ..Default::default()
                },
            },
        }
    }

    fn live() -> LiveState {
        LiveState {
            // `tank` is imported already; `backup` is not, and its disk is gone.
            pools: vec![("tank".to_string(), "111".to_string())],
            serials: vec!["ZR9AB12K".to_string(), "ZR18AB3F".to_string()],
            // `tank/vm-store` is here, `fast/scratch` is not.
            datasets: vec!["tank".to_string(), "tank/vm-store".to_string()],
            targets: Vec::new(),
            pool_mountpoints: vec!["/mnt/tank".to_string()],
            shares: vec![ShareRow {
                share_id: "s9".into(),
                name: "backups".into(),
                protocol: "nfs".into(),
                source_path: "/mnt/tank/backups".into(),
                ..Default::default()
            }],
            share_users: vec!["anna".to_string()],
            scrub_pools: vec!["tank".to_string()],
            trim_pools: vec![],
            snapshot_datasets: vec![],
            smart_enabled: false,
        }
    }

    fn find<'a>(items: &'a [NasConfigImportItem], kind: &str, name: &str) -> &'a NasConfigImportItem {
        items
            .iter()
            .find(|i| i.kind == kind && i.name == name)
            .unwrap_or_else(|| panic!("no {kind} '{name}' in the plan"))
    }

    #[test]
    fn the_plan_says_what_each_entry_would_do() {
        let (items, _) = plan(&document(), &live());
        assert_eq!(find(&items, "pool", "tank").action, "skip");
        let backup = find(&items, "pool", "backup");
        assert_eq!(backup.action, "missing");
        assert!(backup.detail.contains("GONE1"), "{}", backup.detail);

        assert_eq!(find(&items, "dataset", "tank/projekty").action, "create");
        // Its pool is not going to exist, so the dataset cannot be created.
        assert_eq!(find(&items, "dataset", "backup/old").action, "conflict");

        assert_eq!(find(&items, "share_user", "anna").action, "skip");
        let jan = find(&items, "share_user", "jan");
        assert_eq!(jan.action, "create");
        assert!(jan.detail.contains("password"), "{}", jan.detail);

        assert_eq!(find(&items, "share", "projekty").action, "create");
        // The path is already exported under another name.
        assert_eq!(find(&items, "share", "archiwum").action, "conflict");
        // Not under any pool this node has.
        assert_eq!(find(&items, "share", "elsewhere").action, "conflict");

        assert_eq!(find(&items, "schedule", "scrub tank").action, "update");
        assert_eq!(
            find(&items, "schedule", "snapshot tank/projekty").action,
            "create"
        );
        assert_eq!(find(&items, "schedule", "smart").action, "create");
    }

    #[test]
    fn an_import_of_the_same_node_warns_about_nothing() {
        let mut doc = document();
        doc.node_id = super::super::fleet_mounts::local_node_id();
        let (_, warnings) = plan(&doc, &live());
        assert!(warnings.is_empty(), "{warnings:?}");
        let (_, warnings) = plan(&document(), &live());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("helios"), "{}", warnings[0]);
    }

    #[test]
    fn a_share_that_already_exists_is_never_recreated() {
        let mut doc = document();
        doc.shares[0].name = "backups".into();
        doc.shares[0].source_path = "/mnt/tank/inne".into();
        let (items, _) = plan(&doc, &live());
        let existing = find(&items, "share", "backups");
        assert_eq!(existing.action, "skip");
        assert!(existing.detail.contains("/mnt/tank/backups"), "{}", existing.detail);
    }

    #[test]
    fn a_document_round_trips_and_refuses_a_foreign_schema() {
        let json = serde_json::to_string(&document()).expect("encode");
        let back = parse(&json).expect("parse");
        assert_eq!(back.pools.len(), 2);
        assert_eq!(back.shares.len(), 3);
        assert!(parse("{}").is_err());
        assert!(parse(r#"{"schema": 99}"#).is_err());
        // No secret ever leaves: the export has nowhere to put one.
        assert!(!json.contains("password"), "{json}");
    }

    #[test]
    fn the_backup_filename_is_node_and_time_scoped() {
        let mut doc = document();
        doc.node_name = "helios.lan".into();
        let name = filename(&doc);
        assert!(name.starts_with("tentanas-helios-lan-"), "{name}");
        assert!(name.ends_with(".json"), "{name}");
    }

    /// §5.8: the export carries the targets and NEVER a CHAP secret. The
    /// document has no field to put one in, which is the point — there is no
    /// way to write one by accident.
    #[test]
    fn the_export_carries_a_target_without_any_way_to_carry_its_secret() {
        let doc = document();
        let json = serde_json::to_string_pretty(&doc).expect("encode");
        assert!(json.contains("\"vm-store\""), "the target is in the export");
        assert!(json.contains("\"mutual-chap\""), "the method is in the export");
        assert!(json.contains("\"vmware01\""), "the user name is in the export");
        // Not "the value is absent" — the words themselves are absent, so a
        // future field cannot smuggle one in unnoticed.
        for forbidden in ["secret", "password", "dhchap_key", "auth_secret"] {
            assert!(!json.contains(forbidden), "'{forbidden}' is in the export:\n{json}");
        }
        // And it survives the round trip the import reads it back with.
        let back = parse(&json).expect("parse");
        assert_eq!(back.targets.len(), 2);
        assert_eq!(back.targets[0].auth_method, "mutual-chap");
        assert_eq!(back.targets[0].port_groups[0].state, "optimized");

        // A document written before targets existed is still readable: the
        // field is defaulted, not schema-bumped.
        let mut older = serde_json::to_value(&doc).expect("encode");
        older.as_object_mut().expect("object").remove("targets");
        let older: ConfigDocument =
            serde_json::from_value(older).expect("a pre-N3 export still parses");
        assert!(older.targets.is_empty());
    }

    #[test]
    fn an_imported_target_needs_its_zvol_and_comes_back_disabled_when_it_authenticates() {
        let (items, _) = plan(&document(), &live());
        let vm = find(&items, "target", "vm-store");
        assert_eq!(vm.action, "create");
        assert!(vm.detail.contains("secret has to be set again"), "{}", vm.detail);
        // The zvol of the second target is not on this node.
        let scratch = find(&items, "target", "scratch");
        assert_eq!(scratch.action, "conflict");
        assert!(scratch.detail.contains("fast/scratch"), "{}", scratch.detail);

        // A target that is already here is skipped rather than replaced.
        let mut live = live();
        live.targets = vec![store::TargetRow {
            name: "vm-store".into(),
            wwn: "iqn.2026-09.local.tentaflow:helios.vm-store".into(),
            ..Default::default()
        }];
        let (items, _) = plan(&document(), &live);
        assert_eq!(find(&items, "target", "vm-store").action, "skip");

        // A DIFFERENT target already exporting the same volume is a conflict:
        // two targets on one zvol is two clients writing one raw disk.
        live.targets = vec![store::TargetRow {
            name: "inny".into(),
            wwn: "iqn.2026-09.local.tentaflow:helios.inny".into(),
            luns: vec![NasTargetLun {
                source: "tank/vm-store".into(),
                source_kind: "zvol".into(),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let (items, _) = plan(&document(), &live);
        let vm = find(&items, "target", "vm-store");
        assert_eq!(vm.action, "conflict");
        assert!(vm.detail.contains("already exports"), "{}", vm.detail);
        // An import never OVERWRITES a target: it creates what is missing,
        // skips what is there and flags what collides. Asserted over the
        // target rows themselves — `overwritten` filters the whole plan for
        // "update", and other kinds (schedules) legitimately do update, so
        // asking it about the plan says nothing about targets.
        let target_actions: Vec<&str> = items
            .iter()
            .filter(|i| i.kind == "target")
            .map(|i| i.action.as_str())
            .collect();
        assert!(!target_actions.is_empty(), "the fixture has targets to judge");
        assert!(
            target_actions.iter().all(|a| *a != "update"),
            "the target planner creates, skips or conflicts — it never overwrites: {target_actions:?}"
        );
        assert!(!overwritten(&items).iter().any(|n| n == "vm-store" || n == "scratch"));
    }

    #[tokio::test]
    async fn the_import_itself_reports_a_host_collision_it_creates() {
        // `host_conflicts_in` was tested as a function and never REACHED
        // through `config_io::apply` — the same "the function is tested, its
        // wiring is not" shape that let a whole family ship unregistered.
        //
        // Driving the real loop needed two seams, both of which this codebase
        // already uses everywhere else: `apply` read its own live state (a
        // `zfs`/`zpool` shell-out, so on this machine every target is judged
        // "its zvol is not here" and skipped) and probed its own block
        // capabilities (so every nvmet target would be refused before the loop
        // reached anything). Injected, the body runs for real.
        let conn = rusqlite::Connection::open_in_memory().expect("db");
        super::super::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let handle = super::super::jobs::JobHandle::for_test(&db, "job-import-collision");

        let esx = "nqn.2014-08.org.nvmexpress:uuid:esx01";
        let target = |name: &str, method: &str| TargetConfig {
            name: name.to_string(),
            protocol: "nvmet".to_string(),
            wwn: super::super::targets::wwn_for("nvmet", "helios", name),
            enabled: true,
            luns: vec![NasTargetLun {
                index: 1,
                source: format!("tank/{name}"),
                source_kind: "zvol".to_string(),
                device_path: format!("/dev/zvol/tank/{name}"),
                size_bytes: 1024,
                thin: true,
                uuid: format!("uuid-{name}"),
                group_id: 1,
                ..Default::default()
            }],
            portals: vec![NasTargetPortal {
                interface: "storage0".to_string(),
                address: "10.10.0.5".to_string(),
                port: 4420,
                transport: "tcp".to_string(),
            }],
            port_groups: super::super::targets::default_port_groups(),
            initiators: vec![esx.to_string()],
            auth_method: method.to_string(),
            auth_username: String::new(),
            auth_mutual_username: String::new(),
            dhchap_hash: "hmac(sha256)".to_string(),
            dhchap_dhgroup: "null".to_string(),
        };
        let mut document = document();
        document.targets = vec![target("vm-a", "dhchap"), target("vm-b", "none")];
        document.shares.clear();
        document.datasets.clear();
        document.pools.clear();

        // The node as this import needs to see it: the two zvols are here, so
        // the rows are CREATED rather than flagged as missing sources.
        let mut live = live();
        live.datasets = vec!["tank/vm-a".to_string(), "tank/vm-b".to_string()];
        live.targets = Vec::new();
        live.shares = Vec::new();

        let caps = NasBlockCapabilities {
            iscsi: true,
            nvmet: true,
            dhchap: true,
            ..Default::default()
        };
        apply_with(&handle, &db, "tentanas", document, None, live, Some(caps))
            .await
            .expect("import");

        // Both rows landed…
        let rows = store::list_targets(&db).expect("targets");
        assert_eq!(rows.len(), 2, "{rows:?}");
        // …and the job log — the only place an admin sees this — carries the
        // collision, named by the host and by the other target.
        let log = store::job(&db, "job-import-collision")
            .expect("read")
            .expect("the job row")
            .log;
        let collision = log
            .iter()
            .find(|l| l.contains("shares host"))
            .unwrap_or_else(|| panic!("the import said nothing about the collision:\n{log:#?}"));
        assert!(collision.contains(esx), "{collision}");
        assert!(collision.contains("vm-a"), "{collision}");
        assert!(collision.contains("only one of these two targets can be applied"), "{collision}");
    }

    #[test]
    fn two_imported_nvmet_targets_sharing_a_host_are_reported_in_either_order() {
        // The claim the import path makes about itself, tested rather than
        // asserted in a comment: a document carrying two nvmet targets that
        // disagree about one node-wide host object says so.
        //
        // The rows are the shape IMPORT ACTUALLY CREATES — `auth_secret`
        // EMPTY, because an export carries no secrets at all (§5.8). The
        // previous version of this test built rows with a stored secret, which
        // import never produces, and so it passed while the real path was
        // silent: the save-time check deliberately exempts a keyless `dhchap`
        // sibling, and at import every authenticated row is keyless.
        let base = |name: &str, method: &str| super::super::db::TargetRow {
            target_id: format!("0191f2c0-0000-7000-8000-0000000000{}", &name[name.len() - 1..]),
            name: name.to_string(),
            protocol: "nvmet".into(),
            wwn: super::super::targets::wwn_for("nvmet", "helios", name),
            auth_method: method.into(),
            // Empty, always: this is what an imported row looks like.
            auth_secret: String::new(),
            initiators: vec!["nqn.2014-08.org.nvmexpress:uuid:esx01".into()],
            ..Default::default()
        };
        let authenticated = base("vm-a", "dhchap");
        let open = base("vm-b", "none");

        // ORDER-INDEPENDENT, which the per-row version was not: it reported
        // the pair only when the authenticated row happened to be written
        // second, and stayed silent in the order an export of that same pair
        // produces.
        for rows in [
            vec![authenticated.clone(), open.clone()],
            vec![open.clone(), authenticated.clone()],
        ] {
            let found = super::super::targets::host_conflicts_in(&rows);
            assert_eq!(found.len(), 1, "one pair, one sentence: {found:?}");
            let (named, warning) = &found[0];
            assert_eq!(named, &rows[1].name, "reported against the later row");
            assert!(warning.contains("nqn.2014-08.org.nvmexpress:uuid:esx01"), "{warning}");
            assert!(warning.contains(&rows[0].name), "{warning}");
            assert!(warning.contains("only one of these two targets can be applied"), "{warning}");
        }

        // The counter-examples, so this is not a function that always fires.
        // Same method — the keys may still differ, but that is the node's
        // question and the core cannot answer it:
        let mut same = open.clone();
        same.auth_method = "dhchap".into();
        assert!(super::super::targets::host_conflicts_in(&[authenticated.clone(), same]).is_empty());
        // A different host NQN shares no object:
        let mut elsewhere = open.clone();
        elsewhere.initiators = vec!["nqn.2014-08.org.nvmexpress:uuid:esx02".into()];
        assert!(
            super::super::targets::host_conflicts_in(&[authenticated.clone(), elsewhere]).is_empty()
        );
        // An iSCSI neighbour's allowlist is IQNs on a TPG, not a host object:
        let mut iscsi = authenticated.clone();
        iscsi.protocol = "iscsi".into();
        iscsi.auth_method = "chap".into();
        assert!(super::super::targets::host_conflicts_in(&[iscsi, open.clone()]).is_empty());
        // And a single row is never its own neighbour.
        assert!(super::super::targets::host_conflicts_in(&[open]).is_empty());
    }
}
