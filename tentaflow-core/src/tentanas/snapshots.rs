// =============================================================================
// File: tentanas/snapshots.rs — snapshots and their GFS retention (plan-02
//       §5.2, tab "Snapshoty"). `zfs list -t snapshot -Hp` is the whole read
//       side; the interesting logic is what the scheduler is allowed to
//       DELETE, which lives here as a pure function over a snapshot list so a
//       fixed clock can test it.
//
//       Naming is the contract between the scheduler and the UI: an automatic
//       snapshot is `auto-<YYYYMMDD>-<HHMM>-<tier>`. Anything else is manual
//       and pruning never touches it — neither does it touch a snapshot that
//       is held or that a clone still depends on.
// =============================================================================

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use tentaflow_protocol::tentanas::{NasDirEntry, NasSnapshot, NasSnapshotSchedule};

use super::broker::BrokerError;
use super::zfs;

/// `zfs list -t snapshot` columns, in parser order.
pub const LIST_COLUMNS: &str = "name,used,refer,creation,userrefs,clones";

/// Retention buckets, coarsest last — the order pruning reports them in.
pub const TIERS: &[&str] = &["frequent", "hourly", "daily", "weekly", "monthly"];

/// The name an automatic snapshot of `tier` taken at `at` (node local time)
/// gets. The timestamp is local because the retention the admin configured is
/// local: an "hourly at :05" snapshot must read as 05 past the hour on the
/// node, whatever UTC says.
pub fn auto_name(at: chrono::DateTime<chrono::Local>, tier: &str) -> String {
    format!("auto-{}-{tier}", at.format("%Y%m%d-%H%M"))
}

/// Splits an automatic snapshot's short name into its tier, or None when the
/// snapshot was not made by a schedule.
pub fn tier_of(short_name: &str) -> Option<&'static str> {
    let rest = short_name.strip_prefix("auto-")?;
    let mut parts = rest.split('-');
    let date = parts.next()?;
    let time = parts.next()?;
    let tier = parts.next()?;
    if parts.next().is_some()
        || date.len() != 8
        || !date.bytes().all(|b| b.is_ascii_digit())
        || time.len() != 4
        || !time.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    TIERS.iter().copied().find(|t| *t == tier)
}

pub fn parse_list(text: &str) -> Vec<NasSnapshot> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 6 {
                return None;
            }
            let name = f[0].trim().to_string();
            let (dataset, short_name) = name.split_once('@')?;
            let tier = tier_of(short_name);
            Some(NasSnapshot {
                dataset: dataset.to_string(),
                short_name: short_name.to_string(),
                used_bytes: zfs::u64_field(f[1]),
                referenced_bytes: zfs::u64_field(f[2]),
                created_at: zfs::field(f[3])
                    .and_then(|v| v.parse::<i64>().ok())
                    .map(zfs::epoch_to_rfc3339)
                    .unwrap_or_default(),
                holds: zfs::u64_field(f[4]) as u32,
                clones: zfs::field(f[5])
                    .map(|c| c.split(',').map(str::to_string).collect())
                    .unwrap_or_default(),
                origin: if tier.is_some() { "auto" } else { "manual" }.to_string(),
                tier: tier.unwrap_or_default().to_string(),
                name,
            })
        })
        .collect()
}

// ----- GFS retention ----------------------------------------------------------------

/// How many snapshots of each tier survive a pruning pass. A count of 0 is
/// what the protocol calls a disabled tier: nothing new is taken and nothing
/// old is kept, so turning a tier off actually frees its space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keep {
    pub frequent: u32,
    pub hourly: u32,
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}

impl Keep {
    pub fn of(&self, tier: &str) -> u32 {
        match tier {
            "frequent" => self.frequent,
            "hourly" => self.hourly,
            "daily" => self.daily,
            "weekly" => self.weekly,
            "monthly" => self.monthly,
            _ => 0,
        }
    }

    pub fn from_schedule(schedule: &NasSnapshotSchedule) -> Self {
        Self {
            frequent: schedule.keep_frequent,
            hourly: schedule.keep_hourly,
            daily: schedule.keep_daily,
            weekly: schedule.keep_weekly,
            monthly: schedule.keep_monthly,
        }
    }
}

