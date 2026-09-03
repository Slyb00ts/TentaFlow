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
    NasConfigImportItem, NasNfsOptions, NasSchedule, NasSmartSchedule, NasSmbOptions,
    NasSnapshotSchedule,
};
use tentanas_helper::HelperCommand;

use super::db::{self as store, ShareRow};
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulesConfig {
    pub scrub: Vec<ScrubConfig>,
    pub snapshot: Vec<NasSnapshotSchedule>,
    pub smart: NasSmartSchedule,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScrubConfig {
    pub pool: String,
    pub enabled: bool,
    pub schedule: NasSchedule,
}

// =============================================================================
// export
// =============================================================================

fn hostname() -> String {
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

    let scrub = store::list_scrub_schedules(db)?
        .into_iter()
        .map(|s| ScrubConfig {
            pool: s.pool,
            enabled: s.enabled,
            schedule: s.schedule,
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
        schedules: SchedulesConfig {
            scrub,
            snapshot: store::list_snapshot_schedules(db)?,
            smart: store::smart_schedule(db)?,
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
    pub share_users: Vec<String>,
    pub scrub_pools: Vec<String>,
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
        share_users: store::list_share_users(db)?
            .into_iter()
            .map(|u| u.name)
            .collect(),
        scrub_pools: store::list_scrub_schedules(db)?
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

    for scrub in &document.schedules.scrub {
        let action = if live.scrub_pools.contains(&scrub.pool) {
            "update"
        } else {
            "create"
        };
        items.push(item(
            "schedule",
            &format!("scrub {}", scrub.pool),
            action,
            scrub.schedule.every.clone(),
        ));
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

    for scrub in &document.schedules.scrub {
        let next = scrub
            .enabled
            .then(|| super::scheduler::next_run_utc(&scrub.schedule, chrono::Local::now()))
            .flatten();
        store::set_scrub_schedule(&db, &scrub.pool, scrub.enabled, &scrub.schedule, next.as_deref())?;
        handle.log(format!("scrub schedule for {}: written", scrub.pool));
        step(handle, &mut done);
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
            schedules: SchedulesConfig {
                scrub: vec![ScrubConfig {
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
            datasets: vec!["tank".to_string()],
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
}