/// Whether a snapshot may be destroyed by a schedule at all. A hold is an
/// explicit "do not delete" and a clone makes the snapshot the origin of a
/// live dataset — destroying either is data loss the schedule never intends.
pub fn is_prunable(snapshot: &NasSnapshot) -> bool {
    snapshot.origin == "auto" && snapshot.holds == 0 && snapshot.clones.is_empty()
}

/// The snapshots a pruning pass would destroy: per tier, everything past the
/// newest `keep` of that tier. Protected snapshots still occupy a retention
/// slot — they exist and the admin can see them — they are simply never
/// selected for destruction.
pub fn prune_selection(snapshots: &[NasSnapshot], keep: &Keep) -> Vec<String> {
    let mut doomed = Vec::new();
    for tier in TIERS {
        let mut of_tier: Vec<&NasSnapshot> = snapshots
            .iter()
            .filter(|s| s.origin == "auto" && s.tier == *tier)
            .collect();
        // Newest first: `created_at` is RFC 3339 UTC, so string order is time
        // order, and the full name breaks ties deterministically.
        of_tier.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.name.cmp(&a.name))
        });
        for snapshot in of_tier.into_iter().skip(keep.of(tier) as usize) {
            if is_prunable(snapshot) {
                doomed.push(snapshot.name.clone());
            }
        }
    }
    doomed
}

// ----- live reads ---------------------------------------------------------------------

/// Snapshots of one dataset (with `recursive`, of its children too), of one
/// pool, or of the whole node. Newest first.
pub async fn list(pool: &str, dataset: &str, recursive: bool) -> Result<Vec<NasSnapshot>, BrokerError> {
    let mut args: Vec<&str> = vec!["list", "-Hp", "-t", "snapshot", "-o", LIST_COLUMNS];
    if !dataset.is_empty() {
        tentanas_helper::validate_dataset_name(dataset)
            .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
        // Depth 1 of a dataset is exactly its own snapshots; `-r` walks the
        // children too.
        if recursive {
            args.push("-r");
        } else {
            args.extend(["-d", "1"]);
        }
        args.push(dataset);
    } else if !pool.is_empty() {
        tentanas_helper::validate_pool_name(pool)
            .map_err(|e| BrokerError::InvalidArgument(e.to_string()))?;
        args.extend(["-r", pool]);
    }
    let text = zfs::zfs(&args).await?;
    let mut rows = parse_list(&text);
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(rows)
}

// ----- browsing a snapshot -------------------------------------------------------------

/// Where ZFS exposes a dataset's snapshots read-only, below its mountpoint.
const SNAPSHOT_DIR: &str = ".zfs/snapshot";

/// Joins `relative` under `root` and refuses anything that leaves it. Pure so
/// the escape rules are tested without a pool: the check is on the RESOLVED
/// paths, because a symlink inside a snapshot points wherever it pointed when
/// the snapshot was taken — including out of the pool.
fn resolve_within(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.starts_with('/') {
        return Err(anyhow!("'{relative}' must be relative to the snapshot root"));
    }
    for part in relative.split('/') {
        if part == ".." {
            return Err(anyhow!("'{relative}' leaves the snapshot"));
        }
    }
    let target = if relative.is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    // `.zfs/snapshot/<name>` is itself a mountpoint ZFS materializes on
    // access, so both sides are canonicalized: comparing a lexical join
    // against a resolved path would reject every valid request.
    let canonical_root = std::fs::canonicalize(root)
        .map_err(|e| anyhow!("the snapshot cannot be opened: {e}"))?;
    let canonical = std::fs::canonicalize(&target)
        .map_err(|e| anyhow!("'{relative}' cannot be opened in this snapshot: {e}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(anyhow!("'{relative}' leaves the snapshot"));
    }
    Ok(canonical)
}

/// Lists one directory inside `dataset@name`, as
/// `<dataset mountpoint>/.zfs/snapshot/<name>/<path>`. Entirely unprivileged:
/// the snapshot directory is readable by whoever can read the dataset, so
/// nothing here needs the channel. Directories only, like the share browser —
/// the two are the same picker and a returned entry must be somewhere the
/// caller can descend into.
pub async fn browse(snapshot: &str, path: &str) -> Result<(String, Vec<NasDirEntry>)> {
    tentanas_helper::validate_snapshot_name(snapshot).map_err(|e| anyhow!(e.to_string()))?;
    let (dataset, short_name) = snapshot
        .split_once('@')
        .ok_or_else(|| anyhow!("'{snapshot}' is not a snapshot name"))?;
    let row = super::datasets::get(dataset)
        .await
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("dataset '{dataset}' is not on this node"))?;
    let mountpoint = row
        .mountpoint
        .filter(|m| m.starts_with('/'))
        .ok_or_else(|| anyhow!("dataset '{dataset}' has no mountpoint to browse"))?;
    let root = Path::new(&mountpoint).join(SNAPSHOT_DIR).join(short_name);
    let dir = resolve_within(&root, path)?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .map_err(|e| anyhow!("'{}' cannot be listed: {e}", dir.display()))?
        .flatten()
    {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let child = if path.is_empty() {
            name.clone()
        } else {
            format!("{}/{name}", path.trim_end_matches('/'))
        };
        entries.push(NasDirEntry {
            name,
            // Paths stay RELATIVE to the snapshot root: the absolute one leaks
            // the `.zfs` plumbing and is not what the next request wants.
            path: child,
            dataset: None,
            shared_as: Vec::new(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((path.to_string(), entries))
}

// ----- jobs ------------------------------------------------------------------------

const SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The scheduler's snapshot run: one snapshot per crossed tier. Retention is
/// a second job the run chains after itself, so the Tasks tab shows what was
/// taken and what was removed as two lines with their own logs — a failed
/// prune never looks like a failed snapshot.
pub fn spawn_auto(
    db: &crate::db::DbPool,
    schedule: &NasSnapshotSchedule,
    tiers: Vec<String>,
    now: chrono::DateTime<chrono::Local>,
) -> anyhow::Result<tentaflow_protocol::tentanas::NasJob> {
    let dataset = schedule.dataset.clone();
    let recursive = schedule.recursive;
    let keep = Keep::from_schedule(schedule);
    super::jobs::spawn(
        db,
        "snapshot_auto",
        &schedule.dataset,
        super::scheduler::STARTED_BY,
        move |h| auto_job(h, dataset, recursive, tiers, keep, now),
    )
}

/// Retention on its own: what the scheduler chains after a snapshot run, and
/// what re-running the retention of a dataset would use.
pub fn spawn_prune(
    db: &crate::db::DbPool,
    dataset: &str,
    recursive: bool,
    keep: Keep,
    started_by: &str,
) -> anyhow::Result<tentaflow_protocol::tentanas::NasJob> {
    let subject = dataset.to_string();
    super::jobs::spawn(db, "snapshot_prune", dataset, started_by, move |h| {
        prune_job(h, subject, recursive, keep)
    })
}

async fn auto_job(
    h: super::jobs::JobHandle,
    dataset: String,
    recursive: bool,
    tiers: Vec<String>,
    keep: Keep,
    now: chrono::DateTime<chrono::Local>,
) -> anyhow::Result<()> {
    if tiers.is_empty() {
        h.log("every retention tier is disabled — nothing to take");
        return Ok(());
    }
    for tier in &tiers {
        let command = tentanas_helper::HelperCommand::ZfsSnapshot {
            snapshot: format!("{dataset}@{}", auto_name(now, tier)),
            recursive,
        };
        super::jobs::run_step(&h, &command, None, SNAPSHOT_TIMEOUT).await?;
    }
    h.progress(100);
    let pruning = spawn_prune(h.db(), &dataset, recursive, keep, super::scheduler::STARTED_BY)?;
    h.log(format!("retention runs as job {}", pruning.job_id));
    Ok(())
}

/// Destroys what GFS retention no longer keeps.
async fn prune_job(
    h: super::jobs::JobHandle,
    dataset: String,
    recursive: bool,
    keep: Keep,
) -> anyhow::Result<()> {
    let existing = list("", &dataset, recursive).await?;
    let doomed = prune_selection(&existing, &keep);
    if doomed.is_empty() {
        h.log("retention satisfied — nothing to prune");
        h.progress(100);
        return Ok(());
    }
    h.log(format!("pruning {} snapshots past retention", doomed.len()));
    for name in doomed {
        let command = tentanas_helper::HelperCommand::ZfsDestroy {
            name,
            recursive: false,
        };
        super::jobs::run_step(&h, &command, None, SNAPSHOT_TIMEOUT).await?;
    }
    h.progress(100);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(name: &str, created_at: &str) -> NasSnapshot {
        let (dataset, short_name) = name.split_once('@').expect("snapshot name");
        let tier = tier_of(short_name);
        NasSnapshot {
            name: name.to_string(),
            dataset: dataset.to_string(),
            short_name: short_name.to_string(),
            created_at: created_at.to_string(),
            origin: if tier.is_some() { "auto" } else { "manual" }.to_string(),
            tier: tier.unwrap_or_default().to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_rows_carry_holds_clones_and_the_tier() {
        let text = "tank/projekty@auto-20260901-1445-frequent\t18874368\t18100000000000\t1756738700\t0\t-\n\
tank/projekty@auto-20260901-0000-daily\t1181116006\t18100000000000\t1756684800\t0\t-\n\
tank/projekty@przed-migracja-bazy\t9019431321\t18100000000000\t1756664520\t1\t-\n\
tank/projekty@auto-20260801-0000-monthly\t15247343616\t18000000000000\t1754006400\t0\ttank/klon,tank/klon2\n";
        let rows = parse_list(text);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].dataset, "tank/projekty");
        assert_eq!(rows[0].short_name, "auto-20260901-1445-frequent");
        assert_eq!(rows[0].origin, "auto");
        assert_eq!(rows[0].tier, "frequent");
        assert_eq!(rows[0].used_bytes, 18_874_368);
        assert!(rows[0].clones.is_empty());
        assert_eq!(rows[1].tier, "daily");
        assert_eq!(rows[2].origin, "manual");
        assert_eq!(rows[2].tier, "");
        assert_eq!(rows[2].holds, 1);
        assert_eq!(rows[3].clones, ["tank/klon", "tank/klon2"]);
        assert!(rows[0].created_at.ends_with('Z'));
    }

    #[test]
    fn only_well_formed_auto_names_carry_a_tier() {
        assert_eq!(tier_of("auto-20260901-1445-frequent"), Some("frequent"));
        assert_eq!(tier_of("auto-20260901-0000-monthly"), Some("monthly"));
        assert_eq!(tier_of("auto-20260901-1445-yearly"), None);
        assert_eq!(tier_of("auto-2026091-1445-daily"), None);
        assert_eq!(tier_of("auto-20260901-144-daily"), None);
        assert_eq!(tier_of("auto-20260901-1445-daily-extra"), None);
        assert_eq!(tier_of("przed-migracja-bazy"), None);
        assert_eq!(tier_of("daily-20260831"), None);
    }

    #[test]
    fn auto_names_round_trip_through_the_tier_parser() {
        use chrono::TimeZone;
        let at = chrono::Local
            .with_ymd_and_hms(2026, 9, 1, 14, 45, 0)
            .single()
            .expect("local time");
        let name = auto_name(at, "frequent");
        assert_eq!(name, "auto-20260901-1445-frequent");
        assert_eq!(tier_of(&name), Some("frequent"));
    }

    #[test]
    fn gfs_pruning_keeps_the_newest_of_every_tier() {
        let keep = Keep {
            frequent: 2,
            hourly: 1,
            daily: 2,
            weekly: 0,
            monthly: 1,
        };
        let snapshots = vec![
            snap("t/p@auto-20260901-1445-frequent", "2026-09-01T14:45:00Z"),
            snap("t/p@auto-20260901-1430-frequent", "2026-09-01T14:30:00Z"),
            snap("t/p@auto-20260901-1415-frequent", "2026-09-01T14:15:00Z"),
            snap("t/p@auto-20260901-1400-frequent", "2026-09-01T14:00:00Z"),
            snap("t/p@auto-20260901-1400-hourly", "2026-09-01T14:00:00Z"),
            snap("t/p@auto-20260901-1300-hourly", "2026-09-01T13:00:00Z"),
            snap("t/p@auto-20260901-0000-daily", "2026-09-01T00:00:00Z"),
            snap("t/p@auto-20260831-0000-daily", "2026-08-31T00:00:00Z"),
            snap("t/p@auto-20260830-0000-daily", "2026-08-30T00:00:00Z"),
            snap("t/p@auto-20260830-0000-weekly", "2026-08-30T00:00:00Z"),
            snap("t/p@auto-20260801-0000-monthly", "2026-08-01T00:00:00Z"),
            snap("t/p@przed-migracja-bazy", "2026-08-31T18:22:00Z"),
        ];
        let doomed = prune_selection(&snapshots, &keep);
        assert_eq!(
            doomed,
            vec![
                "t/p@auto-20260901-1415-frequent",
                "t/p@auto-20260901-1400-frequent",
                "t/p@auto-20260901-1300-hourly",
                "t/p@auto-20260830-0000-daily",
                // keep_weekly = 0 disables the tier: nothing of it survives.
                "t/p@auto-20260830-0000-weekly",
            ]
        );
        // The manual snapshot is never a candidate, whatever the counts say.
        assert!(!doomed.iter().any(|n| n.contains("przed-migracja")));
    }

    #[test]
    fn holds_and_clones_survive_pruning_but_still_use_their_slot() {
        let keep = Keep {
            frequent: 1,
            ..Default::default()
        };
        let mut held = snap("t/p@auto-20260901-1430-frequent", "2026-09-01T14:30:00Z");
        held.holds = 1;
        let mut cloned = snap("t/p@auto-20260901-1415-frequent", "2026-09-01T14:15:00Z");
        cloned.clones = vec!["t/klon".to_string()];
        let snapshots = vec![
            snap("t/p@auto-20260901-1445-frequent", "2026-09-01T14:45:00Z"),
            held,
            cloned,
            snap("t/p@auto-20260901-1400-frequent", "2026-09-01T14:00:00Z"),
        ];
        let doomed = prune_selection(&snapshots, &keep);
        assert_eq!(doomed, vec!["t/p@auto-20260901-1400-frequent"]);
    }

    #[test]
    fn nothing_is_pruned_while_the_tiers_are_not_full() {
        let keep = Keep {
            frequent: 96,
            daily: 30,
            monthly: 12,
            ..Default::default()
        };
        let snapshots = vec![
            snap("t/p@auto-20260901-1445-frequent", "2026-09-01T14:45:00Z"),
            snap("t/p@auto-20260901-0000-daily", "2026-09-01T00:00:00Z"),
            snap("t/p@auto-20260801-0000-monthly", "2026-08-01T00:00:00Z"),
        ];
        assert!(prune_selection(&snapshots, &keep).is_empty());
    }

    #[test]
    fn the_snapshot_browser_refuses_to_leave_the_snapshot() {
        let root = std::env::temp_dir().join("tentanas-browse-test/root");
        let outside = std::env::temp_dir().join("tentanas-browse-test/outside");
        std::fs::create_dir_all(root.join("projekty")).expect("fixture");
        std::fs::create_dir_all(&outside).expect("fixture");

        assert_eq!(resolve_within(&root, "").expect("root"), std::fs::canonicalize(&root).unwrap());
        assert!(resolve_within(&root, "projekty").is_ok());
        // A `..` component is refused before the filesystem is touched…
        assert!(resolve_within(&root, "../outside").is_err());
        assert!(resolve_within(&root, "projekty/../../outside").is_err());
        // …an absolute path never means "inside the snapshot"…
        assert!(resolve_within(&root, "/etc").is_err());
        // …and a symlink that resolves out of the snapshot is caught after it.
        #[cfg(unix)]
        {
            let link = root.join("escape");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&outside, &link).expect("symlink");
            assert!(resolve_within(&root, "escape").is_err());
        }
        // A path that does not exist is an error, not a silently empty listing.
        assert!(resolve_within(&root, "nie-ma").is_err());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("tentanas-browse-test"));
    }
}
