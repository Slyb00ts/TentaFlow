// =============================================================================
// File: tentanas/elastic.rs — the Elastic Array model (plan-02 §5.3): mergerfs
// over cache + data disks, SnapRAID parity over the data disks, and a mover
// that carries files from one to the other underneath a path that never
// changes.
//
// What lives here and what does not:
//   * here — the model, the layout refusals, the state/verdict function the
//     apply path filters on, the protection window, and when the mover is due;
//   * `tentanas_helper::elastic` — the privileged plan those decisions produce
//     (mkfs, mounts, the snapraid config, sync/scrub), rendered by one
//     function so the preview and the action cannot disagree;
//   * create/restore korzystają z transakcji NAS i journala helpera; odczyt
//     łączy trwałą intencję z aktualnym pomiarem bez zgadywania brakujących danych.
//
// THE THREE FACTS THAT SHAPE EVERY DECISION BELOW
//
// 1. ONE union, and the cache is inside it. mergerfs spans the cache disk AND
//    the data disks as branches of a single mount at `/mnt/<array>`. Shares
//    and folders name that path and nothing else, so the mover moves files
//    BETWEEN BRANCHES under it and no client ever sees a path change. Every
//    function here that touches paths goes through
//    `tentanas_helper::elastic`'s path helpers rather than building one, so
//    there is exactly one place that could get this wrong.
//
// 2. Parity is what the last `sync` saw. SnapRAID is not RAID: it protects
//    the state of the data disks at the moment of the last sync, it never
//    covers the cache at all, and a file the mover has just moved down is
//    outside parity until the next sync runs. That gap has a size in bytes,
//    it is the first thing n11 shows, and `protection` computes it.
//
// 3. Unknown is not zero. Every measured quantity is an `Option`, and a
//    verdict that would ACT on a missing measurement refuses instead. The
//    sharpest case is `BranchProbe::mounted`: reading `None` as "not mounted"
//    would mount a union over empty directories and send every client write to
//    the root filesystem — the §3.4 empty-share trap with the whole data path
//    behind it.
// =============================================================================

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use tentaflow_protocol::features::FeatureState;
use tentaflow_protocol::tentanas::{
    NasDisk, NasElasticArray, NasElasticBranch, NasElasticCapabilities, NasElasticFolder,
    NasElasticImportCandidate, NasElasticImportMember, NasElasticParity, NasElasticPendingAdd, NasElasticPlan,
    NasElasticProtection,
    NasElasticRefusal, NasHealthReason, NasMoverRun, NasMoverSettings, NasSchedule, NasSnapraidRun,
    NasSnapraidState,
};
use tentanas_helper::elastic::{
    cache_branch_path, config_path, data_branch_path, parity_file_path, parity_mount_path,
    union_path, Branch as SpecBranch, ElasticSpec, MergerfsOptions, MoverRules,
    ParityDisk as SpecParity, SnapraidOptions, Tools,
};
use anyhow::{anyhow, ensure, Result};
use tentanas_helper::elastic::{ElasticCreateSpec, ElasticDiskSpec, ElasticJournalEntry, ElasticJournalsResult, ElasticUnreadableJournal, ElasticConflict, ElasticOwner, ElasticResult, ElasticRole, ElasticStage, ElasticClaimsResult, ElasticServiceMode};
use tentanas_helper::elastic::{ElasticCacheAge, ElasticDissolveResult, ElasticMoverIssueKind, ElasticMoverPhase, ElasticMoverResult, ElasticMoverRun, ElasticSnapraidKind, ElasticSnapraidOutcome, ElasticSnapraidResult};
use tentanas_helper::HelperCommand;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use super::{db as store, jobs, CodedText};
use std::sync::{Arc, Mutex, OnceLock};

/// The machine kind of §5.3, next to a ZFS pool's `zfs`. The SPEC fixes the
/// spelling: "Elastic Array" in prose, `elastic-array` on the wire, and never
/// the trademarked name of the product this resembles.
pub const KIND: &str = "elastic-array";

/// Environment feature ids of the two tools an array needs.
pub const MERGERFS_FEATURE_ID: &str = "mergerfs";
pub const SNAPRAID_FEATURE_ID: &str = "snapraid";

/// How long an automatic mover run waits before it may fire again, and how
/// often the node may probe an array's cache for aged files.
///
/// TWENTY MINUTES IS A JUDGEMENT, not a measurement. The trigger it guards is
/// "the cache fell below its minimum free space", and the failure mode without
/// a cooldown is real and cheap to reach: a cache full of files that are all
/// open, or all younger than the age rule, cannot be drained, so every tick
/// would see the same threshold crossed, start another mover job, and get the
/// same nothing — a job list full of no-ops hammering the disks. The direction
/// of the guess is what makes it acceptable: too long only delays a move the
/// scheduled run would make anyway, too short costs disk churn during exactly
/// the period the array is under pressure.
///
/// The age probe (`ElasticCacheAge`) is paced by the same interval: it walks
/// the whole cache and scans every process's descriptors, as root, and a file
/// that became old enough a few minutes ago loses nothing by waiting for the
/// next probe — it is on the cache either way.
pub const MOVER_RETRIGGER_COOLDOWN: Duration = Duration::from_secs(20 * 60);

/// How many age thresholds a file may wait on the cache before the node calls
/// the cache stuck (`cache_stuck_after_secs`).
///
/// A healthy automatic mover takes a file at most one probe interval plus one
/// cooldown after it becomes old enough, i.e. well inside two thresholds at the
/// default 2 h. Four leaves room for a long run and a failed coupled sync
/// without letting a genuinely stuck file go unreported past half a day.
pub const CACHE_STUCK_AGE_MULTIPLE: u64 = 4;

/// The window the card promises. n11 renders the row „Błędy parity (30 dni)"
/// and the wire carries this number beside the count, so the sum below and the
/// label the UI reads come from ONE place and can never drift apart.
pub const PARITY_ERRORS_WINDOW_DAYS: u32 = 30;

/// The most rows the parity window may return. The window itself is selected
/// BY DATE (`db::elastic_arrays`), so this is only a guard against a
/// pathological array pulling an unbounded result set — it never decides the
/// window, and it is not the display history's retention.
///
/// Why it cannot cut into the window: the fastest cadence the product
/// schedules is a nightly sync plus a weekly scrub, about 1.14 runs a day, so
/// 30 days holds roughly 34 runs. 200 rows is about 5.8x that — some 175 days
/// at the same cadence — so an array would have to run maintenance nearly six
/// times faster than the fastest supported schedule before the cap could reach
/// the far edge of a 30-day window.
///
/// And if it ever did, nothing green is invented: a result AT the cap is read
/// as a possibly truncated window and answers unknown, never zero.
pub const PARITY_ERRORS_MAX_ROWS: u32 = 200;

/// How many recorded mover runs n11's history strip carries.
pub const MOVER_HISTORY_ROWS: u32 = 20;

// =============================================================================
// The model
// =============================================================================

/// One data or cache disk of an array, as the node remembers it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchRow {
    pub disk_id: String,
    /// Kernel name, e.g. `sdg`.
    pub name: String,
    /// What mkfs and mount are pointed at. Preferably a
    /// `/dev/disk/by-id/…` link: a branch mounted by kernel name would move
    /// to another disk after a rename, with mergerfs serving the result.
    pub device: String,
    /// 'data' | 'cache'.
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParityRow {
    pub disk_id: String,
    pub name: String,
    pub device: String,
    /// 1-based.
    pub index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderRow {
    pub name: String,
    /// 'yes' | 'no' | 'only' — see `CachePolicy`.
    pub cache_policy: String,
    pub share_id: String,
    pub share_label: String,
}

/// What "use cache" means for one folder.
///
/// It is a MOVER policy and not a mergerfs one, and that is not a shortcut: a
/// mergerfs create policy is mount-wide, so there is no mergerfs setting that
/// can keep ONE folder's new files off the cache while another folder's go on
/// it. §5.3 reads as though the per-folder switch were partly a mergerfs
/// create policy; mechanically it cannot be, and pretending otherwise would
/// mean a folder set to "no" silently still receiving every new file on the
/// cache. So: new files always land on the cache while a cache exists, and
/// what the folder decides is how fast — or whether — they leave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// The default: the age and free-space rules decide.
    Yes,
    /// Moved down on the first run whatever the age rule says.
    No,
    /// Never moved down. The bytes stay on the cache, and therefore stay
    /// outside parity for as long as the folder exists — which is why
    /// `protection` counts them separately and says so.
    Only,
}

impl CachePolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "yes" => Some(Self::Yes),
            "no" => Some(Self::No),
            "only" => Some(Self::Only),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Only => "only",
        }
    }
}

/// The most top-level folders one array may publish.
///
/// The bound is not cosmetic: `MoverRules::validate` refuses more than 128
/// folder rules and 16 KiB of names, and the wire carries this list on every
/// read of the array. A union with more entries than this is a union nobody
/// can administer one row at a time, and a truncated list would be read as
/// "these are the folders" — so the listing answers UNKNOWN at the cap, the
/// same way `PARITY_ERRORS_MAX_ROWS` refuses to report a possibly truncated
/// window as a count.
pub const FOLDERS_MAX_ROWS: usize = 256;

/// Whether `name` can be ONE top-level folder of a union.
///
/// It is the single-segment half of the helper's own `mover_folder_valid`,
/// which is what finally accepts these names as mover rules — a name this
/// accepts and the helper refuses would be a policy an admin could store and
/// no run could honour.
pub fn folder_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

/// The top-level directory names under `union`, or `None` when this node
/// cannot say what they are.
///
/// AN EMPTY READ IS `None`, AND THAT IS THE WHOLE POINT. §3.4 forbids fstab,
/// so TentaNas is the only thing that mounts an array's branches: before the
/// mounts are made `/mnt/<array>` is an ordinary, empty directory of the ROOT
/// filesystem, and `read_dir` on it succeeds and yields nothing. Reading that
/// as "this array has no folders" would publish an empty Foldery table for an
/// array that is full of them — the §3.4 empty-share trap, one screen up.
/// A union that genuinely holds nothing reads unknown too, which costs an
/// admin one sentence and cannot cost anybody a folder.
///
/// A name this node cannot publish faithfully — not UTF-8, or past the cap —
/// also yields `None` rather than a list with a hole in it: a hole is exactly
/// where a pinned folder would go missing.
fn read_union_folders(union: &std::path::Path) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(union).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().ok()?;
        // Every ext4 branch carries one at its root and the union merges them
        // all into a single entry. It is the filesystem's, not the admin's,
        // and offering to pin it would offer a cache policy for a directory
        // the mover has no business walking.
        if name == "lost+found" {
            continue;
        }
        if !folder_name_valid(&name) {
            return None;
        }
        names.insert(name);
        if names.len() > FOLDERS_MAX_ROWS {
            return None;
        }
    }
    (!names.is_empty()).then_some(names)
}

/// The array's folders: its real top-level directories joined with the cache
/// policies this node has STORED, and whether the first half could be read.
///
/// The two halves are not symmetric, and that asymmetry is the safety rule:
/// * a folder with a stored policy is ALWAYS listed, whatever the union says.
///   The policy is a persisted intention, `mover_rules` turns it into what the
///   next run may and may not touch, and dropping it because a mount is not up
///   yet would quietly unpin a folder whose bytes were pinned on purpose;
/// * a folder with no stored policy is listed only because it was FOUND, at
///   the default `yes`. That is what lets an admin discover what there is to
///   pin instead of having to guess a name.
///
/// `shares` maps a folder name to the `(share_id, label)` serving it.
pub fn folders_of(
    union: &std::path::Path,
    stored: &BTreeMap<String, String>,
    shares: &BTreeMap<String, (String, String)>,
) -> (Vec<FolderRow>, bool) {
    let discovered = read_union_folders(union);
    let mut names: BTreeSet<&str> = stored.keys().map(String::as_str).collect();
    if let Some(found) = discovered.as_ref() {
        names.extend(found.iter().map(String::as_str));
    }
    let folders = names
        .into_iter()
        .map(|name| {
            let (share_id, share_label) = shares.get(name).cloned().unwrap_or_default();
            FolderRow {
                name: name.to_string(),
                cache_policy: stored
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| CachePolicy::Yes.as_str().to_string()),
                share_id,
                share_label,
            }
        })
        .collect();
    (folders, discovered.is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoverConfig {
    /// Whether the saved schedule is switched ON. Automatic moving does NOT
    /// depend on it: every enabled array with a cache moves its aged files by
    /// itself (`MoverTrigger::FilesAged`), like any cache. A schedule that is
    /// on RESTRICTS those moves to its own slots, for an admin who wants the
    /// data disks woken only at certain hours; cache pressure still overrides
    /// it. Off, or no schedule at all, is the unrestricted default.
    pub schedule_enabled: bool,
    pub schedule: Option<NasSchedule>,
    pub min_age_secs: u64,
    pub cache_min_free_pct: u8,
    pub coupled_sync: bool,
    /// Whether these values were chosen for this array or are the defaults
    /// below. True exactly when a settings row exists for this array, which is
    /// why the settings live in a table of their own: a stored `7200` is
    /// indistinguishable from a column nobody wrote, but a MISSING ROW is not.
    pub configured: bool,
}

impl Default for MoverConfig {
    fn default() -> Self {
        Self {
            schedule_enabled: false,
            schedule: None,
            min_age_secs: 7_200,
            cache_min_free_pct: 20,
            // §5.3 makes this the default and the wizard explains it: without
            // the coupling the mover moves bytes out of one unprotected
            // window and into another.
            coupled_sync: true,
            configured: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapraidConfig {
    pub sync_schedule: Option<NasSchedule>,
    pub scrub_schedule: Option<NasSchedule>,
    /// Whether each cadence fires. A disabled schedule stays SAVED, so the
    /// schedule and the switch are two separate facts and the card must be
    /// able to tell them apart.
    pub sync_enabled: bool,
    pub scrub_enabled: bool,
    pub scrub_percent: u8,
    pub scrub_older_than_days: u32,
}

/// Why one folder of an array cannot be given a cache policy.
///
/// Two arms and not one boolean, because the two are the difference this
/// whole feature keeps having to make: a folder the node LOOKED FOR and did
/// not find is a typo an admin should fix, while a folder nobody could look
/// for is a mount that is not up — and answering the second as the first
/// would tell an admin their folder does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderPolicyRefusal {
    NoSuchFolder,
    FoldersUnknown,
}

/// Whether `folder` may be given a cache policy on this array. `None` = it may.
///
/// A folder that already CARRIES a stored policy passes even while the union
/// is unreadable: it is in `folders` either way (see `folders_of`), so an
/// admin can always take a pin back off, which is the one edit that must not
/// depend on a mount being up.
pub fn folder_policy_refusal(
    array: &ElasticArrayRow,
    folder: &str,
) -> Option<FolderPolicyRefusal> {
    if array.folders.iter().any(|f| f.name == folder) {
        return None;
    }
    Some(if array.folders_known {
        FolderPolicyRefusal::NoSuchFolder
    } else {
        FolderPolicyRefusal::FoldersUnknown
    })
}

/// One array's desired state, the shape a store row will carry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ElasticArrayRow {
    pub snapraid_history: Vec<tentaflow_protocol::tentanas::NasSnapraidRun>,
    /// The sync and scrub runs that finished inside `PARITY_ERRORS_WINDOW_DAYS`,
    /// selected BY DATE rather than by count, so rotation of the display
    /// history above can never hide an error that is still inside the window.
    /// Bounded by `PARITY_ERRORS_MAX_ROWS`; at that bound the window is read as
    /// truncated and the count answers unknown.
    pub parity_window_runs: Vec<tentaflow_protocol::tentanas::NasSnapraidRun>,
    pub last_sync_run: Option<tentaflow_protocol::tentanas::NasSnapraidRun>,
    pub last_scrub_run: Option<tentaflow_protocol::tentanas::NasSnapraidRun>,
    pub create_spec: Option<tentanas_helper::elastic::ElasticCreateSpec>,
    pub name: String,
    pub enabled: bool,
    /// 'xfs' | 'ext4'.
    pub filesystem: String,
    pub create_policy: String,
    pub branches: Vec<BranchRow>,
    pub parity: Vec<ParityRow>,
    pub folders: Vec<FolderRow>,
    /// Whether `folders` is the array's real top level — see `folders_of`.
    /// `false` is unknown, never "none", and `Default` starts there because a
    /// row nobody has looked at has looked at nothing.
    pub folders_known: bool,
    pub mover: MoverConfig,
    /// The mover runs this node has RECORDED, newest first — n11's history
    /// strip. Empty means "nothing recorded", which is why the strip says that
    /// in words rather than printing a zero beside a populated last run.
    pub mover_history: Vec<NasMoverRun>,
    /// An operation of this array is closed `needs_attention` and unresolved.
    pub unresolved_operation: bool,
    /// The array was never brought to life: its create is still running, or
    /// ended without success and no Restore has finished it since
    /// (`store::CREATION_UNFINISHED`). Nothing unattended runs on it.
    pub creation_unfinished: bool,
    /// Every unresolved operation is a mover run that stopped with something
    /// left, and the next mover run settles it. The scheduler starts that run
    /// on its own; nothing waits for a `fix` that could not run.
    pub mover_settles_unresolved: bool,
    /// Mover runs that ended needing attention since the last one that
    /// succeeded, newest first and counted until that success. A run refused
    /// before it started (`failed`) is not counted: nothing ran.
    pub mover_failed_runs: u32,
    /// Whether a Sync or a full Scrub may start right now: an array with
    /// nothing unresolved, or one whose only unresolved operations are parity
    /// runs a Sync settles (W5/W6 of the fourth review).
    pub parity_run_available: bool,
    pub snapraid: SnapraidConfig,
    /// The data disk an add has pinned and not finished
    /// (`db::unfinished_elastic_add_disk`): the identity its resume and its
    /// undo must reuse, and the one disk a helper answer may count beyond the
    /// spec (`answered_spec`).
    pub pending_add: Option<ElasticDiskSpec>,
    /// The last state this node persisted, and why — carried the way
    /// `TargetRow::state`/`state_detail` are, so an array switched off by an
    /// import keeps the sentence that explains it.
    pub state: String,
    pub state_detail: String,
    /// `state_detail` as codes (migration 23): the stored sentence is an
    /// operation's error or the node's own words, and the screen words the
    /// code and shows the sentence only as its tooltip.
    pub state_reasons: Vec<NasHealthReason>,
    pub created_at: String,
    pub updated_at: String,
}

impl ElasticArrayRow {
    pub fn persisted_spec(&self) -> Result<&ElasticCreateSpec> {
        self.create_spec.as_ref().ok_or_else(|| anyhow!("Brak utrwalonej intencji macierzy"))
    }

    pub fn data(&self) -> impl Iterator<Item = &BranchRow> {
        self.branches.iter().filter(|b| b.role == "data")
    }

    pub fn cache(&self) -> impl Iterator<Item = &BranchRow> {
        self.branches.iter().filter(|b| b.role == "cache")
    }

    pub fn union_path(&self) -> String {
        union_path(&self.name)
    }

    /// The identity every other Elastic table keys on. `None` for a row whose
    /// persisted intention could not be read — such a row has no schedule to
    /// match either, so the scheduler skips it rather than guessing.
    pub fn array_id(&self) -> Option<&str> {
        self.create_spec.as_ref().map(|s| s.array_id.as_str())
    }

    /// The helper's spec for this row — the single bridge between the model
    /// and the plan. Every plan, preview and privileged step goes through it,
    /// so the row and the thing that runs cannot describe two different
    /// arrays.
    pub fn spec(&self) -> ElasticSpec {
        ElasticSpec {
            name: self.name.clone(),
            filesystem: self.filesystem.clone(),
            data: self
                .data()
                .map(|b| SpecBranch {
                    disk: b.name.clone(),
                    device: b.device.clone(),
                })
                .collect(),
            cache: self
                .cache()
                .map(|b| SpecBranch {
                    disk: b.name.clone(),
                    device: b.device.clone(),
                })
                .collect(),
            parity: self
                .parity
                .iter()
                .map(|p| SpecParity {
                    index: p.index,
                    disk: p.name.clone(),
                    device: p.device.clone(),
                })
                .collect(),
            mergerfs: MergerfsOptions {
                create_policy: if self.create_policy.is_empty() {
                    MergerfsOptions::default().create_policy
                } else {
                    self.create_policy.clone()
                },
                ..MergerfsOptions::default()
            },
            snapraid: SnapraidOptions {
                // A zero here is "never configured", not "scrub nothing":
                // `snapraid scrub -p 0` is a run that reads no block, so a
                // row that has not been through the schedule dialog would
                // otherwise get a scrub job that does nothing and reports
                // success.
                scrub_percent: if self.snapraid.scrub_percent == 0 {
                    SnapraidOptions::default().scrub_percent
                } else {
                    self.snapraid.scrub_percent
                },
                scrub_older_than_days: self.snapraid.scrub_older_than_days,
                ..SnapraidOptions::default()
            },
        }
    }

    /// The rules one mover run is carried out under, derived from the mover
    /// settings and the folders' cache policies.
    pub fn mover_rules(&self) -> MoverRules {
        MoverRules {
            min_age_secs: self.mover.min_age_secs,
            min_free_pct: self.mover.cache_min_free_pct,
            pinned_folders: self
                .folders
                .iter()
                .filter(|f| CachePolicy::parse(&f.cache_policy) == Some(CachePolicy::Only))
                .map(|f| f.name.clone())
                .collect(),
            eager_folders: self
                .folders
                .iter()
                .filter(|f| CachePolicy::parse(&f.cache_policy) == Some(CachePolicy::No))
                .map(|f| f.name.clone())
                .collect(),
            // Never negotiable. §5.3: a file another process holds open is
            // skipped and reported, never moved out from under its writer.
            skip_open_files: true,
        }
    }
}

// =============================================================================
// Observation
// =============================================================================

/// What this node could measure about one mountpoint and the device behind it.
///
/// Every field is an `Option` because every one of them can genuinely fail to
/// be readable, and a `0` or a `false` in any of them would be a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BranchProbe {
    /// Whether a filesystem is mounted here. `None` = the mount table could
    /// not be read.
    pub mounted: Option<bool>,
    /// Whether the block device this branch names EXISTS on the node right
    /// now. `None` = nothing looked.
    ///
    /// THIS FIELD IS THE DIFFERENCE BETWEEN AN ARRAY THAT COMES BACK AFTER A
    /// REBOOT AND ONE THAT NEVER DOES, and it was missing.
    ///
    /// §3.4 forbids fstab: TentaNas is the only thing that mounts these
    /// branches, so after every reboot all of them are unmounted. With
    /// `mounted` as the only fact, `Some(false)` meant "cold start" and "the
    /// disk is dead" at once — and the verdict, unable to tell them apart,
    /// had to freeze. `Apply` was reachable only once every branch was
    /// already mounted, which is the one state in which there is nothing left
    /// to mount. The plan that exists to mount them could never be run.
    ///
    /// That is `targets::kernel_can_serve` again, exactly: "the modules load
    /// only once the modules are loaded". The fix is the same one §5.5 made —
    /// the verdict asks whether this node CAN do the thing, not whether it
    /// has already been done. Here that question is "is the disk there?".
    pub device_present: Option<bool>,
    pub size_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

impl BranchProbe {
    /// A branch whose disk is present and whose filesystem is not mounted —
    /// the state of every branch of every array right after a reboot.
    pub fn cold() -> Self {
        Self {
            mounted: Some(false),
            device_present: Some(true),
            ..Self::default()
        }
    }

    /// A branch whose disk is NOT on this node. Not the same thing as `cold`,
    /// and the whole point of `device_present`.
    pub fn device_gone() -> Self {
        Self {
            mounted: Some(false),
            device_present: Some(false),
            ..Self::default()
        }
    }

    pub fn free_pct(&self) -> Option<u8> {
        match (self.size_bytes, self.free_bytes) {
            (Some(size), Some(free)) if size > 0 => {
                Some(((free.min(size) * 100) / size).min(100) as u8)
            }
            _ => None,
        }
    }
}

/// Everything this node can see about one array right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArrayObservation {
    /// Whether the mount table was readable at all. `false` makes every
    /// verdict `Freeze`: this node does not know what it is looking at.
    pub mount_table_known: bool,
    /// Keyed by mountpoint, branches and parity disks alike.
    pub probes: BTreeMap<String, BranchProbe>,
    /// Whether the union itself is mounted, and NOTHING ELSE about it.
    ///
    /// MEASURED (2026-09-06, mergerfs 2.42.0), and it is why this is a bare
    /// `Option<bool>` rather than a `BranchProbe` like every other row here:
    /// `df` on a mergerfs mountpoint reports ONE BRANCH, not the sum of them.
    /// A union of three 4 TB disks does not report 12 TB. So there is no
    /// capacity, no used and no free figure that may be read from the union,
    /// and the way to make sure nobody reads one is to leave nowhere to put
    /// it. Capacity is summed per branch, in `to_protocol`.
    pub union_mounted: Option<bool>,
    /// The last successful `snapraid sync`, ISO-8601.
    pub last_sync_at: Option<String>,
    /// Errors the last scrub found. `None` = nothing has scrubbed.
    pub parity_errors: Option<u64>,
    /// Bytes on the DATA disks the last sync did not cover, when something
    /// measured it (`snapraid diff` is the only thing that can, and it is
    /// expensive, so this is usually `None`).
    pub moved_unsynced_bytes: Option<u64>,
    /// The helper's explicit verdict that parity does not cover what the
    /// mover moved. It alone decides; a byte count never reads as protected.
    pub parity_stale: bool,
    pub last_mover: Option<ElasticMoverRun>,
    /// What a mover run would find on the cache, when the scheduler's age
    /// probe measured it. `None` = not measured this time, never "empty".
    pub cache_age: Option<ElasticCacheAge>,
    /// Files a stuck mover record left in two versions, as the helper found
    /// them on the branches (A3 of the second review).
    pub conflicts: Vec<ElasticConflict>,
    /// The kernel name the helper resolved each member's device to in this
    /// very read, keyed by mountpoint like `probes`. A LIVE name: it is the
    /// device as it is now, so it names a member the node's inventory does
    /// not hold yet (right after a core start, before the first sampler
    /// pass) — see the naming rule at `disks::ShownDiskName`.
    pub kernel_names: BTreeMap<String, String>,
}

impl ArrayObservation {
    fn probe(&self, mountpoint: &str) -> BranchProbe {
        self.probes.get(mountpoint).copied().unwrap_or_default()
    }

    /// Files what the helper measured for each disk under its mountpoint.
    ///
    /// Shared by the full read and by the scheduler's cache-pressure check so
    /// the two cannot disagree about which path a branch's free space is
    /// filed under — `mover_trigger` looks the cache up by exactly the path
    /// this writes, and a second copy of this mapping that drifted would make
    /// the trigger read every cache as unmeasured and silently never fire.
    fn record_disks(
        &mut self,
        array_name: &str,
        disks: Vec<tentanas_helper::elastic::ElasticDiskObservation>,
    ) {
        for disk in disks {
            let path = match disk.role {
                ElasticRole::Data(i) => data_branch_path(array_name, &format!("d{i}")),
                ElasticRole::Cache => cache_branch_path(array_name, "c1"),
                ElasticRole::Parity(i) => parity_mount_path(array_name, i),
            };
            if let Some(name) = disk.kernel_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                self.kernel_names.insert(path.clone(), name.to_string());
            }
            self.probes.insert(
                path,
                BranchProbe {
                    mounted: disk.mounted,
                    device_present: disk.device_present,
                    size_bytes: disk.size_bytes,
                    used_bytes: disk.used_bytes,
                    free_bytes: disk.free_bytes,
                },
            );
        }
    }
}

// =============================================================================
// State and verdict
// =============================================================================

/// What the apply path must DO with an array once `array_state` has judged it.
///
/// The same three-way shape `targets::Disposition` has, and for the same
/// reason: a state string is not enough, because two different errors want
/// opposite actions. An array the admin switched off has to come down; an
/// array whose data disk is missing must NOT come down and must NOT go up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Mount the branches and the union, keep them mounted.
    Apply,
    /// Take the union and the branches down. The admin disabled the array —
    /// and that is the ONLY thing that produces this verdict. Nothing about a
    /// disk, a tool or a measurement unmounts a live union: taking the union
    /// down cuts every share on it, so it happens because a person asked.
    Remove,
    /// Report it and touch NOTHING: do not mount it, do not unmount it.
    ///
    /// This is where every error goes, and the asymmetry is deliberate.
    ///   * A branch that is not mounted must stop the union from being
    ///     mounted, because a union over an empty branch directory sends
    ///     client writes to the root filesystem, where they are invisible and
    ///     are shadowed the moment the real disk is mounted (§3.4).
    ///   * The same branch must NOT bring a union that is already up down,
    ///     because that takes working shares away from clients over a fault
    ///     that snapraid can repair with the array running.
    ///   * A measurement that failed must do neither, because a node that
    ///     cannot see its own mounts cannot be trusted to change them.
    /// One verdict covers all three: report, alert, wait for an admin.
    Freeze,
}

/// Whether an array reaches the union, why not when it does not, and what the
/// apply path should do about it.
///
/// `installed` is injected exactly the way `targets::target_state` injects it,
/// so this table is testable on a host with neither mergerfs nor snapraid.
pub fn array_state(
    array: &ElasticArrayRow,
    observed: &ArrayObservation,
    installed: &dyn Fn(&str) -> bool,
) -> (&'static str, CodedText, Disposition) {
    if !array.enabled {
        // A row that ARRIVED disabled keeps the sentence it arrived with —
        // the config import writes reasons an admin has to read into exactly
        // this field. A row an admin STOPPED keeps nothing: its old detail
        // described a running array. The carried sentence is the importer's
        // and has no code: the screen shows it as written.
        let carried = if array.state == "disabled" {
            CodedText { text: array.state_detail.clone(), reasons: array.state_reasons.clone() }
        } else {
            CodedText::default()
        };
        return ("disabled", carried, Disposition::Remove);
    }

    if !observed.mount_table_known {
        return (
            "unknown",
            CodedText::new(
                "mount_table_unknown",
                &[],
                "this node could not read its mount table, so it will not mount or unmount \
                 anything: a union mounted over branches it cannot see would send every client \
                 write to the root filesystem",
            ),
            Disposition::Freeze,
        );
    }

    if !installed(MERGERFS_FEATURE_ID) {
        // Freeze, not Remove. A union that is already mounted keeps serving
        // — the mount does not need the binary any more — and unmounting a
        // live array because a package check came back negative would take
        // every share on it away for nothing.
        return (
            "error",
            CodedText::new(
                "mergerfs_missing",
                &[],
                "mergerfs is not installed on this node, so the union cannot be mounted",
            ),
            Disposition::Freeze,
        );
    }
    if !array.parity.is_empty() && !installed(SNAPRAID_FEATURE_ID) {
        return (
            "error",
            CodedText::new(
                "snapraid_unusable",
                &[],
                "snapraid is not usable on this node — it is missing, or installed and not \
                 working (see the Environment tab) — so the parity disks of this array protect \
                 nothing until it is",
            ),
            Disposition::Freeze,
        );
    }

    // The branch check, and it is the one that matters.
    //
    // THREE readings, three different sentences, two different verdicts. The
    // version that had only `mounted` collapsed the first two into one and
    // deadlocked the whole feature — see `BranchProbe::device_present`.
    //
    //   mounted            nothing to do
    //   not mounted, disk present   this node CAN mount it: Apply
    //   not mounted, disk gone      a fault an admin has to answer: Freeze
    //   nothing measured            this node cannot judge: Freeze
    let mut unknown = Vec::new();
    let mut gone = Vec::new();
    let mut mountable = Vec::new();
    for member in mountpoints_of(array, observed) {
        let probe = observed.probe(&member.mountpoint);
        match (probe.mounted, probe.device_present) {
            (Some(true), _) => {}
            (Some(false), Some(true)) => mountable.push(member),
            (Some(false), Some(false)) => gone.push(member),
            // Not mounted and nobody looked at the disk, or the mount table
            // itself was unreadable. Both are "this node does not know".
            (Some(false), None) | (None, _) => unknown.push(member),
        }
    }
    if !unknown.is_empty() {
        return (
            "unknown",
            CodedText::new(
                "branches_unknown",
                &members_params(&unknown),
                format!(
                    "this node could not tell whether {} {} there, and will not mount the union \
                     until it can",
                    members_text(&unknown),
                    if unknown.len() == 1 { "is" } else { "are" }
                ),
            ),
            Disposition::Freeze,
        );
    }
    // A disk that is genuinely absent freezes the array, and it freezes it in
    // BOTH directions: the union must not come up over a hole (client writes
    // would land on the root filesystem) and a union that is already up must
    // not come down (that takes working shares away over a fault snapraid can
    // repair with the array running). Only an admin resolves this one.
    if !gone.is_empty() {
        let serving = observed.union_mounted == Some(true);
        let consequence = if serving {
            "the union is still serving the disks it has, and the files on the missing one \
             are not visible in it"
        } else {
            "the union stays down: mounting it over an unmounted branch would send client \
             writes to the root filesystem"
        };
        let mut params = members_params(&gone);
        params.push(("union", if serving { "serving" } else { "down" }.to_string()));
        return (
            "error",
            CodedText::new(
                "branches_gone",
                &params,
                format!(
                    "{} {} not on this node — {consequence}",
                    members_text(&gone),
                    if gone.len() == 1 { "is" } else { "are" }
                ),
            ),
            Disposition::Freeze,
        );
    }
    // Everything the array needs is present and some of it is not mounted:
    // ordinary, and it is the state of every array on every boot. `Apply` is
    // what lets `plan_mount` run at all.
    if !mountable.is_empty() {
        return (
            "pending",
            CodedText::new(
                "branches_mountable",
                &members_params(&mountable),
                format!(
                    "{} {} present and not mounted yet — the next reconcile mounts {} and then \
                     the union",
                    members_text(&mountable),
                    if mountable.len() == 1 { "is" } else { "are" },
                    if mountable.len() == 1 { "it" } else { "them" }
                ),
            ),
            Disposition::Apply,
        );
    }

    if observed.union_mounted != Some(true) {
        // Not an error: nothing is wrong, this node simply has not done it
        // yet — the array was saved a moment ago, or the node has just
        // booted. The next reconcile mounts it.
        return (
            "pending",
            CodedText::new(
                "union_not_mounted",
                &[],
                "saved, but this node is not serving the union yet — the next reconcile mounts it",
            ),
            Disposition::Apply,
        );
    }

    let detail = if array.parity.is_empty() {
        CodedText::new("no_parity", &[], "no parity disk: a disk failure loses that disk's files")
    } else {
        CodedText::default()
    };
    ("active", detail, Disposition::Apply)
}

/// One mountpoint the array needs: where it is, which part of the array it
/// serves ('data' | 'cache' | 'parity') and the member's shown name
/// (`member_shown_name`).
struct ArrayMember {
    mountpoint: String,
    role: &'static str,
    name: String,
}

/// Every mountpoint the array needs, with the member it belongs to.
fn mountpoints_of(array: &ElasticArrayRow, observed: &ArrayObservation) -> Vec<ArrayMember> {
    let mut out = Vec::new();
    let member = |mountpoint: String, role: &'static str, disk_id: &str, slot: &str, number: usize| {
        let name = member_shown_name(observed.kernel_names.get(&mountpoint).map(String::as_str), disk_id, slot, number);
        ArrayMember { mountpoint, role, name }
    };
    for (i, branch) in array.data().enumerate() {
        out.push(member(data_branch_path(&array.name, &branch.name), "data", &branch.disk_id, &branch.name, i + 1));
    }
    for (i, branch) in array.cache().enumerate() {
        out.push(member(cache_branch_path(&array.name, &branch.name), "cache", &branch.disk_id, &branch.name, i + 1));
    }
    for parity in &array.parity {
        out.push(member(
            parity_mount_path(&array.name, parity.index),
            "parity",
            &parity.disk_id,
            &parity.name,
            usize::from(parity.index),
        ));
    }
    out
}

/// The name a state sentence gives one member: the kernel name its mount
/// was found under, else the one the live inventory has for its disk. A
/// stored array's branch is keyed by its SLOT ('d1', 'c1', 'parity1' — the
/// directory under the branch root), and a slot is not a name an admin has
/// ever seen: a member neither knows by a kernel name is "#<its number>",
/// which the screen words as "data disk no. 2". A branch row that already
/// carries a kernel name (the wizard's plan) keeps it.
fn member_shown_name(mounted_as: Option<&str>, disk_id: &str, slot: &str, number: usize) -> String {
    if let Some(name) = mounted_as.filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    if let Some(name) = super::disks::disk_name(disk_id).filter(|n| !n.is_empty()) {
        return name;
    }
    if is_slot_name(slot) || slot.is_empty() {
        format!("#{number}")
    } else {
        slot.to_string()
    }
}

/// Whether a branch or parity name is a stored SLOT ('d3', 'c1', 'parity2')
/// rather than a kernel name.
fn is_slot_name(name: &str) -> bool {
    let digits = |rest: &str| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    name.strip_prefix("parity").is_some_and(digits)
        || name.strip_prefix('d').is_some_and(digits)
        || name.strip_prefix('c').is_some_and(digits)
}

/// The members of a state sentence, the way the English names them:
/// "data disk sdb, cache disk nvme0n1".
fn members_text(members: &[ArrayMember]) -> String {
    members
        .iter()
        .map(|m| format!("{} disk {}", m.role, m.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The same members as parameters of a state code: one key per role
/// ('data', 'cache', 'parity'), the names comma-separated, a role with no
/// member left out. The screen words each role and joins them.
fn members_params(members: &[ArrayMember]) -> Vec<(&'static str, String)> {
    ["data", "cache", "parity"]
        .into_iter()
        .filter_map(|role| {
            let names: Vec<&str> =
                members.iter().filter(|m| m.role == role).map(|m| m.name.as_str()).collect();
            (!names.is_empty()).then(|| (role, names.join(",")))
        })
        .collect()
}

// =============================================================================
// Alerts
// =============================================================================

/// One array's branch alert: a data, cache or parity disk that is not where
/// the array expects it. One open row per DISK, not per array, so two failed
/// disks do not overwrite each other's story.
pub fn branch_alert_key(array: &str, disk: &str) -> String {
    format!("elastic:{array}:branch:{disk}")
}

/// The protection alert: files have waited on the cache, outside parity, past
/// `cache_stuck_after_secs`, and the node is not moving them. Pending files on
/// their own never raise it — see `cache_stuck_verdict`.
pub fn protection_alert_key(array: &str) -> String {
    format!("elastic:{array}:protection")
}

/// The alert for files a stuck mover record left in two versions.
pub fn conflict_alert_key(array: &str) -> String {
    format!("elastic:{array}:conflict")
}

/// What the conflict alert says, or `None` when there is nothing to say: one
/// line per file, naming where clients see it and where the other version is.
/// The alert stands for as long as the helper still finds a second version.
///
/// Coded 'elastic_conflict' {array, count}, with one 'conflict_file'
/// {path, visible, kept_kind, kept_disk?, kept_path?} line per file: the
/// paths are data, the sentence around them is the screen's.
///
/// WHERE the other version is is WORDED as a place, not as the helper's path
/// (wave-4 critic M2): a quarantined original sits in the cache root under
/// `.tentanas-quarantine-<operation uuid>-<seq>`, and that uuid is not
/// something a screen may show. The file's own path (`path`) is what the
/// admin knows it by. The helper's path does travel as `kept_path`, for one
/// use only: the row's deliberate "copy path" control puts it on the
/// clipboard, the one way to find a quarantined copy, and no screen renders
/// it. Only for a place inside THIS array's branches (`kept_place` found a
/// disk): any other path could name another array.
pub fn conflict_alert(array: &str, conflicts: &[ElasticConflict]) -> Option<store::AlertText> {
    if conflicts.is_empty() {
        return None;
    }
    let files = conflicts
        .iter()
        .map(|conflict| {
            let (kind, disk) = kept_place(array, &conflict.kept);
            let mut params: BTreeMap<String, String> = [
                ("path".to_string(), conflict.path.clone()),
                ("visible".to_string(), conflict.visible.clone()),
                ("kept_kind".to_string(), kind.to_string()),
            ]
            .into_iter()
            .collect();
            if let Some(disk) = disk {
                params.insert("kept_disk".to_string(), disk);
                params.insert("kept_path".to_string(), conflict.kept.clone());
            }
            tentaflow_protocol::tentanas::NasHealthReason {
                code: "conflict_file".to_string(),
                params,
            }
        })
        .collect();
    let lines = conflicts
        .iter()
        .map(|conflict| {
            let other = match kept_place(array, &conflict.kept) {
                ("quarantine", Some(disk)) => format!("kopia w kwarantannie na dysku cache {disk}"),
                ("data", Some(disk)) => format!("pod tą samą ścieżką na dysku danych {disk}"),
                _ => "na jednym z dysków macierzy".to_string(),
            };
            format!("{}: wersja widoczna {}, druga wersja: {other}", conflict.path, conflict.visible)
        })
        .collect::<Vec<_>>()
        .join("; ");
    Some(
        store::AlertText::new(
            "elastic_conflict",
            format!("Pliki zachowane w dwóch wersjach: {}", conflicts.len()),
            format!(
                "Przerwane przenoszenie zostawiło dwie wersje plików i żadna nie została usunięta. \
                 Porównaj je i usuń zbędną; alert zniknie, gdy zostanie jedna. {lines}"
            ),
        )
        .param("array", array)
        .param("count", conflicts.len())
        .reasons(files),
    )
}

/// The prefix the helper gives a quarantined original in the cache root
/// (`tentanas_helper::elastic_transfer::QUARANTINE_PREFIX`, which is not
/// public outside the helper).
const QUARANTINE_PREFIX: &str = ".tentanas-quarantine-";

/// Where the other version of a conflicted file is, as the helper's `kept`
/// path says it: `('quarantine', cache disk)` for a quarantined original in
/// a cache branch's root, `('data', data disk)` for a copy under a data
/// branch, and `('branch', None)` for a path of any other shape — still
/// said to be on the array's disks, and still never shown.
fn kept_place(array: &str, kept: &str) -> (&'static str, Option<String>) {
    let root = format!("{}{array}/", tentanas_helper::elastic::BRANCH_ROOT);
    let Some(rest) = kept.strip_prefix(&root) else {
        return ("branch", None);
    };
    let mut parts = rest.splitn(3, '/');
    let (Some(role), Some(disk), Some(tail)) = (parts.next(), parts.next(), parts.next()) else {
        return ("branch", None);
    };
    if disk.is_empty() || tail.is_empty() {
        return ("branch", None);
    }
    match role {
        "cache" if !tail.contains('/') && tail.starts_with(QUARANTINE_PREFIX) => ("quarantine", Some(disk.to_string())),
        "data" => ("data", Some(disk.to_string())),
        _ => ("branch", None),
    }
}

/// Raises or resolves the conflict alert from one observation of the array.
pub fn record_conflict_alert(db: &DbPool, array: &str, conflicts: &[ElasticConflict]) -> Result<()> {
    let key = conflict_alert_key(array);
    match conflict_alert(array, conflicts) {
        Some(text) => store::raise_coded_alert(db, &key, "warning", "elastic-array", array, &text).map(|_| ()),
        None => store::resolve_alert(db, &key),
    }
}

/// The parity alert: a scrub found errors.
pub fn parity_alert_key(array: &str) -> String {
    format!("elastic:{array}:parity")
}

/// Every alert key an array owns.
///
/// The delete path closes each of them. Without this an array that is
/// dissolved leaves its alerts open forever, with a drill-down to something
/// that no longer exists — the same defect `targets::forget_alerts` was added
/// for, and the reason this returns the WHOLE set rather than the ones that
/// happen to be open.
pub fn alert_keys(array: &ElasticArrayRow) -> Vec<String> {
    let mut keys = vec![
        protection_alert_key(&array.name),
        parity_alert_key(&array.name),
    ];
    for branch in &array.branches {
        keys.push(branch_alert_key(&array.name, &branch.name));
    }
    for parity in &array.parity {
        keys.push(branch_alert_key(&array.name, &parity.name));
    }
    keys
}

// =============================================================================
// The protection window
// =============================================================================

/// How much of this array is protected right now, and by how much it is not.
///
/// The sentence this feeds is fixed by the SPEC and is a SIZE, never a
/// duration: "18 GiB na cache bez parity (czeka na mover)". "Unsynced for 3 h"
/// is banned there and the reason is worth writing down — it says nothing
/// about how much is at risk, and it reads as an alarm on a perfectly healthy
/// array whose mover runs hourly, which is the state such an array is in most
/// of the time.
///
/// OPEN DISAGREEMENT BETWEEN TWO DOCUMENTS, RECORDED HERE AND NOT SETTLED.
/// n11 shows 18 GiB beside a cache the same screen reports as 218 GiB used,
/// and labels the 18 "świeże zapisy, reguła wieku > 2 h" — i.e. the SLICE the
/// next mover run would take. This function returns ALL used cache bytes,
/// which on that same array is 218 GiB: twelve times more.
///
/// Both readings are defensible and they answer different questions. Ours is
/// "how much of this array is outside parity right now", and the honest
/// answer is all of it: the 200 GiB too young to move is no better protected
/// than the 18 GiB about to be. n11's is "how much will the next mover run
/// rescue". Read ours as n11's and the urgency is badly overstated; read
/// n11's as ours and the EXPOSURE is badly understated, which is the worse
/// direction and the reason this one is left as it is.
///
/// The coordinator is settling it with the owner. Whoever closes it: the
/// tests below use fixtures where the two numbers coincide — an empty cache,
/// or a cache holding only fresh writes — so nothing in this repository
/// currently fails whichever reading is wrong. Whichever wins, give it a
/// fixture where the two differ.
///
/// `fault_tolerance` opisuje dane ostatniego `protected_as_of`; nowe dane i cache
/// pozostają poza tą odpornością, co opisują status i liczniki niechronionych bajtów.
pub fn protection(array: &ElasticArrayRow, observed: &ArrayObservation) -> NasElasticProtection {
    // Cache bytes are unprotected BY CONSTRUCTION: the cache is not a snapraid
    // data disk (it cannot be — the mover moves blocks out from under it), so
    // nothing on it is covered whatever the last sync did.
    //
    // This figure is OURS, and MEASURED (2026-09-06, snapraid 14.7) it has to
    // be: 12 MiB were written to a data disk without a sync and `snapraid
    // status` came back byte for byte identical to the run before the write.
    // snapraid simply does not report unsynced data there — only `diff` sees
    // a change, and it is expensive. So an implementation that asked snapraid
    // "how much is unprotected" would get a confident, unchanged, WRONG
    // answer, which is the worst shape this model can take.
    // `Some(0)`, not `None`, and the difference is a whole card.
    //
    // The cache is OPTIONAL (§5.3). Starting this at `None` and only ever
    // writing it inside the loop meant an array with no cache disk fell out
    // with `None` — "not measured" — for a quantity that needs no
    // measurement: zero cache disks hold zero unprotected bytes, and that is
    // a fact, not a gap. The whole array then rendered `unknown`: a grey card
    // and a dash on the Protection KPI, with a sentence blaming a measurement
    // nobody ever had to take. The loop below still collapses the sum to
    // `None` the moment a cache disk that EXISTS cannot be read, which is the
    // case the `None` is actually for.
    let mut cache_bytes: Option<u64> = Some(0);
    for branch in array.cache() {
        let probe = observed.probe(&cache_branch_path(&array.name, &branch.name));
        match probe.used_bytes {
            Some(used) => cache_bytes = Some(cache_bytes.unwrap_or(0) + used),
            // One unreadable cache disk makes the whole figure unknown. A
            // partial sum presented as the total is exactly the confident
            // number this model exists to avoid: it would UNDERSTATE the
            // risk, which is the wrong direction to be wrong in.
            None => {
                cache_bytes = None;
                break;
            }
        }
    }

    let parity_present = !array.parity.is_empty();
    let fault_tolerance = if !parity_present {
        // A measurement, not a gap: an array with no parity disk survives no
        // failures, and this is the one place `Some(0)` is the honest answer.
        Some(0)
    } else if !observed.mount_table_known
        || observed.parity_errors != Some(0)
        || observed.last_sync_at.is_none()
    {
        None
    } else {
        // Counted on the DISK BEING THERE, not on it being mounted.
        // Resilience is a property of the hardware: a parity disk this node
        // has not got round to mounting still holds the parity that would
        // rebuild a failed data disk. Counting mounts would drop every array
        // on the node to "survives 0 failures" for the length of every boot,
        // which is both false and exactly when an admin is most likely to be
        // looking.
        let mut healthy = 0u8;
        let mut known = true;
        for parity in &array.parity {
            match observed
                .probe(&parity_mount_path(&array.name, parity.index))
                .device_present
            {
                Some(true) => healthy = healthy.saturating_add(1),
                Some(false) => {}
                None => known = false,
            }
        }
        if known {
            Some(healthy)
        } else {
            None
        }
    };

    // The open-window clause is built BEFORE the status chain, because the
    // arms below that report an UNMEASURED fault tolerance must be able to
    // carry it too. 18 GiB measured on cache is a certain figure; "parity
    // availability could not be measured" is an uncertain one, and letting
    // the uncertain one silence the certain one is the same mistake this
    // model refuses everywhere else. The status still says `unknown` there —
    // nothing confirmed parity, and that stands — but the sentence no longer
    // drops what this node does know.
    let window_clause: Option<String> =
        if cache_bytes.is_some_and(|bytes| bytes > 0) || observed.parity_stale {
        // Both halves can be open at once, and either may be open while the
        // other is merely unmeasured. A known-bad fact outranks an unmeasured
        // one, so this arm sits ABOVE the unknown cache: saying only "the
        // cache could not be measured" would drop what this node does know,
        // next to a byte figure the card already shows.
        //
        // Wording canon (plan §5.3b), verbatim and in Polish: a size and a
        // mechanism, never a duration. Naming only the cache half would promise
        // the next sync closes the window, which is false for files whose
        // coupled sync already failed.
        //
        // BOTH halves are chosen by their own FIGURE, never by the flag or by
        // "was it measured": a half measured as ZERO has nothing sitting in it,
        // and a sentence that claims otherwise beside a 0 B figure is exactly
        // the confident-zero this model exists to refuse (protocol: a confident
        // 0 next to "unprotected bytes" is worse than no answer). `parity_stale`
        // still guards the arm — an unconfirmed sync is not "protected" — but
        // it no longer selects a sentence that asserts a quantity.
        let waiting = "na cache bez parity (czeka na mover): ochronę domyka najbliższy sync, \
             który mover uruchamia zaraz po przenosinach";
        let unmeasured = "ten node nie zmierzył, ile czeka na cache";
        let moved = "pliki już przeniesione przez mover także są poza parity: ich sprzężony sync \
             nie potwierdził ochrony, domknie ją dopiero kolejny udany sync";
        let unconfirmed = "sprzężony sync nie potwierdził ochrony; domknie ją dopiero kolejny udany sync";
        let cache_clause = if cache_bytes.is_some_and(|bytes| bytes > 0) {
            Some(waiting)
        } else if cache_bytes.is_none() {
            Some(unmeasured)
        } else {
            // Measured empty: nothing waits there.
            None
        };
        let moved_clause = observed.parity_stale.then(|| {
            match observed.moved_unsynced_bytes.filter(|bytes| *bytes > 0) {
                // Bytes are known to sit outside parity.
                Some(_) => moved,
                // Stale, but nothing measured as moved: say only that.
                None => unconfirmed,
            }
        });
        let detail = match (cache_clause, moved_clause) {
            (Some(cache), Some(moved)) => format!("{cache}; {moved}"),
            (Some(cache), None) => cache.to_string(),
            (None, Some(moved)) => moved.to_string(),
            (None, None) => {
                // Unreachable: the arm is entered only when a half is open. A
                // future guard edit must degrade the wording of one card, never
                // panic the whole array listing.
                debug_assert!(false, "okno bez otwartej połowy");
                unconfirmed.to_string()
            }
        };
            Some(detail)
        } else {
            None
        };
    // PLACEMENT IS DELIBERATE AND OWNER-APPROVED (2026-09-12). This arm is
    // tested BEFORE `cache_bytes.is_none()` below, so an array whose cache
    // could not be measured but whose parity the helper confirms is STALE
    // renders `window_open` — a confirmed open window — rather than `unknown`.
    // A known bad fact outranks an unmeasured one, and hiding a confirmed open
    // window behind "unknown" is less honest to the operator, not more.
    //
    // This DIFFERS from the last committed behaviour, where `cache_bytes
    // .is_none()` sat above both halves and won. The change came with merging
    // the two halves into one arm, it was accepted knowingly, and
    // `a_confirmed_stale_parity_outranks_an_unmeasured_cache` pins it so it
    // cannot drift back unnoticed in either direction.
    //
    // Appended, never prepended: the arm's own sentence names the reason the
    // card is in that state, and the window clause qualifies it.
    let with_window = |base: &str| match window_clause.as_deref() {
        Some(clause) => format!("{base}; {clause}"),
        None => base.to_string(),
    };
    let (status, detail) = if !parity_present {
        (
            "unprotected",
            "ta macierz nie ma dysku parity: awaria dysku traci jego pliki".to_string(),
        )
    } else if observed.last_sync_at.is_none() {
        (
            "window_open",
            "brak potwierdzonego sync: ochrona parity nie została potwierdzona".to_string(),
        )
    } else if observed.parity_errors.is_some_and(|errors| errors > 0) {
        (
            "unknown",
            "wykryto błędy parity; nie można potwierdzić ochrony danych".to_string(),
        )
    } else if fault_tolerance.is_none() {
        (
            "unknown",
            with_window("brak pełnego pomiaru dostępności i poprawności parity"),
        )
    } else if fault_tolerance.map(usize::from) != Some(array.parity.len()) {
        (
            "unknown",
            with_window("brakuje dysku parity; pełna skonfigurowana ochrona nie jest dostępna"),
        )
    } else if let Some(detail) = window_clause.clone() {
        ("window_open", detail)
    } else if cache_bytes.is_none() {
        (
            "unknown",
            "ten node nie zmierzył, ile czeka na cache".to_string(),
        )
    } else if observed.moved_unsynced_bytes.is_some_and(|bytes| bytes > 0) {
        (
            "window_open",
            "dane na dyskach danych nie są objęte ostatnim sync; opróżnienie cache nie zamyka okna bez ochrony".to_string(),
        )
    } else if observed.moved_unsynced_bytes.is_none() {
        // NOT a live path in production: the producer sets this from
        // `parity_stale`, and the helper keeps `parity_stale` true exactly when
        // `stale_parity_bytes` is `Some`, so a `None` here cannot arrive from a
        // real observation. It is reachable only from a directly-constructed
        // `ArrayObservation` — kept so a hand-built one degrades to "unknown"
        // rather than borrowing the sentence below.
        (
            "unknown",
            "nie zmierzono zmian na dyskach danych od ostatniego sync".to_string(),
        )
    } else {
        (
            // THE HONEST FLOOR. `moved_unsynced_bytes == Some(0)` means exactly
            // one thing: the MOVER left nothing outside the last sync — it is
            // derived from `!parity_stale`, and `stale_parity_bytes` is written
            // by the mover and by nobody else. It is NOT a measurement of the
            // array's contents. mergerfs `create` is mount-wide MFS (§5.3
            // SPROSTOWANIE), so a share write lands straight on a data branch,
            // outside parity, and NOTHING in `ArrayObservation` sees it: the
            // measurement recorded at the top of this function (12 MiB written
            // to a data disk, `snapraid status` byte-identical afterwards) is
            // that same blindness from the other side. So the sentence says
            // what was measured and stops there, and names the vintage of the
            // protection instead of implying it covers this moment.
            "protected",
            "mover nie zostawił danych poza ostatnim sync, cache jest pusty, a parity dostępna \
             bez zgłoszonych błędów; ochrona obejmuje stan z ostatniego sync"
                .to_string(),
        )
    };

    NasElasticProtection {
        cache_unprotected_bytes: cache_bytes,
        moved_unsynced_bytes: observed.moved_unsynced_bytes,
        status: status.to_string(),
        detail,
        fault_tolerance,
        protected_as_of: observed.last_sync_at.clone(),
    }
}

// =============================================================================
// The mover trigger
// =============================================================================

/// Why a mover run should start now, or why it should not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoverTrigger {
    /// The cache fell below its minimum free space. §5.3: below the threshold
    /// the mover runs whether or not the schedule says so.
    CacheLow,
    /// Files old enough to move are waiting on the cache and nothing restricts
    /// moving them now. This is the ordinary cache behaviour the owner asked
    /// for (2026-09-17): nobody has to configure or press anything.
    FilesAged,
    /// A mover run stopped with something unresolved (`mover_settles_unresolved`)
    /// and nothing else is: the next run finishes or reverses what it left,
    /// whatever the cache holds and whatever window a schedule sets, because
    /// until it runs the array's Sync and Scrub stay refused.
    Settle,
    /// Nothing to do — or nothing this node is allowed to conclude.
    None,
}

/// Automatic settling runs after which the node stops and asks an admin
/// (A2 of the second review).
pub const SETTLE_ATTEMPTS: u32 = 4;

/// Whether the node may start a settling run now. Each failed run doubles the
/// wait — one cooldown, then two, then four — and after `SETTLE_ATTEMPTS`
/// failures in a row nothing more starts on its own: a coupled Sync failing on
/// a dying disk would otherwise read the whole array every twenty minutes,
/// for ever. A run that succeeds resets the count; so does an admin's run.
pub fn settling_due(array: &ElasticArrayRow, clock: &MoverClock) -> bool {
    if array.mover_failed_runs >= SETTLE_ATTEMPTS {
        return false;
    }
    let wait = MOVER_RETRIGGER_COOLDOWN.saturating_mul(1 << array.mover_failed_runs.saturating_sub(1).min(8));
    clock.idle_for(&array.name, wait)
}

/// The alert of an array whose automatic settling stopped.
pub fn settle_alert_key(array: &str) -> String {
    format!("elastic:{array}:settle")
}

/// Whether a switched-on schedule confines automatic moves to its slots.
pub fn restricted_to_schedule(array: &ElasticArrayRow) -> bool {
    array.mover.schedule_enabled && array.mover.schedule.is_some()
}

/// The age of a file at which the pace of automatic moving no longer explains
/// it being on the cache. Never below one cooldown per multiple, so an array
/// with no age rule (`min_age_secs = 0`) is not called stuck within minutes.
fn automatic_move_budget_secs(array: &ElasticArrayRow) -> u64 {
    CACHE_STUCK_AGE_MULTIPLE
        .saturating_mul(array.mover.min_age_secs.max(MOVER_RETRIGGER_COOLDOWN.as_secs()))
}

/// How long the oldest file may wait on the cache before the stuck alert.
///
/// `CACHE_STUCK_AGE_MULTIPLE` age thresholds (each at least one cooldown), plus
/// one full period of the schedule when a schedule restricts moving: under a
/// daily window a file written just after the slot legitimately waits a day.
pub fn cache_stuck_after_secs(array: &ElasticArrayRow) -> u64 {
    let window = restricted_to_schedule(array)
        .then(|| array.mover.schedule.as_ref().and_then(super::scheduler::cadence_minutes))
        .flatten()
        .map(|minutes| u64::try_from(minutes).unwrap_or(0).saturating_mul(60))
        .unwrap_or(0);
    automatic_move_budget_secs(array).saturating_add(window)
}

/// Whether the last recorded run finished having moved nothing.
///
/// Such a run is evidence that what the probe calls due cannot be moved right
/// now — the data disks have no room above minfreespace, or a path collides
/// with another branch — and those are facts the probe does not see. Starting
/// the same run every cooldown would walk the cache, scan every process's
/// descriptors and wake the data disks three times an hour to move nothing, so
/// the age trigger backs off (`mover_trigger`).
fn last_run_moved_nothing(array: &ElasticArrayRow) -> bool {
    array
        .mover_history
        .first()
        .is_some_and(|run| matches!(run.outcome.as_str(), "ok" | "partial") && run.moved_files == 0)
}

/// Whether a mover run should start for this array right now.
///
/// The clock is a PARAMETER, not a wall-clock read, for the reason
/// `targets::removal_is_due_with` takes one: the process-wide clock is shared
/// state and `cargo test` runs tests in parallel, so a test that wound the
/// global clock would decide another test's outcome. It is also what makes the
/// cooldown testable without waiting twenty minutes.
///
/// Returns `MoverTrigger::None` for every uncertainty. A mover run is a
/// privileged job that moves files; starting one because a free-space figure
/// or the cache's age could not be read would be acting on a measurement that
/// does not exist.
pub fn mover_trigger(
    array: &ElasticArrayRow,
    observed: &ArrayObservation,
    clock: &MoverClock,
) -> MoverTrigger {
    if !array.enabled {
        return MoverTrigger::None;
    }
    if array.cache().next().is_none() {
        return MoverTrigger::None;
    }
    // Below the cooldown nothing fires, whatever the cache says: the run that
    // just happened is the answer to this threshold, and if it did not help
    // (every file open, every file too young) running it again in twenty
    // seconds will not help either.
    if !clock.cooled_down(&array.name) {
        return MoverTrigger::None;
    }
    if array.mover_settles_unresolved {
        return if settling_due(array, clock) { MoverTrigger::Settle } else { MoverTrigger::None };
    }
    for branch in array.cache() {
        let probe = observed.probe(&cache_branch_path(&array.name, &branch.name));
        // Unknown free space is not low free space.
        let Some(free_pct) = probe.free_pct() else {
            continue;
        };
        if free_pct < array.mover.cache_min_free_pct {
            return MoverTrigger::CacheLow;
        }
    }
    let aged = observed.cache_age.is_some_and(|age| age.due_files > 0);
    let backing_off = last_run_moved_nothing(array)
        && !clock.idle_for(&array.name, Duration::from_secs(automatic_move_budget_secs(array)));
    if aged && !restricted_to_schedule(array) && !backing_off {
        return MoverTrigger::FilesAged;
    }
    MoverTrigger::None
}

/// What the stuck-cache alert should do after one age probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheStuckVerdict {
    /// A file has waited past `cache_stuck_after_secs` and nothing was started
    /// to move it. Coded 'elastic_cache_stuck' (see `NasAlert::code`).
    Raise(store::AlertText),
    /// Nothing on the cache has waited that long.
    Clear,
    /// A run was just started for these files; the next probe after it is the
    /// answer, so the alert neither opens nor closes on this one.
    Keep,
}

/// The stuck-cache alert, decided from one age probe.
///
/// It fires on AGE, never on the pending byte count: fresh files on the cache
/// are the normal state of an array with a cache and must not look like a
/// fault. It fires only when the oldest file outside pinned folders is older
/// than `cache_stuck_after_secs` AND this pass did not start a run for it, and
/// it clears on the first probe that finds no file that old — including an
/// empty cache. The detail names the most likely reason, so the admin knows
/// whether to free space, close a file, or resolve an operation.
pub fn cache_stuck_verdict(
    array: &ElasticArrayRow,
    age: &ElasticCacheAge,
    run_started: bool,
) -> CacheStuckVerdict {
    let limit = cache_stuck_after_secs(array);
    let Some(oldest) = age.oldest_secs.filter(|secs| *secs >= limit) else {
        return CacheStuckVerdict::Clear;
    };
    if run_started {
        return CacheStuckVerdict::Keep;
    }
    let (cause, reason) = if array.unresolved_operation {
        ("unresolved_operation", "przenoszenie wstrzymuje nierozwiązana operacja tej macierzy")
    } else if age.due_files == 0 {
        ("files_busy", "najstarsze pliki są otwarte albo przypisane do nieudanego przeniesienia, więc nie mogą zostać przeniesione")
    } else if restricted_to_schedule(array) {
        ("schedule_window", "przenoszenie jest ograniczone do okna harmonogramu")
    } else if last_run_moved_nothing(array) {
        ("last_run_moved_nothing", "ostatnie przenoszenie nie zabrało z cache żadnego pliku (sprawdź jego wynik w historii zadań, np. brak miejsca na dyskach danych)")
    } else {
        ("not_started", "automatyczne przenoszenie nie mogło wystartować (sprawdź stan macierzy)")
    };
    CacheStuckVerdict::Raise(
        store::AlertText::new(
            "elastic_cache_stuck",
            format!("Pliki zbyt długo czekają na cache macierzy {}", array.name),
            format!(
                "Najstarszy plik czeka na dysku cache {} (alarm po {}) i do przeniesienia na dyski danych nie chroni go parity: {reason}.",
                wait_text(oldest),
                wait_text(limit)
            ),
        )
        .param("array", &array.name)
        .param("oldest_secs", coarse_wait_secs(oldest))
        .param("limit_secs", coarse_wait_secs(limit))
        .param("cause", cause),
    )
}

/// A waiting time for the alert sentence, coarse on purpose: the sentence is
/// rewritten on every probe, and minute precision would rewrite it every time.
fn wait_text(secs: u64) -> String {
    match secs {
        s if s >= 2 * 86_400 => format!("{} dni", s / 86_400),
        s if s >= 3_600 => format!("{} h", s / 3_600),
        s => format!("{} min", s / 60),
    }
}

/// `wait_text`'s rounding as seconds, for the alert's parameters: cut down to
/// the unit the sentence shows, so the coded row changes exactly when the
/// sentence does and a refresh does not rewrite it on every probe.
fn coarse_wait_secs(secs: u64) -> u64 {
    match secs {
        s if s >= 2 * 86_400 => s / 86_400 * 86_400,
        s if s >= 3_600 => s / 3_600 * 3_600,
        s => s / 60 * 60,
    }
}

/// When each array last started an automatic mover run, and when its cache was
/// last probed for aged files.
///
/// One instance per caller-supplied clock; `global()` is the process-wide one
/// the tick uses, and a test makes its own.
#[derive(Debug, Default)]
pub struct MoverClock {
    started: std::sync::Mutex<BTreeMap<String, Instant>>,
    probed: std::sync::Mutex<BTreeMap<String, Instant>>,
}

impl MoverClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn global() -> &'static Self {
        static CLOCK: std::sync::OnceLock<MoverClock> = std::sync::OnceLock::new();
        CLOCK.get_or_init(MoverClock::new)
    }

    /// Records that a run has just been started for this array.
    pub fn started(&self, array: &str) {
        if let Ok(mut at) = self.started.lock() {
            at.insert(array.to_string(), Instant::now());
        }
    }

    /// Records that this array's cache has just been probed for aged files.
    pub fn probed(&self, array: &str) {
        if let Ok(mut at) = self.probed.lock() {
            at.insert(array.to_string(), Instant::now());
        }
    }

    /// Whether the age probe may run again: at most once per cooldown per
    /// array. A poisoned mutex answers `false`, for the reason `cooled_down`
    /// gives — an unknown clock must not turn the pacing off.
    pub fn probe_due(&self, array: &str) -> bool {
        let Ok(at) = self.probed.lock() else {
            return false;
        };
        at.get(array)
            .map(|t| t.elapsed() >= MOVER_RETRIGGER_COOLDOWN)
            .unwrap_or(true)
    }

    /// Whether no run has started for this array within `quiet`.
    pub fn idle_for(&self, array: &str, quiet: Duration) -> bool {
        let Ok(at) = self.started.lock() else {
            return false;
        };
        at.get(array).map(|t| t.elapsed() >= quiet).unwrap_or(true)
    }

    /// Whether the cooldown has expired. An array that has never run is
    /// cooled down — the first trigger must not wait.
    ///
    /// A POISONED MUTEX ANSWERS `false`, not `true`. The two "no answer"
    /// cases here are not the same: a missing mark means "this array has
    /// never run", which is a fact and lets the first trigger through, while
    /// a poisoned lock means a thread panicked holding it and this clock's
    /// contents are unknown. Answering `true` there would DISABLE the
    /// cooldown for every array from that moment on — a panic anywhere would
    /// turn the guard against retrigger storms off, permanently, and the
    /// symptom would be a node spawning mover jobs in a loop with nothing in
    /// the log to connect it to the panic. Failing closed only ever delays a
    /// move the scheduled run makes anyway.
    pub fn cooled_down(&self, array: &str) -> bool {
        self.idle_for(array, MOVER_RETRIGGER_COOLDOWN)
    }

    /// Moves an existing mark back in time so a test can reach the far side of
    /// the cooldown. It REQUIRES the mark to exist and says so if it does not
    /// — a version that silently created one would pass just as happily
    /// against a `started` that recorded nothing.
    #[cfg(test)]
    pub(crate) fn rewind_for_test(&self, array: &str, by: Duration) {
        let mut at = self.started.lock().expect("mover clock");
        let mark = at
            .get_mut(array)
            .unwrap_or_else(|| panic!("{array} has no mark to rewind — `started` did not record"));
        *mark = Instant::now() - by;
    }
}

// =============================================================================
// The wizard: refusals, warnings and the plan
// =============================================================================

/// The device a branch is mounted by: `/dev/disk/by-id/…` when this node
/// publishes one, the kernel name only as a fallback.
///
/// `BranchRow::device` documented this and `plan_layout` did not do it — it
/// copied `NasDisk::path`, which is `/dev/sdg`. Kernel names are assigned in
/// discovery order and a controller that enumerates differently after a
/// reboot renames them. `plan_mount` runs unattended on exactly that reboot,
/// would mount whatever now answers to `sdg` into the branch directory of the
/// disk that used to, and mergerfs would serve the result as part of the
/// array — with snapraid's parity computed against the disk that is no longer
/// there. The ZFS path in this same application has gone through
/// `zfs::stable_device_path` from the start; there is no reason for this one
/// to be the exception, and every reason for it not to be.
fn branch_device(disk: &NasDisk) -> String {
    super::zfs::stable_device_path(&disk.name)
}

/// A refusal, built where the rule lives so the sentence and the machine code
/// cannot drift apart.
fn refuse(code: &str, disk: &NasDisk, params: &[(&str, String)], detail: String) -> NasElasticRefusal {
    NasElasticRefusal {
        code: code.to_string(),
        disk_id: disk.disk_id.clone(),
        disk_name: disk.name.clone(),
        detail,
        params: refusal_params(params),
    }
}

/// A refusal's parameters, as the wire carries them.
fn refusal_params(params: &[(&str, String)]) -> BTreeMap<String, String> {
    params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

/// `conflicting_owner` as a code the wizard words: the kind of owner
/// ('spare' | 'pool' | 'md' | 'system' | 'mounted' | 'used') and the name it
/// goes by ('' when it has none). One rule, two spellings — kept beside the
/// sentence so the two cannot drift.
pub fn conflicting_owner_code(disk: &NasDisk) -> Option<(&'static str, String)> {
    let named = |name: Option<&String>| name.cloned().unwrap_or_default();
    if disk.vdev_role == "spare" {
        return Some(("spare", named(disk.member_of.as_ref())));
    }
    match disk.role.as_str() {
        "pool_member" => Some(("pool", named(disk.member_of.as_ref()))),
        "array_member" => Some(("md", named(disk.member_of.as_ref()))),
        "system" => Some(("system", String::new())),
        "mounted" => Some(("mounted", named(disk.mountpoints.first()))),
        "used" => Some(("used", String::new())),
        _ => None,
    }
}

/// Whether a disk is already owned by something else, and by what.
///
/// §5.3: a disk in an Elastic Array cannot be in a ZFS pool or be a spare. The
/// conflicting owner is NAMED, because "this disk is in use" sends an admin
/// hunting and "this disk is in the pool tank" does not. The inventory's
/// `role` is the authority — it is derived from what `lsblk` and `zpool`
/// actually report, not from what this app remembers writing.
///
/// ONE rule (wave-6 critic MINOR 9): the sentence is worded from
/// `conflicting_owner_code`, the rule itself, so the refusal and its words
/// cannot disagree about whether a disk is in use.
pub fn conflicting_owner(disk: &NasDisk) -> Option<String> {
    let (kind, name) = conflicting_owner_code(disk)?;
    let named = |with: String, without: &str| if name.is_empty() { without.to_string() } else { with };
    Some(match kind {
        "spare" => named(format!("a hot spare of pool {name}"), "a hot spare"),
        "pool" => named(format!("a member of ZFS pool {name}"), "a member of a ZFS pool"),
        "md" => named(format!("a member of md array {name}"), "a member of an md array"),
        "system" => "carrying this node's running system".to_string(),
        "mounted" => named(format!("mounted at {name}"), "mounted"),
        _ => "holding a filesystem or partitions".to_string(),
    })
}

/// Whether two inventory rows are the same piece of hardware, and by which
/// identity they were recognised as one.
///
/// Identity, in falling order of how much it proves: a WWN is assigned by the
/// device itself, a serial by its maker, and the `/dev` path is the weakest of
/// the three but catches a list that simply repeats an entry. Empty strings
/// prove nothing and are skipped — two disks that both report no serial are
/// not the same disk, and a check that concluded they were would refuse
/// perfectly good arrays on cheap hardware.
pub fn same_device(a: &NasDisk, b: &NasDisk) -> Option<String> {
    match (a.wwn.as_deref(), b.wwn.as_deref()) {
        (Some(x), Some(y)) if !x.is_empty() && x == y => return Some(format!("WWN {x}")),
        _ => {}
    }
    if !a.serial.is_empty() && a.serial == b.serial {
        return Some(format!("serial {}", a.serial));
    }
    if !a.path.is_empty() && a.path == b.path {
        return Some(format!("device {}", a.path));
    }
    None
}

/// Everything that stops this layout from being created.
///
/// A hard refusal with a named reason, never a warning — §5.3 is explicit
/// about the parity rule, and a warning an admin can click past on a parity
/// disk that is too small produces an array whose parity silently protects
/// nothing.
///
/// `taken` is the disk ids already claimed by other arrays on this node, which
/// the inventory cannot know until the store exists. `reserved` is every name
/// already mounted under `/mnt/` — the ZFS pools and the other arrays.
pub fn layout_refusals(
    name: &str,
    data: &[NasDisk],
    parity: &[NasDisk],
    cache: &[NasDisk],
    taken: &BTreeSet<String>,
    reserved: &BTreeSet<String>,
) -> Vec<NasElasticRefusal> {
    let mut out = Vec::new();

    if cache.len() > 1 {
        out.push(NasElasticRefusal {
            code: "too_many_cache_disks".to_string(),
            detail: "Elastic dopuszcza najwyżej jeden dysk cache".to_string(),
            params: refusal_params(&[("count", cache.len().to_string())]),
            ..Default::default()
        });
    }

    if !name.is_empty()
        && (tentanas_helper::elastic::validate_array_name(name).is_err()
            || matches!(name, "tentanas" | "tentanas-branches"))
    {
        out.push(NasElasticRefusal {
            code: "name_invalid".to_string(),
            detail: format!(
                "'{name}' cannot be an array name: it becomes a directory under /mnt, a \
                 directory under the branch root and a file name"
            ),
            params: refusal_params(&[("name", name.to_string())]),
            ..Default::default()
        });
    }
    // An array and a ZFS pool share ONE mountpoint namespace, which is what
    // lets n05 list them in one table and lets a share name either without
    // knowing which it is. It is also why this is a refusal and not a
    // warning: mounting a mergerfs union at `/mnt/media` over a mounted pool
    // called `media` hides the pool's data behind the union and sends every
    // subsequent write into the union instead of the pool.
    if !name.is_empty() && reserved.contains(name) {
        out.push(NasElasticRefusal {
            code: "name_taken".to_string(),
            detail: format!(
                "'{name}' is already mounted at {}: a pool and an array cannot share a \
                 mountpoint, and the union would hide what is under it",
                union_path(name)
            ),
            params: refusal_params(&[("name", name.to_string()), ("path", union_path(name))]),
            ..Default::default()
        });
    }
    if data.is_empty() {
        out.push(NasElasticRefusal {
            code: "no_data_disks".to_string(),
            detail: "an Elastic Array needs at least one data disk".to_string(),
            ..Default::default()
        });
    }
    if parity.len() > tentanas_helper::elastic::MAX_PARITY {
        out.push(NasElasticRefusal {
            code: "too_many_parity".to_string(),
            detail: format!(
                "{} parity disks: this array supports at most {}",
                parity.len(),
                tentanas_helper::elastic::MAX_PARITY
            ),
            params: refusal_params(&[
                ("count", parity.len().to_string()),
                ("max", tentanas_helper::elastic::MAX_PARITY.to_string()),
            ]),
            ..Default::default()
        });
    }

    // One disk in two roles would be formatted twice by the same plan.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (role, disks) in [("data", data), ("parity", parity), ("cache", cache)] {
        for disk in disks {
            if let Some(previous) = seen.insert(disk.disk_id.as_str(), role) {
                out.push(refuse(
                    "disk_repeated",
                    disk,
                    &[("first_role", previous.to_string()), ("role", role.to_string())],
                    format!("{} is picked as {previous} and as {role}", disk.name),
                ));
            }
        }
    }

    // Różne identyfikatory inwentarza mogą wskazywać ten sam nośnik; jego
    // powtórne użycie w dowolnej roli pozwoliłoby formatować go wielokrotnie.
    let selected: Vec<(&str, &NasDisk)> = [("data", data), ("parity", parity), ("cache", cache)]
        .into_iter()
        .flat_map(|(role, disks)| disks.iter().map(move |disk| (role, disk)))
        .collect();
    for (i, (a_role, a)) in selected.iter().enumerate() {
        for (b_role, b) in selected.iter().skip(i + 1) {
            if a.disk_id == b.disk_id {
                continue;
            }
            let Some(shared) = same_device(a, b) else {
                continue;
            };
            out.push(refuse(
                if *a_role == "data" && *b_role == "data" {
                    "data_disks_same_device"
                } else {
                    "disk_repeated"
                },
                b,
                // The shared identity (a WWN, a serial) stays in the node's
                // sentence: the screen names the two disks, not the id.
                &[
                    ("other", a.name.clone()),
                    ("other_role", a_role.to_string()),
                    ("role", b_role.to_string()),
                ],
                format!(
                    "{} ({a_role}) i {} ({b_role}) wskazują ten sam nośnik ({shared}): \
                     jeden nośnik nie może zajmować dwóch miejsc w macierzy",
                    a.name, b.name
                ),
            ));
        }
    }

    for (role, disks) in [("data", data), ("parity", parity), ("cache", cache)] {
        for disk in disks {
            if taken.contains(&disk.disk_id) {
                out.push(refuse(
                    "disk_in_use",
                    disk,
                    &[("owner", "elastic".to_string()), ("role", role.to_string())],
                    format!("{} already belongs to another Elastic Array", disk.name),
                ));
                continue;
            }
            // Keyed on the rule alone: the sentence is worded from it.
            if let Some((kind, owner_name)) = conflicting_owner_code(disk) {
                let owner = conflicting_owner(disk).unwrap_or_default();
                out.push(refuse(
                    "disk_in_use",
                    disk,
                    &[("owner", kind.to_string()), ("owner_name", owner_name), ("role", role.to_string())],
                    format!("{} is {owner} — a disk belongs to one of them, not both (the {role} role of this array would erase it)", disk.name),
                ));
            }
        }
    }

    // THE parity rule of §5.3. Every parity disk must be at least as large as
    // the LARGEST data disk, because SnapRAID's parity is computed block for
    // block across the data disks and a parity file cannot be shorter than
    // the longest one it covers.
    //
    // MEASURED (2026-09-06, snapraid 14.7): snapraid does NOT check this. A
    // 64 MiB parity file was configured against a 200 MiB data disk holding
    // 40 MiB, and the sync succeeded (`Resizing...`). The measurement is
    // narrow — it shows there is no capacity check at configuration time and
    // none at sync time while the data still fits — but the consequence is
    // not: THIS REFUSAL IS THE ONLY WARNING THE ADMIN EVER GETS. snapraid
    // will refuse once the data outgrows the parity disk, months later, at
    // the moment the array most needs to be protected. Hence a refusal here
    // and never a warning somebody can click past.
    if let Some(largest) = data.iter().map(|d| d.size_bytes).max() {
        for disk in parity {
            if disk.size_bytes < largest {
                out.push(refuse(
                    "parity_too_small",
                    disk,
                    &[("size", disk.size_bytes.to_string()), ("largest", largest.to_string())],
                    format!(
                        "{} holds {} and the largest data disk holds {}: a parity disk must be \
                         at least as large as the largest data disk, or it cannot cover it",
                        disk.name,
                        human_bytes(disk.size_bytes),
                        human_bytes(largest)
                    ),
                ));
            }
        }
    }
    out
}

/// Things an admin should know and may still choose, as the node's English
/// (`warnings`, for an older screen) — one sentence per code of
/// `layout_warning_codes`.
fn layout_warnings(data: &[NasDisk], parity: &[NasDisk], cache: &[NasDisk]) -> Vec<String> {
    layout_warning_codes(data, parity, cache).iter().map(layout_warning_sentence).collect()
}

/// The same warnings as CODES the wizard words in the reader's language:
/// - `no_parity` {} — no protection at all;
/// - `parity_tight` {disk} — a parity disk only just as large as the largest
///   data disk;
/// - `no_cache` {} — nothing for the mover to do;
/// - `unhealthy_disks` {disks} — kernel names SMART warns about, comma-separated;
/// - `mixed_sizes` {} — the data disks differ in size, which is the point of
///   this array kind.
pub fn layout_warning_codes(data: &[NasDisk], parity: &[NasDisk], cache: &[NasDisk]) -> Vec<NasHealthReason> {
    let mut out = Vec::new();
    if parity.is_empty() {
        out.push(super::disks::coded_reason("no_parity", &[]));
    }
    // A parity disk exactly the size of the largest data disk passes the hard
    // rule and can still come up short: the parity FILE lives on a
    // filesystem, and the filesystem's own metadata takes space the file
    // cannot have. Not a refusal — it may well fit — but it is the failure an
    // admin discovers weeks later, on the first sync that fills the disk.
    if let Some(largest) = data.iter().map(|d| d.size_bytes).max() {
        for disk in parity {
            if disk.size_bytes >= largest && disk.size_bytes < largest + largest / 100 {
                out.push(super::disks::coded_reason("parity_tight", &[("disk", disk.name.clone())]));
            }
        }
    }
    if cache.is_empty() {
        out.push(super::disks::coded_reason("no_cache", &[]));
    }
    let unhealthy: Vec<&str> = data
        .iter()
        .chain(parity.iter())
        .chain(cache.iter())
        .filter(|d| matches!(d.health.as_str(), "warning" | "critical"))
        .map(|d| d.name.as_str())
        .collect();
    if !unhealthy.is_empty() {
        out.push(super::disks::coded_reason("unhealthy_disks", &[("disks", unhealthy.join(", "))]));
    }
    // Mixed sizes are the POINT of this array kind, so they are not warned
    // about the way a ZFS vdev warns about them — the note says the opposite
    // of the ZFS one on purpose.
    if data.len() > 1 {
        let smallest = data.iter().map(|d| d.size_bytes).min().unwrap_or(0);
        let largest = data.iter().map(|d| d.size_bytes).max().unwrap_or(0);
        if largest > smallest {
            out.push(super::disks::coded_reason("mixed_sizes", &[]));
        }
    }
    out
}

/// The English of one `layout_warning_codes` entry, as the wizard used to
/// show it.
fn layout_warning_sentence(code: &NasHealthReason) -> String {
    let param = |key: &str| super::disks::reason_param(code, key).to_string();
    match code.code.as_str() {
        "no_parity" => "no parity disk: this array has no protection at all, and losing one data disk \
             loses that disk's files"
            .to_string(),
        "parity_tight" => format!(
            "{} is only just as large as the largest data disk: the parity file has to \
             fit on a filesystem, and its metadata may leave too little room",
            param("disk")
        ),
        "no_cache" => "no cache disk: there is nothing for the mover to do, and new files are written \
             straight to the data disks"
            .to_string(),
        "unhealthy_disks" => format!(
            "SMART warnings on {}: building an array on a disk that is already failing starts \
             it degraded",
            param("disks")
        ),
        "mixed_sizes" => "the data disks are different sizes, and every byte of each of them is used: \
             that is what this array kind is for"
            .to_string(),
        other => other.to_string(),
    }
}

/// The wizard's whole answer for a set of picked disks.
///
/// `tools` is injected so a preview can be rendered on a node that does not
/// have mergerfs or snapraid installed yet — refusing to SHOW an admin the
/// plan because the tool is missing is backwards, since the plan is what tells
/// them to install it.
#[allow(clippy::too_many_arguments)]
pub fn plan_layout(
    name: &str,
    filesystem: &str,
    data: &[NasDisk],
    parity: &[NasDisk],
    cache: &[NasDisk],
    taken: &BTreeSet<String>,
    reserved: &BTreeSet<String>,
    // `available_filesystems`: what this node can actually make, from
    // `capabilities().filesystems`. Empty means "nobody asked", and the
    // availability check is then skipped rather than refusing everything.
    available_filesystems: &[String],
    tools: &Tools,
) -> NasElasticPlan {
    let mut refusals = layout_refusals(name, data, parity, cache, taken, reserved);
    // THE FILESYSTEM WAS NEVER CHECKED, and an empty refusal list is the
    // protocol's word for "the create button is live". A request naming
    // `btrfs` used to sail past every rule here, fail inside `plan_create`,
    // land in the `Err` arm below — and come back with NO refusals and an
    // enabled button over a plan that cannot run.
    let wanted = if filesystem.is_empty() { "xfs" } else { filesystem };
    if !tentanas_helper::elastic::FILESYSTEMS.contains(&wanted) {
        refusals.push(NasElasticRefusal {
            code: "filesystem_invalid".to_string(),
            params: refusal_params(&[("filesystem", wanted.to_string())]),
            detail: format!(
                "'{wanted}' is not a filesystem this app makes: an Elastic Array data disk \
                 carries {} so it stays readable on its own",
                tentanas_helper::elastic::FILESYSTEMS.join(" or ")
            ),
            ..Default::default()
        });
    } else if !available_filesystems.is_empty()
        && !available_filesystems.iter().any(|f| f == wanted)
    {
        refusals.push(NasElasticRefusal {
            code: "filesystem_unavailable".to_string(),
            params: refusal_params(&[("filesystem", wanted.to_string())]),
            detail: format!(
                "mkfs.{wanted} is not installed on this node, so no disk of this array can \
                 be prepared"
            ),
            ..Default::default()
        });
    }
    let warnings = layout_warnings(data, parity, cache);
    let warning_codes = layout_warning_codes(data, parity, cache);

    let usable: u64 = data.iter().map(|d| d.size_bytes).sum();
    let parity_bytes: u64 = parity.iter().map(|d| d.size_bytes).sum();
    let cache_bytes: u64 = cache.iter().map(|d| d.size_bytes).sum();

    let array_name = if name.is_empty() { "array" } else { name };
    let row = ElasticArrayRow {
        name: array_name.to_string(),
        enabled: true,
        filesystem: if filesystem.is_empty() {
            "xfs".to_string()
        } else {
            filesystem.to_string()
        },
        branches: data
            .iter()
            .map(|d| BranchRow {
                disk_id: d.disk_id.clone(),
                name: d.name.clone(),
                device: branch_device(d),
                role: "data".to_string(),
            })
            .chain(cache.iter().map(|d| BranchRow {
                disk_id: d.disk_id.clone(),
                name: d.name.clone(),
                device: branch_device(d),
                role: "cache".to_string(),
            }))
            .collect(),
        parity: parity
            .iter()
            .enumerate()
            .map(|(i, d)| ParityRow {
                disk_id: d.disk_id.clone(),
                name: d.name.clone(),
                device: branch_device(d),
                index: (i + 1) as u8,
            })
            .collect(),
        mover: MoverConfig::default(),
        ..Default::default()
    };

    // A refused layout gets NO plan and NO wipe list. The two are the same
    // decision: the plan is what would run, and nothing would run — offering
    // the steps next to the refusal invites the reading that they are one
    // click away.
    let (steps_preview, wiped) = if refusals.is_empty() {
        match tentanas_helper::elastic::plan_create(&row.spec(), tools) {
            Ok(steps) => (
                tentanas_helper::elastic::render(&steps),
                tentanas_helper::elastic::wiped_devices(&steps),
            ),
            // A spec the helper refuses after the model accepted it is a
            // disagreement between the two — and it has to become a REFUSAL,
            // not a sentence in the preview box. The protocol says an empty
            // `refusals` means the create button is live, so leaving this arm
            // without one offered the admin a button over a plan the node had
            // already declined to build. The admin still sees the helper's
            // own words, in the place that stops the button.
            Err(e) => {
                refusals.push(NasElasticRefusal {
                    code: "plan_failed".to_string(),
                    detail: format!("this layout cannot be turned into a plan: {e}"),
                    ..Default::default()
                });
                (String::new(), Vec::new())
            }
        }
    } else {
        (String::new(), Vec::new())
    };

    NasElasticPlan {
        usable_bytes: usable,
        raw_bytes: usable + parity_bytes + cache_bytes,
        parity_bytes,
        cache_bytes,
        fault_tolerance: parity.len().min(u8::MAX as usize) as u8,
        refusals,
        warnings,
        union_path: union_path(array_name),
        wiped_devices: wiped,
        steps_preview,
        warning_codes,
    }
}

/// `4.0 TB` — decimal, the way a disk is sold and the way the wizard names it.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// =============================================================================
// "Installed" is not "working": the snapraid health probe
// =============================================================================

/// What a health probe of the snapraid binary concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolHealth {
    /// It ran and did the work.
    Working,
    /// It is on the node and it does NOT work, with the reason.
    Broken(String),
    /// Wynik nie pozwala rozstrzygnąć, czy narzędzie działa.
    Unknown(String),
}

/// The smallest snapraid configuration that makes the binary do real work.
///
/// WHY THIS EXISTS AT ALL, and it is the same defect family as a measured
/// zero. MEASURED (2026-09-06, snapraid 14.7): a build made with this
/// machine's default flags (`-march=native -O3 -flto`) SEGFAULTS on `status`
/// and on `sync`, while `snapraid --version` answers perfectly happily. So
/// "the binary is present and prints a version" is not evidence that parity
/// works — and a snapraid that crashes is a snapraid that never reports a
/// parity error, which means an array that LOOKS protected and is not. That
/// is the worst thing this application can display. Rebuilding at plain `-O2`
/// fixed it completely, and the AUR package's own self-test caught it, which
/// is the shape this probe copies: run something that computes.
///
/// `--version` is therefore NOT the probe. `status` against a minimal but
/// VALID configuration is, because that is what was measured crashing.
///
/// This is deliberately not `tentanas_helper::elastic::snapraid_config`: that
/// one describes a real array on real branch paths, and the probe must not
/// need either. Kept next to it in spirit, separate in fact.
pub fn probe_config(dir: &std::path::Path) -> String {
    let dir = dir.display();
    // One `push_str` per line, not a multi-line `format!`.
    //
    // The `format!` version indented every directive by nine spaces — the
    // continuation's own leading whitespace, which is inside the literal.
    // MEASURED (2026-09-06, snapraid 14.7): snapraid parses an indented
    // config exactly as it parses a flush one, so nothing would ever have
    // gone wrong at run time and nothing would ever have said so. The only
    // thing it broke was the assertion sharp enough to see it — a
    // `starts_with("data ")` count, which read 0. The loop beside it used
    // `contains("data probe ")`, which indentation is invisible to. That is
    // the lesson worth more than the fix: `contains` passes, `starts_with`
    // catches.
    let mut out = String::new();
    out.push_str("# TentaNas snapraid health probe. Throwaway: nothing here is an array.\n");
    out.push_str(&format!("parity {dir}/parity/snapraid.parity\n"));
    out.push_str(&format!("content {dir}/snapraid.content\n"));
    out.push_str(&format!("data probe {dir}/data\n"));
    out.push_str("blocksize 256\n");
    out
}

/// The argv of the health probe.
pub fn probe_args(config: &std::path::Path) -> Vec<String> {
    vec![
        "-c".to_string(),
        config.display().to_string(),
        "status".to_string(),
    ]
}

/// Co oznacza wynik sondy działającej na minimalnej konfiguracji.
/// Kod 1 jest oczekiwany tylko z diagnostyką braku kopii content na różnych
/// dyskach. Sam komunikat Self-test pojawia się również przed awarią binarki.
pub fn probe_verdict(code: i32, stdout: &str, stderr: &str) -> ToolHealth {
    // Two spellings of "killed by a signal" reach us: `broker::run_unprivileged`
    // reports -1 when the child had no exit status, and a shell-style wrapper
    // reports 128 + signo — 139 for SIGSEGV, which is what was measured.
    if code < 0 || code >= 128 {
        let signal = if code >= 128 {
            format!(" by signal {}", code - 128)
        } else {
            String::new()
        };
        return ToolHealth::Broken(format!(
            "snapraid is installed but was killed{signal} while reading a trivial configuration. A build made with -march=native -O3 -flto is known to segfault on `status` and `sync`; rebuilding at -O2 fixes it. Until then this node cannot sync or scrub and its parity would never report an error, so the array would look protected and would not be."
        ));
    }
    let expected_diagnostic = "You must have at least 2 'content' files in different disks.";
    if code == 0
        || (code == 1
            && stdout
                .lines()
                .chain(stderr.lines())
                .any(|line| line.trim() == expected_diagnostic))
    {
        ToolHealth::Working
    } else {
        ToolHealth::Unknown(format!(
            "Sonda SnapRAID zakończyła się kodem {code} bez rozpoznanego potwierdzenia działania"
        ))
    }
}

// =============================================================================
// Capabilities
// =============================================================================

// The output borrows from `features`, never from `id`, and with two input
// lifetimes the compiler cannot guess that — E0106. Naming it is the fix;
// eliding it broke the whole crate for every session sharing this tree.
fn feature<'a>(features: &'a [FeatureState], id: &str) -> Option<&'a FeatureState> {
    features.iter().find(|f| f.id == id)
}

/// Whether this node can run an Elastic Array, from the Environment probes
/// rather than from an assumption.
pub fn capabilities(features: &[FeatureState], has_mkfs: &dyn Fn(&str) -> bool) -> NasElasticCapabilities {
    let mergerfs = feature(features, MERGERFS_FEATURE_ID);
    let snapraid = feature(features, SNAPRAID_FEATURE_ID);
    // Only `ok` counts. A row the health probe downgraded to `broken` is
    // present, versioned and unusable, and it must not read as a capability —
    // see `probe_verdict` for why a snapraid that crashes is worse than one
    // that is missing.
    let ok = |f: Option<&FeatureState>| f.is_some_and(|f| f.status == "ok");
    let filesystems: Vec<String> = tentanas_helper::elastic::FILESYSTEMS
        .iter()
        .copied()
        .filter(|fs| has_mkfs(fs))
        .map(String::from)
        .collect();

    let mut reasons = Vec::new();
    if !ok(mergerfs) {
        reasons.push(format!(
            "mergerfs: {}",
            mergerfs.map(|f| f.detail.clone()).unwrap_or_else(|| "not probed".to_string())
        ));
    }
    if !ok(snapraid) {
        reasons.push(format!(
            "snapraid: {}",
            snapraid.map(|f| f.detail.clone()).unwrap_or_else(|| "not probed".to_string())
        ));
    }
    if filesystems.is_empty() {
        reasons.push(
            "neither mkfs.xfs nor mkfs.ext4 is installed, so no data disk can be prepared"
                .to_string(),
        );
    }

    NasElasticCapabilities {
        mergerfs: ok(mergerfs),
        mergerfs_version: mergerfs.and_then(|f| f.version.clone()).unwrap_or_default(),
        snapraid: ok(snapraid),
        snapraid_version: snapraid.and_then(|f| f.version.clone()).unwrap_or_default(),
        filesystems,
        detail: reasons.join("; "),
    }
}

/// A disk with no other owner: the wizard's candidate list.
pub fn free_disks(disks: &[NasDisk], taken: &BTreeSet<String>) -> Vec<NasDisk> {
    disks
        .iter()
        .filter(|d| conflicting_owner(d).is_none() && !taken.contains(&d.disk_id))
        .cloned()
        .collect()
}

// =============================================================================
// To the wire
// =============================================================================

/// What the screen calls a member: a LIVE kernel name, or nothing.
///
/// The naming rule is `disks::pick_shown_name`: the inventory's name for the
/// `disk_id` first, then the name the helper observed for the member's device
/// in this same read (`observed`). An empty answer means neither source has
/// the disk now; the last-known name then travels separately
/// (`disk_last_name`), so the screen can mark it as last-known instead of
/// presenting it as the current device.
///
/// The rows name members by slot (`d1`, `parity1`) because that is what the
/// branch directory and the snapraid entry are called, and `device` is a
/// by-uuid path — neither is a disk anybody can find in a shelf, and both
/// used to be printed as the disk's name.
fn member_disk_name(disks: &BTreeMap<String, NasDisk>, disk_id: &str, observed: Option<&str>) -> String {
    let inventory = disks.get(disk_id).map(|d| d.name.clone());
    match super::disks::pick_shown_name(inventory, observed, || None) {
        super::disks::ShownDiskName::Live(name) => name,
        _ => String::new(),
    }
}

/// Adds the name a member was LAST seen under to every member no live source
/// could name — the third step of the naming rule. The screen marks it as
/// last-known; it is never put into `disk_name`, which is the device now.
pub(crate) fn fill_last_known_names(
    array: &mut NasElasticArray,
    last_known: impl Fn(&str) -> Option<String>,
) {
    let fill = |disk_name: &str, disk_id: &str, into: &mut String| {
        if disk_name.is_empty() {
            *into = last_known(disk_id).map(|n| n.trim().to_string()).unwrap_or_default();
        }
    };
    for branch in array.data_disks.iter_mut().chain(array.cache_disks.iter_mut()) {
        fill(&branch.disk_name, &branch.disk_id, &mut branch.disk_last_name);
    }
    for parity in array.parity_disks.iter_mut() {
        fill(&parity.disk_name, &parity.disk_id, &mut parity.disk_last_name);
    }
    if let Some(pending) = array.pending_add_disk.as_mut() {
        fill(&pending.disk_name, &pending.disk_id, &mut pending.disk_last_name);
    }
}

fn branch_to_protocol(
    array: &ElasticArrayRow,
    branch: &BranchRow,
    disks: &BTreeMap<String, NasDisk>,
    observed: &ArrayObservation,
) -> NasElasticBranch {
    let mountpoint = if branch.role == "cache" {
        cache_branch_path(&array.name, &branch.name)
    } else {
        data_branch_path(&array.name, &branch.name)
    };
    let probe = observed.probe(&mountpoint);
    let disk = disks.get(&branch.disk_id);
    NasElasticBranch {
        disk_id: branch.disk_id.clone(),
        name: branch.name.clone(),
        device: branch.device.clone(),
        disk_name: member_disk_name(
            disks,
            &branch.disk_id,
            observed.kernel_names.get(&mountpoint).map(String::as_str),
        ),
        disk_last_name: String::new(),
        kind: disk.map(|d| d.kind.clone()).unwrap_or_else(|| "unknown".to_string()),
        role: branch.role.clone(),
        filesystem: array.filesystem.clone(),
        mountpoint,
        size_bytes: probe.size_bytes,
        used_bytes: probe.used_bytes,
        free_bytes: probe.free_bytes,
        mounted: probe.mounted,
        device_present: probe.device_present,
        // The disk's health is the DISK's, and a disk this node has never
        // read SMART from is 'unknown' rather than 'ok'.
        health: disk
            .map(|d| d.health.clone())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

pub(crate) fn mover_to_protocol(run: &ElasticMoverRun) -> NasMoverRun {
    let outcome = match run.phase {
        ElasticMoverPhase::Announced | ElasticMoverPhase::Moving | ElasticMoverPhase::Syncing => "running",
        // Stopped with something unresolved; the union keeps serving, and the
        // next run finishes or reverses what it left.
        ElasticMoverPhase::NeedsAttention if run.finished_at.is_none() => "needs_attention",
        // Closed without finishing, by the run after it (or by the Resume of a
        // Hold an older helper left).
        ElasticMoverPhase::NeedsAttention => "failed",
        ElasticMoverPhase::Complete
            if run.skipped_files > 0
                || run.refused_files > 0
                || run.issues.iter().any(|issue| issue.kind == ElasticMoverIssueKind::Attention)
                // Its Sync skipped files that changed while it ran.
                || run.coupled_sync.as_ref().is_some_and(|sync| sync.outcome == ElasticSnapraidOutcome::Partial) =>
        {
            "partial"
        }
        ElasticMoverPhase::Complete => "ok",
    };
    // The wire has no per-file list: refusals and the first reported paths
    // travel in the detail, so a partial run says what it left behind.
    let mut detail = run.detail.clone().unwrap_or_default();
    if run.refused_files > 0 || !run.issues.is_empty() {
        let issues = run
            .issues
            .iter()
            .take(3)
            .map(|issue| format!("{}: {}", issue.path, issue.reason))
            .collect::<Vec<_>>()
            .join("; ");
        let summary = format!(
            "pominięto {}, odmówiono {}: {issues}",
            run.skipped_files, run.refused_files
        );
        detail = if detail.is_empty() { summary } else { format!("{detail}; {summary}") };
    }
    // Records the helper could neither finish nor withdraw stay listed until
    // an admin acknowledges them; the wire carries them in the detail.
    if !run.stuck_records.is_empty() || run.stuck_hidden > 0 {
        let records = run
            .stuck_records
            .iter()
            .take(3)
            .map(|record| format!("{} (operacja {})", record.path, record.operation_id))
            .collect::<Vec<_>>()
            .join("; ");
        // Paths whose full record the helper no longer shows are still skipped.
        let hidden = if run.stuck_hidden > 0 {
            format!(" (+{} bez pełnego rekordu)", run.stuck_hidden)
        } else {
            String::new()
        };
        // What THIS run evicted: a path that left the skip set is no longer
        // skipped at all. The array's lifetime figure lives on the state, not
        // here, so a later clean run says nothing.
        let evicted = if run.stuck_evicted > 0 {
            format!("; wyparto z listy pominięć: {}", run.stuck_evicted)
        } else {
            String::new()
        };
        let listed = if records.is_empty() { String::new() } else { format!(": {records}") };
        let summary =
            format!("utknięte rekordy: {}{hidden}{listed}{evicted}", run.stuck_records.len());
        detail = if detail.is_empty() { summary } else { format!("{detail}; {summary}") };
    }
    NasMoverRun {
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        outcome: outcome.to_string(),
        moved_bytes: run.moved_bytes,
        moved_files: run.moved_files,
        skipped_files: run.skipped_files,
        skipped_bytes: run.skipped_bytes,
        counts_known: run.counts_known,
        detail,
        coupled_sync: run.coupled_sync.as_ref().map(|sync| NasSnapraidRun {
            operation_id: Some(sync.operation_id.clone()),
            kind: snapraid_kind_to_protocol(&sync.kind),
            started_at: sync.started_at.clone(),
            finished_at: sync.finished_at.clone(),
            outcome: snapraid_outcome_to_protocol(sync.outcome),
            detail: sync.detail.clone().unwrap_or_default(),
            errors: None,
            exit_code: sync.exit_code,
            total_blocks: sync.total_blocks,
            checked_blocks: sync.checked_blocks,
            accessed_mb: sync.accessed_mb,
            errors_file: sync.errors_file,
            errors_io: sync.errors_io,
            errors_data: sync.errors_data,
            ..Default::default()
        }),
    }
}

fn snapraid_kind_to_protocol(kind: &ElasticSnapraidKind) -> String {
    snapraid_kind(kind).to_string()
}

pub(crate) fn snapraid_outcome_to_protocol(outcome: ElasticSnapraidOutcome) -> String {
    match outcome {
        ElasticSnapraidOutcome::Running => "running",
        ElasticSnapraidOutcome::Succeeded => "ok",
        ElasticSnapraidOutcome::Failed => "failed",
        ElasticSnapraidOutcome::NeedsAttention => "needs_attention",
        ElasticSnapraidOutcome::Refused => "refused",
        ElasticSnapraidOutcome::Partial => "partial",
        ElasticSnapraidOutcome::NothingRepaired => "nothing_repaired",
    }.to_string()
}

/// One array as the wire carries it.
pub fn to_protocol(
    array: &ElasticArrayRow,
    disks: &BTreeMap<String, NasDisk>,
    observed: &ArrayObservation,
    snapraid_installed: bool,
    snapraid_version: &str,
    state: (&str, &str),
) -> NasElasticArray {
    let data_disks: Vec<NasElasticBranch> = array
        .data()
        .map(|b| branch_to_protocol(array, b, disks, observed))
        .collect();
    let cache_disks: Vec<NasElasticBranch> = array
        .cache()
        .map(|b| branch_to_protocol(array, b, disks, observed))
        .collect();
    let parity_disks: Vec<NasElasticParity> = array
        .parity
        .iter()
        .map(|p| {
            let mountpoint = parity_mount_path(&array.name, p.index);
            let probe = observed.probe(&mountpoint);
            NasElasticParity {
                disk_id: p.disk_id.clone(),
                name: p.name.clone(),
                device: p.device.clone(),
                disk_name: member_disk_name(
                    disks,
                    &p.disk_id,
                    observed.kernel_names.get(&mountpoint).map(String::as_str),
                ),
                disk_last_name: String::new(),
                index: p.index,
                mountpoint,
                parity_file: parity_file_path(&array.name, p.index),
                size_bytes: probe.size_bytes,
                used_bytes: probe.used_bytes,
                mounted: probe.mounted,
                device_present: probe.device_present,
                health: disks
                    .get(&p.disk_id)
                    .map(|d| d.health.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
            }
        })
        .collect();

    // Capacity is the DATA branches and nothing else: parity adds none (the
    // explain box in the wizard says so in one sentence) and the cache is a
    // staging area, not capacity an admin can plan with. A sum over branches
    // one of which could not be read is `None`, not a smaller number.
    //
    // It is summed PER BRANCH, and that is not a style choice. MEASURED
    // (2026-09-06, mergerfs 2.42.0): `df` on the union reports one branch,
    // not the total — so the obvious implementation, one `statfs` on
    // `/mnt/<array>`, would understate a three-disk array by two thirds and
    // look perfectly plausible doing it. `ArrayObservation` has no field to
    // hold a union statfs, so this is the only sum there can be.
    let sum = |branches: &[NasElasticBranch], f: fn(&NasElasticBranch) -> Option<u64>| {
        branches
            .iter()
            .try_fold(0u64, |acc, b| f(b).map(|v| acc + v))
    };

    let protection = protection(array, observed);
    let health = health_of(&protection, &data_disks, &cache_disks, &parity_disks, state.0, observed.conflicts.len());

    NasElasticArray {
        name: array.name.clone(),
        kind: KIND.to_string(),
        state: state.0.to_string(),
        state_detail: state.1.to_string(),
        health: health.0.to_string(),
        health_reason: health.1,
        enabled: array.enabled,
        union_path: array.union_path(),
        create_policy: array.spec().mergerfs.create_policy,
        filesystem: array.filesystem.clone(),
        usable_bytes: sum(&data_disks, |b| b.size_bytes),
        used_bytes: sum(&data_disks, |b| b.used_bytes),
        cache_size_bytes: sum(&cache_disks, |b| b.size_bytes),
        cache_used_bytes: sum(&cache_disks, |b| b.used_bytes),
        data_disks,
        cache_disks,
        parity_disks,
        folders: array
            .folders
            .iter()
            .map(|f| NasElasticFolder {
                name: f.name.clone(),
                path: format!("{}/{}", array.union_path(), f.name),
                cache_policy: f.cache_policy.clone(),
                used_bytes: None,
                share_id: f.share_id.clone(),
                share_label: f.share_label.clone(),
            })
            .collect(),
        folders_known: array.folders_known,
        mover: NasMoverSettings {
            enabled: array.mover.schedule_enabled,
            schedule: array.mover.schedule.clone(),
            min_age_secs: array.mover.min_age_secs,
            cache_min_free_pct: array.mover.cache_min_free_pct,
            coupled_sync: array.mover.coupled_sync,
            configured: array.mover.configured,
            last_run: observed.last_mover.as_ref().map(mover_to_protocol),
            // The RECORDED runs. Observation never invents one: what the
            // helper reports live is `last_run` above, and the strip below it
            // shows only what this node persisted.
            history: array.mover_history.clone(),
        },
        snapraid: NasSnapraidState {
            installed: snapraid_installed,
            version: snapraid_version.to_string(),
            config_path: if array.parity.is_empty() { String::new() } else { config_path(&array.name) },
            last_sync: array.last_sync_run.clone().or_else(|| observed.last_sync_at.as_ref().map(|at| tentaflow_protocol::tentanas::NasSnapraidRun {
                kind: "sync".to_string(), started_at: String::new(), finished_at: Some(at.clone()),
                outcome: "ok".to_string(), detail: "Potwierdzony checkpoint helpera; nie jest pomiarem późniejszych zapisów".to_string(),
                errors: None,
                ..Default::default()
            })),
            last_scrub: array.last_scrub_run.clone(),
            history: array.snapraid_history.clone(),
            sync_schedule: array.snapraid.sync_schedule.clone(),
            scrub_schedule: array.snapraid.scrub_schedule.clone(),
            sync_schedule_enabled: array.snapraid.sync_enabled,
            scrub_schedule_enabled: array.snapraid.scrub_enabled,
            scrub_percent: array.snapraid.scrub_percent,
            scrub_older_than_days: array.snapraid.scrub_older_than_days,
            parity_errors: observed.parity_errors,
            parity_errors_window_days: PARITY_ERRORS_WINDOW_DAYS,
        },
        protection,
        unresolved_operation: array.unresolved_operation,
        mover_settles_unresolved: array.mover_settles_unresolved,
        parity_run_available: array.parity_run_available,
        attention: String::new(),
        sync_needs_acknowledgement: sync_acknowledgement_needed(&array.snapraid_history),
        sync_fault_id: unacknowledged_fault(&array.snapraid_history)
            .and_then(|run| run.operation_id.clone())
            .unwrap_or_default(),
        pending_add_disk: array.pending_add.as_ref().map(|disk| NasElasticPendingAdd {
            disk_id: disk.disk_id.clone(),
            disk_name: member_disk_name(disks, &disk.disk_id, None),
            ..Default::default()
        }),
        created_at: array.created_at.clone(),
        updated_at: array.updated_at.clone(),
        // `observed_protocol` sets the codes of the detail it chose.
        state_reasons: Vec::new(),
    }
}

/// Whether a manual Sync on this history needs the admin's acknowledgement,
/// from the recorded runs alone: a Scrub that marked blocks nothing has
/// repaired (`unresolved_parity_fault`), or a Repair that failed and that no
/// later Repair or full Scrub settled. The helper's own recorded cause is
/// added by `observed_protocol`; either one asks for the confirm, and the
/// helper is what enforces it.
pub fn sync_acknowledgement_needed(history: &[NasSnapraidRun]) -> bool {
    unacknowledged_fault(history).is_some()
}

/// The recorded run a Sync's acknowledgement has to name, from the history
/// alone: the unresolved Scrub fault, or else the Repair that failed and
/// that no later Repair or full Scrub settled.
pub fn unacknowledged_fault(history: &[NasSnapraidRun]) -> Option<&NasSnapraidRun> {
    if let Some(run) = unresolved_parity_fault(history) {
        return Some(run);
    }
    for run in history {
        match (run.kind.as_str(), run.outcome.as_str()) {
            ("fix" | "scrub", "ok") => return None,
            ("fix", "failed" | "needs_attention") => return Some(run),
            _ => (),
        }
    }
    None
}

/// The code of the helper's recorded cause, as `NasElasticArray::attention`
/// spells it. An array that needs attention with no recorded cause (a
/// journal an older helper wrote) admits only a Restore, exactly like
/// `Other`, and reads the same.
fn attention_code(result: &ElasticResult) -> &'static str {
    use tentanas_helper::elastic::ElasticAttention;
    match (&result.attention, result.stage) {
        (Some(ElasticAttention::ParityRun { kind, .. }), _) => match kind {
            ElasticSnapraidKind::Sync => "sync_failed",
            ElasticSnapraidKind::Scrub => "scrub_failed",
            ElasticSnapraidKind::Fix { .. } => "fix_failed",
        },
        (Some(ElasticAttention::AddDisk { .. }), _) => "add_disk",
        (Some(ElasticAttention::Other), _) | (None, ElasticStage::NeedsAttention) => "other",
        (None, _) => "",
    }
}

/// The wire's step code of a helper's add step.
fn add_step_code(step: tentanas_helper::elastic::ElasticAddStep) -> &'static str {
    use tentanas_helper::elastic::ElasticAddStep;
    match step {
        ElasticAddStep::Format => "format",
        ElasticAddStep::Mount => "mount",
        ElasticAddStep::Join => "join",
        ElasticAddStep::Sync => "sync",
    }
}

/// What the helper's own answer adds to the wire (K6): its recorded cause,
/// the gates it will enforce, and the state of an add it has not finished.
///
/// THE CORE OFFERS EXACTLY WHAT THE HELPER ACCEPTS. `parity_run_available`
/// is the core's own admission (`db::parity_admission`) AND the helper's
/// (`maintenance_admission`) over the very facts this answer reports — a
/// Scrub admitted means a Sync admitted with the acknowledgement, the
/// helper's gate being one rule for both. The acknowledgement is asked for
/// whenever the helper would refuse the Sync without it.
fn apply_helper_gates(wire: &mut NasElasticArray, result: &ElasticResult) {
    use tentanas_helper::elastic::{maintenance_admission, ElasticRefusal};
    let facts = result.admission();
    wire.attention = attention_code(result).to_string();
    wire.parity_run_available = wire.parity_run_available
        && maintenance_admission(&facts, &ElasticSnapraidKind::Scrub, None).is_ok();
    // The helper's own recorded fault is THE fault an acknowledgement names
    // (its gate compares exactly this id); the history's only when the
    // helper records none.
    if maintenance_admission(&facts, &ElasticSnapraidKind::Sync, None) == Err(ElasticRefusal::FaultUnacknowledged) {
        wire.sync_needs_acknowledgement = true;
        if let Some(tentanas_helper::elastic::ElasticAttention::ParityRun { operation_id, .. }) = &result.attention {
            wire.sync_fault_id = operation_id.clone();
        }
    }
    if let Some(pending) = wire.pending_add_disk.as_mut() {
        if let Some(add) = result.pending_add.as_ref().filter(|add| add.disk_id == pending.disk_id) {
            pending.step = add_step_code(add.step).to_string();
            pending.in_union = add.in_union;
            pending.undo_possible = add.undo_possible;
        }
    }
}

/// Parity errors over the COMPLETED sync and scrub runs that finished inside
/// `PARITY_ERRORS_WINDOW_DAYS` — the same window the wire advertises.
///
/// All three counters are summed. The helper parses the same three keys for
/// both kinds (`summary:error_file|error_io|error_data`) and already treats any
/// of them being non-zero as a failed run (`coupled_sync_success`), so counting
/// only one of them would hide errors the helper itself calls disqualifying.
///
/// `None` when no completed run falls inside the window: an array nothing has
/// checked lately has not been measured, and an absent measurement is unknown
/// here, never a confident zero. A run still going, or one cancelled or refused
/// before it read a block, is not evidence either way; nor is one that finished
/// without reporting any counter at all.
///
/// The runs come from `parity_window_runs`, which the store selects by DATE —
/// `snapraid_history` is the short display list and would let an error that is
/// still inside the window rotate out behind newer clean runs, turning this
/// into a green zero that nothing measured. If that date query ever comes back
/// AT `PARITY_ERRORS_MAX_ROWS` its oldest rows may have been cut off, and a
/// window that may be incomplete cannot prove a zero: the answer is `None`.
fn parity_errors_in_window(
    array: &ElasticArrayRow,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<u64> {
    if array.parity_window_runs.len() >= PARITY_ERRORS_MAX_ROWS as usize {
        return None;
    }
    let since = now - chrono::Duration::days(i64::from(PARITY_ERRORS_WINDOW_DAYS));
    let mut total: Option<u64> = None;
    for run in &array.parity_window_runs {
        // A REPAIR is excluded on purpose: the errors it reports are the ones
        // it just rebuilt from parity, so counting them would leave a
        // successfully repaired array reading damaged for the whole window.
        if !matches!(run.kind.as_str(), "sync" | "scrub") {
            continue;
        }
        if matches!(run.outcome.as_str(), "running" | "cancelled" | "refused") {
            continue;
        }
        let Some(finished) = run.finished_at.as_deref() else {
            continue;
        };
        let Ok(finished) = chrono::DateTime::parse_from_rfc3339(finished) else {
            continue;
        };
        if finished.with_timezone(&chrono::Utc) < since {
            continue;
        }
        // A partial Sync's file errors are the files that changed while it ran,
        // not damage: only its io and data counters say anything about parity.
        let file_errors = if run.outcome == "partial" { Some(0) } else { run.errors_file };
        let counted = [file_errors, run.errors_io, run.errors_data];
        if counted.iter().all(Option::is_none) {
            continue;
        }
        let found = counted.into_iter().flatten().fold(0u64, u64::saturating_add);
        total = Some(total.unwrap_or(0).saturating_add(found));
    }
    total
}

/// Why a repair cannot run right now, or `None` when the array is in a shape
/// `snapraid fix` can work on.
///
/// SEPARATE from `repair_evidence` because the two answer opposite questions:
/// this one is about whether the operation is POSSIBLE, that one about whether
/// it is WARRANTED. A repair writes back the blocks a scrub marked bad, and
/// `maintenance_guard` requires every data disk present and mounted to do it —
/// so a dead data disk, which is the very situation a repair is reached for, is
/// the one situation in which it cannot run at all. Disk replacement is
/// WITHDRAWN (round 4), so the honest answer is to say what has to happen first
/// rather than to offer a button whose only possible outcome is
/// `precondition_failed` from the helper's own guard.
pub fn repair_blocker(array: &NasElasticArray) -> Option<String> {
    // The helper's recorded cause first: a repair is admitted over a parity
    // fault, never over an unfinished add or a cause only a Restore
    // addresses (`maintenance_admission`), and a button it then refuses
    // reads to an admin as a fault.
    match array.attention.as_str() {
        "add_disk" => {
            return Some("macierz ma niedokończone dodawanie dysku: najpierw je dokończ albo wycofaj".into())
        }
        "other" => return Some("macierz wymaga przywrócenia: najpierw uruchom Przywróć".into()),
        _ => (),
    }
    for member in &array.data_disks {
        // Named as the screen names it — `sdg`, or the member's part in the
        // array when the inventory lost it — never by the slot key.
        let shown = member_label(&member.name, &member.disk_name);
        if member.device_present == Some(false) {
            return Some(format!(
                "dysk '{shown}' nie jest widoczny na tym nodzie: podłącz go ponownie, \
                 bo naprawa z parity zapisuje właśnie ten dysk"
            ));
        }
        if member.mounted == Some(false) {
            return Some(format!(
                "dysk '{shown}' nie jest zamontowany: odtwórz montowania macierzy, \
                 bo naprawa zapisałaby katalog brancha na systemie plików noda"
            ));
        }
    }
    None
}

/// The SCRUB that marked blocks for a repair to write back, if one still
/// stands: a `scrub` that ended without success AND counted errors, with no
/// successful repair after it.
///
/// It reads the SnapRAID HISTORY rather than `unresolved_operation`, and that
/// difference is the point: `unresolved_operation` is equally true for a mover
/// that stopped part-way and for an add-disk that failed, and neither is
/// something parity can repair.
///
/// WHY ONLY A SCRUB, and only one that counted errors: a repair writes back
/// the blocks snapraid has MARKED as bad in its content file, and a scrub (or a
/// check) is what marks them. MEASURED (rig11, snapraid 13.0-1): `-e fix` with
/// no scrub behind it reports `summary:error:0`, `exit:ok` and "Everything OK"
/// while writing nothing at all; a failed Sync marks nothing either. Offering
/// a repair on that evidence was offering an operation that could only do
/// nothing and then report success.
///
/// The history arrives newest first, so a successful repair seen BEFORE an
/// unsuccessful run is one that came after it — the same rule
/// `db::UNRESOLVED_ELASTIC_OPERATION` applies in SQL, over the same rows.
fn unresolved_parity_run(array: &NasElasticArray) -> Option<&NasSnapraidRun> {
    unresolved_parity_fault(&array.snapraid.history)
}

/// The same rule over the history alone, so the two callers that hold
/// different shapes of the same rows — the protocol array the UI and the
/// dispatch read, and the store row the scheduler holds — cannot drift apart.
///
/// The SCHEDULER needs it to keep an unattended Sync off an array whose scrub
/// reported errors nothing has repaired. MEASURED on rig11 (snapraid 13.0-1,
/// `scratchpad/f1probe/run3.sh`, cases f1-f3): a Sync does NOT cost the marked
/// blocks their repairability — it sees an unchanged file as `equal`, writes no
/// parity for it, and `-e fix` still recovers the block afterwards
/// (`error_recovered:1`). An unchanged file the scrub could not READ (EIO)
/// survives it too: the Sync ends `equal`, the content file still lists it,
/// and `fix -f` restores it afterwards (M3 f4, raw logs in the design
/// repository's reviews/artifacts/elastic-measurements-2026-09-24). An earlier
/// probe that reported such files "removed from the content file" had DELETED
/// the file — ordinary Sync behaviour, not a cost of the fault. What a Sync
/// DOES finalise is the state of every file deleted or changed since the
/// previous Sync, a damaged one included: its earlier version can no longer be
/// restored from parity, and cases stay unmeasured (an unreadable file that
/// also changed, a disk degrading during the Sync). That is why an unattended
/// Sync is refused here while a manual one is not — an admin who is told can
/// decide, the cadence cannot.
///
/// WHAT ENDS IT (M2 of the release review): the cure, never the passing of
/// time. A successful repair (`fix` ok), or a clean full scrub (`scrub` ok) —
/// positive evidence that nothing is marked any more. And for a scrub that
/// counted FILE errors only (no data errors), a later successful Sync — the
/// same rule `db::UNRESOLVED_ELASTIC_OPERATION` applies to the operation row.
/// Such an error marks no block, so `-e fix` has nothing to recover; the
/// Sync over it keeps the file in content (M3 f4), and a later `fix -f`
/// still restores it, so ending the fault there hides no lost file. A Sync
/// does NOT end data errors: their marks stay repairable across it
/// (measured, f1-f3), which is what keeps the repair on offer.
pub fn unresolved_parity_fault(history: &[NasSnapraidRun]) -> Option<&NasSnapraidRun> {
    let mut synced_since = false;
    for run in history {
        match (run.kind.as_str(), run.outcome.as_str()) {
            ("fix" | "scrub", "ok") => return None,
            ("sync", "ok") => synced_since = true,
            ("scrub", "failed" | "needs_attention") if run.errors.is_some_and(|errors| errors > 0) => {
                if synced_since && run.errors_data == Some(0) {
                    return None;
                }
                return Some(run);
            }
            _ => (),
        }
    }
    None
}

/// Why the CADENCE must not start a Sync on this array, or `None` when it may,
/// as the code the schedule row records (`pominięto: reason:<code>`) and the
/// screen words (`schedules.skip_<code>`), so an admin reading the schedule
/// sees why a slot did nothing instead of finding a silent gap:
/// - `elastic_scrub_errors_unrepaired` — a scrub counted errors nothing has
///   repaired (`unresolved_parity_fault`);
/// - `elastic_parity_fault_unacknowledged` — a scrub that failed without
///   counting anything (its log broke), or a repair that failed. The helper
///   records both as the array's cause and refuses every Sync that does not
///   acknowledge it (minor 8 of the release review): starting one per slot
///   only added a refused run to the history each time.
/// Either ends as the fault does — a successful repair or a clean scrub —
/// and the uncounted one also with a successful (confirmed) Sync, after
/// which the helper holds no cause.
pub fn scheduled_sync_blocker(history: &[NasSnapraidRun]) -> Option<&'static str> {
    if unresolved_parity_fault(history).is_some() {
        return Some("elastic_scrub_errors_unrepaired");
    }
    for run in history {
        match (run.kind.as_str(), run.outcome.as_str()) {
            ("fix" | "scrub", "ok") | ("sync", "ok" | "partial") => return None,
            ("scrub" | "fix", "failed" | "needs_attention") => return Some("elastic_parity_fault_unacknowledged"),
            _ => (),
        }
    }
    None
}

/// The alert key for a cadence slot skipped because parity errors await a
/// repair. Its own key: the settle and cache alerts say something else, and an
/// admin has to be able to clear this one by repairing.
pub fn repair_alert_key(array: &str) -> String {
    format!("elastic:{array}:repair")
}

/// What a repair would be working on, or `None` when the array reports nothing
/// a repair could recover.
///
/// A repair WRITES blocks back from the parity checkpoint. The helper runs only
/// the two filtered forms measured not to revert users' data
/// (`tentanas_helper::elastic::FixScope`): the blocks a Scrub marked bad on a
/// disk in use, or the missing files of a replaced, empty disk. On an array
/// that reports nothing wrong there is nothing for either to do. The bar here is therefore evidence, not permission —
/// and the evidence has to be about PARITY, because parity is what the repair
/// recovers from. An array with none of it has nothing a repair could recover,
/// and the admin's next step is a scrub, which is what the refusal says.
///
/// It takes the OBSERVED array rather than the database row because the
/// frontend's own gate reads exactly these fields off the wire: one rule over
/// one object is what keeps the button and the handler from disagreeing, and a
/// button the node then refuses reads to an admin as a fault.
pub fn repair_evidence(array: &NasElasticArray) -> Option<String> {
    let run = unresolved_parity_run(array)?;
    Some(format!(
        "scrub macierzy zaznaczył {} błędów i nic ich nie naprawiło",
        run.errors.unwrap_or_default()
    ))
}

/// The one status of the array card, with its reason.
fn health_of(
    protection: &NasElasticProtection,
    data: &[NasElasticBranch],
    cache: &[NasElasticBranch],
    parity: &[NasElasticParity],
    state: &str,
    conflicts: usize,
) -> (&'static str, String) {
    if state == "unknown" {
        return ("unknown", "this node could not measure this array".to_string());
    }
    // A DISK THAT IS GONE is critical. A disk that is merely not mounted yet
    // is not: after every reboot every branch of every array is unmounted,
    // and painting that red would make the normal state of a healthy node
    // look like a failure — the same conflation that deadlocked the verdict.
    let gone: Vec<&str> = data
        .iter()
        .chain(cache.iter())
        .filter(|b| b.device_present == Some(false))
        .map(|b| b.name.as_str())
        .collect();
    if !gone.is_empty() {
        return ("critical", format!("{} is not on this node", gone.join(", ")));
    }
    let parity_gone: Vec<&str> = parity
        .iter()
        .filter(|p| p.device_present == Some(false))
        .map(|p| p.name.as_str())
        .collect();
    if !parity_gone.is_empty() {
        return (
            "warning",
            format!(
                "parity disk {} is not on this node, so nothing can be synced",
                parity_gone.join(", ")
            ),
        );
    }
    let cold: Vec<&str> = data
        .iter()
        .chain(cache.iter())
        .filter(|b| b.mounted == Some(false))
        .map(|b| b.name.as_str())
        .collect();
    if !cold.is_empty() {
        return (
            "warning",
            format!(
                "{} is present and not mounted yet — the array is not serving until the \
                 next reconcile",
                cold.join(", ")
            ),
        );
    }
    // An array serving files a stuck mover record left in two versions is not
    // healthy, whatever its parity says: one version is out of every share's
    // sight and may be the only copy of a client's write. It is said before
    // the parity state, which has its own panel.
    if conflicts > 0 {
        return (
            "warning",
            format!("{conflicts} plików zostało w dwóch wersjach po przerwanym przenoszeniu; szczegóły w alercie"),
        );
    }
    if protection.status == "unprotected" {
        return ("warning", protection.detail.clone());
    }
    if protection.status == "unknown" {
        return ("unknown", protection.detail.clone());
    }
    ("ok", String::new())
}

pub fn validate_observation(spec: &ElasticCreateSpec, result: &ElasticResult) -> Result<()> {
    ensure!(
        result.array_id == spec.array_id
            && result.operation_id == spec.operation_id
            && result.owner == spec.owner,
        "Odpowiedź helpera dotyczy innej intencji lub właściciela"
    );
    ensure!(
        result.disks.len() == spec.data.len() + usize::from(spec.cache.is_some()) + spec.parity.len(),
        "Niepełny zestaw obserwacji dysków"
    );
    if let Some(service) = &result.service {
        tentanas_helper::elastic::validate_elastic_uuid(&service.operation_id)?;
        ensure!(
            service.operation_id != spec.operation_id,
            "Operacja service nie może zastępować Create"
        );
    }
    // The slot of an add the helper reports as unfinished is the journal's
    // LAST data slot, and until its filesystem is confirmed nothing on that
    // disk is the array's: a blank disk, or one an interrupted mkfs left, is
    // not an identity mismatch of a member.
    if let Some(add) = &result.pending_add {
        ensure!(
            spec.data.last().is_some_and(|last| last.disk_id == add.disk_id)
                && add.branch == tentanas_helper::elastic::data_branch_name(spec.data.len()),
            "Niedokończone dodanie nie wskazuje ostatniego dysku danych"
        );
    }
    let unconfirmed_slot = result
        .pending_add
        .as_ref()
        .filter(|add| !add.formatted)
        .and_then(|_| u16::try_from(spec.data.len()).ok())
        .map(ElasticRole::Data);
    let mut seen = BTreeSet::new();
    for disk in &result.disks {
        let (role, index, expected) = match disk.role {
            ElasticRole::Data(i) => (
                "data",
                usize::from(i),
                spec.data.get(usize::from(i).wrapping_sub(1)),
            ),
            ElasticRole::Parity(i) => (
                "parity",
                usize::from(i),
                spec.parity.get(usize::from(i).wrapping_sub(1)),
            ),
            ElasticRole::Cache => ("cache", 1, spec.cache.as_ref()),
        };
        let expected = expected.ok_or_else(|| anyhow!("Obca rola w odpowiedzi helpera"))?;
        ensure!(
            seen.insert((role, index)),
            "Powtórzona rola w odpowiedzi helpera"
        );
        if Some(disk.role) == unconfirmed_slot {
            ensure!(disk.mounted != Some(true), "Zamontowany dysk bez potwierdzonego formatowania");
            continue;
        }
        ensure!(
            disk.observed_uuid
                .as_deref()
                .is_none_or(|value| value == expected.expected_uuid.as_str())
                && disk
                    .filesystem
                    .as_deref()
                    .is_none_or(|value| value == spec.filesystem.as_str()),
            "Obca tożsamość filesystemu w obserwacji"
        );
        if disk.mounted == Some(true)
            || (disk.device_present == Some(true) && disk.mounted == Some(false))
        {
            ensure!(
                disk.device_present == Some(true)
                    && disk.observed_uuid.as_deref() == Some(expected.expected_uuid.as_str())
                    && disk.filesystem.as_deref() == Some(spec.filesystem.as_str()),
                "Mount bez potwierdzonej tożsamości filesystemu"
            );
        }
        if let Some(size) = disk.size_bytes {
            ensure!(
                disk.used_bytes.is_none_or(|n| n <= size)
                    && disk.free_bytes.is_none_or(|n| n <= size),
                "Sprzeczne statystyki filesystemu"
            );
        }
    }
    if let Some(at) = &result.sync_completed_at {
        chrono::DateTime::parse_from_rfc3339(at)?;
        ensure!(!spec.parity.is_empty(), "Sync bez parity");
    }
    if let Some(run) = &result.last_mover {
        tentanas_helper::elastic::validate_elastic_uuid(&run.operation_id)?;
        tentanas_helper::elastic::validate_elastic_uuid(&run.resume_operation_id)?;
        ensure!(run.operation_id != spec.operation_id, "Mover nie może zastępować Create");
        ensure!(run.resume_operation_id != run.operation_id && run.resume_operation_id != spec.operation_id,
            "Resume movera musi mieć osobną operację");
        let mover_started = chrono::DateTime::parse_from_rfc3339(&run.started_at)?;
        let mover_finished = run.finished_at.as_ref().map(|at| chrono::DateTime::parse_from_rfc3339(at)).transpose()?;
        ensure!(mover_finished.is_none_or(|finished| finished >= mover_started),
            "Mover ma odwrócony czas");
        // A run ends at Complete, or as NeedsAttention once the next run (or
        // the Resume of an older helper's Hold) closed it.
        ensure!(matches!(run.phase, ElasticMoverPhase::Complete | ElasticMoverPhase::NeedsAttention)
            || run.finished_at.is_none(),
            "Nieukończony mover nie może mieć czasu końca");
        if run.phase == ElasticMoverPhase::Complete {
            ensure!(run.finished_at.is_some(), "Ukończony mover bez czasu końca");
        }
        let reported = |kind| run.issues.iter().filter(|issue| issue.kind == kind).count() as u64;
        ensure!(reported(ElasticMoverIssueKind::Skipped) <= run.skipped_files
            && reported(ElasticMoverIssueKind::Refused) <= run.refused_files,
            "Mover raportuje więcej plików niż jego liczniki");
        if let Some(sync) = &run.coupled_sync {
            ensure!(sync.kind == ElasticSnapraidKind::Sync, "Mover ma nieprawidłowy typ sync");
            ensure!(sync.operation_id == run.operation_id, "Sync movera należy do innej operacji");
            tentanas_helper::elastic::validate_elastic_uuid(&sync.operation_id)?;
            let sync_started = chrono::DateTime::parse_from_rfc3339(&sync.started_at)?;
            let sync_finished = sync.finished_at.as_ref().map(|at| chrono::DateTime::parse_from_rfc3339(at)).transpose()?;
            ensure!(sync_started >= mover_started, "Sync movera rozpoczyna się przed moverem");
            ensure!(sync_finished.is_none_or(|finished| finished >= sync_started), "Sync ma odwrócony czas");
            ensure!(mover_finished.is_none_or(|finished| sync_finished.is_some_and(|sync_end| sync_end <= finished)),
                "Sync kończy się poza moverem");
            if run.phase == ElasticMoverPhase::Complete {
                // A Sync that met files changing under it completes a run too:
                // parity stays marked out of date and the next Sync covers them.
                let clean = sync.outcome == ElasticSnapraidOutcome::Succeeded
                    && sync.exit_code == Some(0)
                    && sync.errors_file.is_none_or(|n| n == 0)
                    && sync.errors_io.is_none_or(|n| n == 0)
                    && sync.errors_data.is_none_or(|n| n == 0);
                let changed_files = sync.outcome == ElasticSnapraidOutcome::Partial
                    && partial_sync_counters(sync)
                    && result.parity_stale;
                ensure!(sync_finished.is_some() && (clean || changed_files),
                    "Mover ukończony bez udanego sync");
            }
        }
    }
    ensure!(result.parity_stale == result.stale_parity_bytes.is_some(),
        "Niespójny znacznik nieaktualnej parity");
    ensure!(!result.restart_required || result.stage != ElasticStage::Ready,
        "Macierz czekająca na restart nie jest gotowa");
    for record in result.stuck_records.iter()
        .chain(result.last_mover.iter().flat_map(|run| run.stuck_records.iter()))
    {
        tentanas_helper::elastic::validate_elastic_uuid(&record.operation_id)?;
    }
    if result.parity_stale {
        ensure!(!spec.parity.is_empty(), "Nieaktualna parity macierzy bez parity");
    }
    if result.stage == ElasticStage::Ready {
        if let Some(service) = &result.service {
            ensure!(
                service.mode == ElasticServiceMode::Online
                    && !service.pending
                    && result.union_readonly == Some(false),
                "Ready bez potwierdzonego trybu service RW"
            );
        }
    }
    Ok(())
}

/// The counters snapraid reports for a Sync that skipped only files changing
/// under it: exit 1, file errors, and no io or data error.
fn partial_sync_counters(run: &tentanas_helper::elastic::ElasticSnapraidRun) -> bool {
    run.exit_code == Some(1)
        && run.errors_file.is_some_and(|n| n > 0)
        && run.errors_io == Some(0)
        && run.errors_data == Some(0)
}

pub fn validate_result(spec: &ElasticCreateSpec, result: &ElasticResult) -> Result<()> {
    validate_observation(spec, result)?;
    if result.stage == ElasticStage::Ready {
        ensure!(
            result
                .disks
                .iter()
                .all(|disk| disk.device_present == Some(true) && disk.mounted == Some(true)),
            "Ready bez potwierdzenia wszystkich filesystemów"
        );
        ensure!(
            result.union_mounted == Some(true)
                && (spec.parity.is_empty() || result.sync_completed_at.is_some()),
            "Ready bez unii lub potwierdzonego sync"
        );
    }
    Ok(())
}

pub async fn claims(db: &DbPool, name: Option<&str>, explicit: Option<&ElevationToken>) -> Result<ElasticClaimsResult> {
    let (out, _) = super::broker::run_privileged(db,
        &HelperCommand::ElasticClaims { name: name.map(str::to_string) }, explicit,
        Duration::from_secs(30)).await?;
    // The helper's refusal names what stopped it — an unreadable journal, by
    // its array id — and that sentence is the only way the admin learns which
    // file to look at.
    ensure!(
        out.success() && out.stdout.len() < 64 * 1024,
        "Nie można odczytać rezerwacji roota (kod {}): {}",
        out.code,
        helper_detail(&out.stderr)
    );
    let result: ElasticClaimsResult = serde_json::from_str(&out.stdout)?;
    ensure!(name.is_none() || (result.name_claimed.is_some() && result.namespace_clear.is_some()),
        "Nie potwierdzono dostępności przestrzeni nazw");
    Ok(result)
}

pub fn claimed_disk_ids(disks: &[NasDisk], claims: &[tentanas_helper::elastic::ElasticClaim]) -> BTreeSet<String> {
    disks.iter().filter(|disk| claims.iter().any(|claim| claim.disk_id == disk.disk_id
        || claim.wwn.as_ref().is_some_and(|w| !w.is_empty() && disk.wwn.as_ref() == Some(w))
        || claim.serial.as_ref().is_some_and(|s| !s.is_empty() && *s == disk.serial)))
        .map(|disk| disk.disk_id.clone()).collect()
}

// =============================================================================
// Import: adopting an array this node holds but has no database record of
// =============================================================================
//
// WHY this exists, measured and not hypothetical: re-provisioning the addon
// gives it a new `org_id`/`addon_id`. The arrays created under the previous
// identity keep their disks, keep their journals and keep serving their
// unions, while the new database starts empty — and every read of an Elastic
// Array is scoped by owner, so nothing in the product can reach them. Their
// member disks then show in the Disks tab as "occupied" by nothing, and the
// only action ever offered for such a disk was to erase it.
//
// THE IDENTITY QUESTION, and it is the whole feature. A disk_id is a path, a
// WWN and a serial say "the same hardware came back" — none of the three says
// the filesystem on it is still the array's. The journal records the
// filesystem UUID it formatted each branch with (`expected_uuid`), so that
// UUID against the live one is the only thing that may authorise an adoption.
// The hardware identities are used for ONE thing here: naming a disk in a
// refusal.
//
// AND THE OWNER LIVES IN TWO PLACES. The database row is one; the journal is
// the other, and every privileged command refuses an array whose journal owner
// is not the caller's ("macierz niedostępna dla właściciela"). An adoption
// that wrote only the database would produce an array that lists, and then
// fails every Inspect, Restore, Sync and mover on it. So `import_apply`
// re-owns the journal through the helper FIRST and writes the row second.

/// Every member of a journal spec with its place in the array — the slot the
/// array's own rows give it (`d1`, `c1`, `parity1`), its role and, for
/// parity, its level — plus the serial the journal recorded. The kernel name
/// is left empty: only the scan knows whether the disk is here at all.
///
/// The place travels as DATA (`NasElasticImportMember`), never as a sentence:
/// the import dialog words it with the same helper the Elastic detail screen
/// names its members with, in the admin's own language.
fn member_names(spec: &ElasticCreateSpec) -> Vec<(NasElasticImportMember, &ElasticDiskSpec)> {
    let place = |slot: String, role: &str, index: Option<u8>, disk: &ElasticDiskSpec| NasElasticImportMember {
        name: slot,
        role: role.to_string(),
        index,
        disk_name: String::new(),
        serial: disk.serial.clone().unwrap_or_default(),
    };
    spec.data
        .iter()
        .enumerate()
        .map(|(i, disk)| (place(format!("d{}", i + 1), "data", None, disk), disk))
        .chain(spec.cache.iter().map(|disk| (place("c1".to_string(), "cache", None, disk), disk)))
        .chain(spec.parity.iter().enumerate().map(|(i, disk)| {
            let level = u8::try_from(i + 1).unwrap_or(u8::MAX);
            (place(format!("parity{}", i + 1), "parity", Some(level), disk), disk)
        }))
        .collect()
}

/// A member as a node-side refusal names it: its kernel name when this node
/// has the disk, otherwise its part in the array ("dysk danych 2").
///
/// Only `repair_blocker` still needs it, for a refusal the node itself
/// returns (the server-text class tracked in the backlog). Anything the
/// SCREEN shows is worded on the front end from structured data — the detail
/// screen's own `repairBlocker` and the import dialog both use
/// `memberName` in `elastic-detail.js` — so this is not a second source of
/// on-screen member names.
///
/// Never the `disk_id` (`wwn-…`, `sn-…`) and never the bare slot: both are
/// keys, and the owner's rule is that a disk is named the way the admin sees
/// it on the Disks tab.
pub(crate) fn member_label(slot: &str, disk_name: &str) -> String {
    if !disk_name.is_empty() {
        return disk_name.to_string();
    }
    if let Some(n) = slot.strip_prefix("parity") {
        return format!("dysk parity {n}");
    }
    if slot.starts_with('c') {
        return "dysk cache".to_string();
    }
    if let Some(n) = slot.strip_prefix('d') {
        return format!("dysk danych {n}");
    }
    slot.to_string()
}

/// Whose journal this is, as a CODE the front end turns into a sentence
/// (`tentanas.owner_kind.*`). The node never sends the sentence itself: it
/// would be Polish in every locale.
///
/// The journal is owned by the instance asking.
pub const OWNER_KIND_THIS_INSTANCE: &str = "this_instance";
/// By another TentaNas instance of the SAME organisation as the one asking —
/// the ordinary case after a re-provisioning inside one organisation. Only in
/// this case may the owner's instance be named (`owner_instance_name`): the
/// admin asking is a member of that organisation.
pub const OWNER_KIND_THIS_ORG: &str = "this_org";
/// By an organisation this node has NO record of — in practice disks carried
/// over from another machine, whose journal names that machine's tenant.
/// Such a journal stays visible and adoptable (with the re-own warning):
/// whoever holds the disks in this node holds them, and no tenant HERE is
/// being overruled. Nothing about that owner is named — not the organisation,
/// not the instance — because this node cannot tell whose it is, and an id is
/// not a name anyway.
pub const OWNER_KIND_OTHER_INSTALLATION: &str = "other_installation";
/// By ANOTHER ORGANISATION THAT EXISTS ON THIS NODE. The owner's rule
/// (2026-09-22): such a journal is invisible to the asking tenant. It is not
/// offered by the import scan, the adoption refuses it as it refuses a journal
/// that does not exist, and the wipe of one of its disks is refused without
/// naming the array — if a tenant may not adopt another tenant's array, it
/// may not destroy it either (OWASP A01). The code itself reaches the client
/// only as the wipe refusal `journal_other_org`; its array name, role and ids
/// never do.
pub const OWNER_KIND_OTHER_ORG_ON_NODE: &str = "other_org_on_node";

/// The owner kind of a journal, judged against the instance that asks and
/// the organisations this node has a record of (`orgs_on_node`, every row of
/// the platform's `organizations` table whatever its status — see
/// `orgs_on_node` in the dispatch module for why a deleted one still counts).
///
/// The organisation decides first: a journal of another organisation is never
/// "this instance" even when the addon id happens to be this one's (the
/// TentaNas package is one instance per node, shared by every organisation on
/// it, so an equal addon id says nothing about the tenant). Only then does it
/// matter whether that other organisation lives here — a tenant of this node
/// (hidden) or somebody this node has never heard of (disks moved in).
pub fn owner_kind(owner: &ElasticOwner, asking: &ElasticOwner, orgs_on_node: &BTreeSet<String>) -> &'static str {
    if owner.org_id == asking.org_id {
        if owner.addon_id == asking.addon_id { OWNER_KIND_THIS_INSTANCE } else { OWNER_KIND_THIS_ORG }
    } else if orgs_on_node.contains(&owner.org_id) {
        OWNER_KIND_OTHER_ORG_ON_NODE
    } else {
        OWNER_KIND_OTHER_INSTALLATION
    }
}

/// The owner ids a client may see for a journal of this `kind`: the owner's
/// own ids when it belongs to the asking organisation, and NOTHING for
/// another installation.
///
/// Another tenant's org and addon ids are opaque, but they are still that
/// tenant's identifiers, and a node shared by several tenants hands none of
/// them to the admin of another (OWASP A01) — not in a sentence and not in a
/// tooltip. Nothing on the client needs them: the adoption request names the
/// array and the retyped name only, and the node reads the previous owner
/// from the journal itself (`import_apply`).
pub(crate) fn owner_ids_for(kind: &str, owner: &ElasticOwner) -> (String, String) {
    if kind == OWNER_KIND_THIS_INSTANCE || kind == OWNER_KIND_THIS_ORG {
        (owner.org_id.clone(), owner.addon_id.clone())
    } else {
        (String::new(), String::new())
    }
}

/// Whether one inventory disk is the same PIECE OF HARDWARE the journal
/// recorded. Deliberately not evidence of anything else: it is what separates
/// "this disk is gone" from "this disk is here and no longer carries the
/// array's filesystem", and the remedies differ.
fn same_hardware(disk: &NasDisk, member: &ElasticDiskSpec) -> bool {
    member.disk_id == disk.disk_id
        || member.wwn.as_ref().is_some_and(|w| !w.is_empty() && disk.wwn.as_ref() == Some(w))
        || member.serial.as_ref().is_some_and(|s| !s.is_empty() && *s == disk.serial)
}

/// What an import scan answers: one candidate per journal on this node.
///
/// `known` is `(array_id, name)` of every array this node already has a row
/// for, whatever the owner — an array with a row is not offered a second time,
/// and neither is one whose NAME a different array already holds, because that
/// name is UNIQUE in the database and the adoption could not be written.
///
/// A journal of another organisation that lives on this node is NOT a
/// candidate at all (`OWNER_KIND_OTHER_ORG_ON_NODE`): it is dropped here,
/// before anything of it — name, members, status — becomes part of the
/// answer, and because `import_selection` only ever picks from this list, the
/// adoption refuses it with the same sentence as a journal that is not there.
pub fn import_candidates(
    entries: &[ElasticJournalEntry],
    unreadable: &[ElasticUnreadableJournal],
    disks: &[NasDisk],
    known: &[(String, String)],
    owner: &ElasticOwner,
    orgs_on_node: &BTreeSet<String>,
) -> Vec<NasElasticImportCandidate> {
    // A journal the node cannot load is listed, never hidden (A7 of the second
    // review): with its spec when that much could be read, by its file name
    // otherwise, and always with the node's reason and nothing to adopt.
    let reason = |array_id: &str| unreadable.iter().find(|journal| journal.array_id == array_id).map(|journal| journal.reason.clone());
    let unlisted = unreadable
        .iter()
        .filter(|journal| !entries.iter().any(|entry| entry.spec.array_id == journal.array_id))
        .map(|journal| NasElasticImportCandidate {
            array_id: journal.array_id.clone(),
            // Nothing could be read, so there is no NAME — and the file name
            // is the array's UUID, which is not one. The dialog names the row
            // by its state and keeps the id in a tooltip.
            name: String::new(),
            status: "unreadable".to_string(),
            detail: journal.reason.clone(),
            ..Default::default()
        });
    entries
        .iter()
        .filter(|entry| owner_kind(&entry.spec.owner, owner, orgs_on_node) != OWNER_KIND_OTHER_ORG_ON_NODE)
        .map(|entry| {
            let spec = &entry.spec;
            let members = member_names(spec);
            let total = members.len();
            let mut matched = 0u32;
            let mut disks_missing = Vec::new();
            let mut disks_reused = Vec::new();
            for (place, member) in members {
                if disks.iter().any(|disk| {
                    disk.fs_uuid
                        .as_deref()
                        .is_some_and(|uuid| uuid.eq_ignore_ascii_case(&member.expected_uuid))
                }) {
                    matched += 1;
                } else if let Some(present) = disks.iter().find(|disk| same_hardware(disk, member)) {
                    // The hardware is here and the array's filesystem is not:
                    // either somebody reused the disk or it was wiped. Both
                    // mean adopting it would claim bytes that are no longer
                    // the array's. The disk is HERE, so it has the name the
                    // Disks tab shows for it.
                    disks_reused.push(NasElasticImportMember { disk_name: present.name.clone(), ..place });
                } else {
                    // Gone, so no kernel name: the dialog says its part in the
                    // array and the serial printed on the drive — the one
                    // thing that finds it in a shelf.
                    disks_missing.push(place);
                }
            }
            let owned_elsewhere = spec.owner != *owner;
            let (status, detail) = match known
                .iter()
                .find(|(id, name)| *id == spec.array_id || *name == spec.name)
            {
                Some((id, _)) if *id == spec.array_id => (
                    "already_known",
                    "this node already has a database record of this array".to_string(),
                ),
                // The array holding the name may be ANOTHER organisation's
                // (`known` is every owner's, because the name column is
                // unique per node), so the sentence names neither array: the
                // row already shows the journal's own name, and repeating it
                // here as "a different array named …" told this tenant what
                // another tenant had called its array.
                Some(_) => (
                    "already_known",
                    "another array on this node already uses this name, and an array name \
                     is unique per node"
                        .to_string(),
                ),
                None if !disks_missing.is_empty() || !disks_reused.is_empty() => (
                    "incomplete",
                    format!(
                        "{matched} of {total} members still carry the filesystem UUID the \
                         journal recorded: {} cannot be found and {} no longer carry it, so \
                         adopting this array would claim storage that is not its own",
                        disks_missing.len(),
                        disks_reused.len()
                    ),
                ),
                None => (
                    "importable",
                    // The owner is NOT named here: the ids are not a name,
                    // and the dialog composes the owner from `owner_kind` above this.
                    if owned_elsewhere {
                        format!(
                            "all {total} members still carry the filesystem UUID the journal \
                             recorded; adopting re-owns the array from another TentaNas \
                             installation to this instance"
                        )
                    } else {
                        format!(
                            "all {total} members still carry the filesystem UUID the journal \
                             recorded, and the journal already names this instance as the owner"
                        )
                    },
                ),
            };
            let kind = owner_kind(&spec.owner, owner, orgs_on_node);
            let (owner_org_id, owner_addon_id) = owner_ids_for(kind, &spec.owner);
            NasElasticImportCandidate {
                array_id: spec.array_id.clone(),
                name: spec.name.clone(),
                filesystem: spec.filesystem.as_str().to_string(),
                owner_org_id,
                owner_addon_id,
                owner_foreign: owned_elsewhere,
                owner_kind: kind.to_string(),
                // The instance's display name lives in the platform database,
                // which this pure function does not read: the dispatch
                // handler adds it, and only for an owner of the asking org.
                owner_instance_name: String::new(),
                data_disks: spec.data.len() as u32,
                parity_disks: spec.parity.len() as u32,
                cache_disks: spec.cache.iter().count() as u32,
                disks_matched: matched,
                disks_missing,
                disks_reused,
                // Unknown is not "not mounted": the helper answers `None` when
                // it could not read the mount table at all.
                union_mounted: entry.union_mounted.unwrap_or(false),
                status: if reason(&spec.array_id).is_some() { "unreadable" } else { status }.to_string(),
                detail: reason(&spec.array_id).unwrap_or(detail),
            }
        })
        .chain(unlisted)
        .collect()
}

/// The apply-time gate: the candidate the request names, out of a FRESHLY
/// measured scan, and only when it may still be adopted.
///
/// Nothing here trusts the scan the dialog was drawn from. Between that scan
/// and this call a member can be pulled, wiped, or claimed by another array,
/// and each of those turns an `importable` candidate into a refusal.
pub fn import_selection<'a>(
    candidates: &'a [NasElasticImportCandidate],
    array_id: &str,
    confirm_name: &str,
) -> Result<&'a NasElasticImportCandidate> {
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.array_id == array_id)
        .ok_or_else(|| anyhow!("Node nie widzi już dziennika tej macierzy; powtórz skanowanie"))?;
    // The same sentence every other retype-to-confirm path in this module
    // answers with, so one typo reads the same wherever it happens.
    ensure!(
        candidate.name == confirm_name,
        "the typed confirmation does not match the name"
    );
    match candidate.status.as_str() {
        "importable" => Ok(candidate),
        "already_known" => Err(anyhow!("Ta macierz jest już zapisana w tej instancji")),
        // Counts, and the one name that is a name: a REUSED member is here
        // under a kernel name. A missing member's part in the array is
        // words, and the dialog already lists it in the admin's language
        // from the scan's structured members.
        _ => Err(anyhow!(
            "Macierz niekompletna: {} z {} dysków potwierdziło UUID z dziennika (brakuje: {}; użyte ponownie: {})",
            candidate.disks_matched,
            candidate.disks_matched as usize + candidate.disks_missing.len() + candidate.disks_reused.len(),
            candidate.disks_missing.len(),
            if candidate.disks_reused.is_empty() {
                "—".to_string()
            } else {
                candidate.disks_reused.iter().map(|m| m.disk_name.as_str()).collect::<Vec<_>>().join(", ")
            },
        )),
    }
}

/// The helper's refusal line, bounded. The broker already caps stderr, and a
/// refusal is one sentence; this keeps a runaway one out of a dialog.
fn helper_detail(stderr: &str) -> String {
    let text: String = stderr.trim().lines().next().unwrap_or_default().chars().take(200).collect();
    if text.is_empty() { "brak opisu z helpera".to_string() } else { text }
}

/// The journals of every owner on this node. Privileged even though it reads
/// nothing but files: `/var/lib/tentanas` is `0700 root` and the service runs
/// unprivileged, so there is no other way to learn that a lost array exists.
pub async fn journals(
    db: &DbPool,
    explicit: Option<&ElevationToken>,
) -> Result<ElasticJournalsResult> {
    let (out, _) = super::broker::run_privileged(
        db,
        &HelperCommand::ElasticJournals {},
        explicit,
        Duration::from_secs(60),
    )
    .await?;
    // The helper's own one-line refusal is the only thing that can say WHY a
    // journal could not be read (a corrupt file, a foreign mode), so it
    // travels with the error instead of being replaced by it.
    ensure!(
        out.success() && out.stdout.len() < 512 * 1024,
        "Nie można odczytać dzienników macierzy (kod {}): {}",
        out.code,
        helper_detail(&out.stderr)
    );
    let result: ElasticJournalsResult = serde_json::from_str(&out.stdout)?;
    for entry in &result.arrays {
        entry.spec.validate()?;
    }
    Ok(result)
}

/// One scan, as the dialog reads it: the journals, this node's live disk
/// signatures and the arrays it already knows, turned into candidates.
/// `orgs_on_node` is what hides another tenant's journals (`owner_kind`).
pub async fn import_scan(
    db: &DbPool,
    owner: &ElasticOwner,
    orgs_on_node: &BTreeSet<String>,
    explicit: Option<&ElevationToken>,
) -> Result<Vec<NasElasticImportCandidate>> {
    Ok(scan_journals(db, owner, orgs_on_node, explicit).await?.1)
}

/// The journals and the candidates made of them, from ONE read. The apply
/// path needs both: the candidate for its gate, and the journal for the
/// previous owner's ids — which go to the server log only, and which a
/// candidate of another installation deliberately does not carry
/// (`owner_ids_for`).
async fn scan_journals(
    db: &DbPool,
    owner: &ElasticOwner,
    orgs_on_node: &BTreeSet<String>,
    explicit: Option<&ElevationToken>,
) -> Result<(ElasticJournalsResult, Vec<NasElasticImportCandidate>)> {
    let journals = journals(db, explicit).await?;
    super::disks::refresh_inventory(db).await?;
    let known = store::elastic_array_identities(db)?;
    let candidates = import_candidates(
        &journals.arrays,
        &journals.unreadable,
        &super::disks::snapshot().0,
        &known,
        owner,
        orgs_on_node,
    );
    Ok((journals, candidates))
}

/// Adopts one scanned array. Verifies again on its own measurement, re-owns
/// the journal, then writes the row — in that order, because a row whose
/// journal still names the previous owner describes an array this instance
/// cannot operate.
///
/// Another tenant's journal cannot get past `import_selection`: the scan it
/// picks from never lists one (`import_candidates`), so the refusal is the
/// same "no such journal" a made-up `array_id` gets and confirms nothing.
pub async fn import_apply(
    db: &DbPool,
    owner: &ElasticOwner,
    orgs_on_node: &BTreeSet<String>,
    array_id: &str,
    confirm_name: &str,
    started_by: &str,
    explicit: Option<&ElevationToken>,
) -> Result<String> {
    let (journals, candidates) = scan_journals(db, owner, orgs_on_node, explicit).await?;
    let candidate = import_selection(&candidates, array_id, confirm_name)?;
    // The previous owner, for the SERVER LOG only, from the journal itself:
    // the candidate blanks another installation's ids (`owner_ids_for`), and
    // a candidate that passed the gate always has its journal in this read.
    // Nothing the admin reads carries these ids — not the array's state, not
    // the job log (`store::elastic_import`).
    let previous = journals
        .arrays
        .iter()
        .find(|entry| entry.spec.array_id == array_id)
        .map(|entry| entry.spec.owner.clone())
        .ok_or_else(|| anyhow!("Node nie widzi już dziennika tej macierzy; powtórz skanowanie"))?;
    let name = candidate.name.clone();
    let (out, _) = super::broker::run_privileged(
        db,
        &HelperCommand::ElasticAdopt {
            array_id: array_id.to_string(),
            owner: owner.clone(),
        },
        explicit,
        Duration::from_secs(60),
    )
    .await?;
    ensure!(
        out.success() && out.stdout.len() < 64 * 1024,
        "Nie można przejąć dziennika macierzy (kod {}): {}",
        out.code,
        helper_detail(&out.stderr)
    );
    let spec: ElasticCreateSpec = serde_json::from_str(&out.stdout)?;
    spec.validate()?;
    ensure!(
        spec.owner == *owner && spec.array_id == array_id && spec.name == name,
        "Helper zwrócił niezgodną specyfikację przejmowanej macierzy"
    );
    store::elastic_import(db, &spec, started_by)?;
    // The audit of WHO the array was taken from. Ids are an operator's
    // business, so they go where the operator reads — the node's log — and
    // never onto a screen another tenant's admin may be looking at.
    tracing::info!(
        array_id,
        array = %name,
        previous_org_id = %previous.org_id,
        previous_addon_id = %previous.addon_id,
        org_id = %owner.org_id,
        addon_id = %owner.addon_id,
        started_by,
        "tentanas Elastic: array adopted from its previous owner"
    );
    Ok(name)
}

async fn execute_job(h: &jobs::JobHandle, spec: ElasticCreateSpec, operation_id: String,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>) -> Result<()> {
    let result = async {
        let out = run.await?;
        ensure!(out.success(), "Helper Elastic zwrócił błąd {}",out.code);
        ensure!(out.stdout.len() < 64 * 1024, "Odpowiedź Elastic przekracza limit");
        let result: ElasticResult = serde_json::from_str(&out.stdout)?;
        // A Restore of an array an add is unfinished on answers with the
        // add's disk counted (K5); the add itself is what finishes it.
        let pinned = store::unfinished_elastic_add_disk(h.db(), &spec.owner, &spec.array_id)?;
        validate_result(&answered_spec(&spec, pinned.as_ref(), &result)?, &result)?;
        Ok::<_,anyhow::Error>(result)
    }.await;
    let key = format!("elastic:{}:restore",spec.array_id);
    match result {
        Ok(result) => {
            store::finish_elastic_operation(h.db(), &spec.owner, &operation_id, Ok(&result))?;
            if result.stage != ElasticStage::Ready {
                let detail = result.detail.as_deref().unwrap_or("Macierz wymaga interwencji; rezerwacje zachowane");
                // The helper's own text, when it gave one, is shown as-is;
                // without it the screen words the default itself.
                let text = store::AlertText::new("elastic_needs_attention", "Macierz wymaga interwencji", detail)
                    .param("array", &spec.name);
                let text = match result.detail.as_deref() {
                    Some(helper) => text.param("helper_detail", helper),
                    None => text,
                };
                store::raise_coded_alert(h.db(), &key, "warning", "elastic-array", &spec.name, &text)?;
                return Err(anyhow!(detail.to_string()));
            }
            store::resolve_alert(h.db(), &key)?;
            h.progress(100);
            Ok(())
        }
        Err(error) => {
            let detail = format!("{error}; utrata odpowiedzi nie dowodzi zatrzymania I/O");
            store::finish_elastic_operation(h.db(), &spec.owner, &operation_id, Err(&detail))?;
            // Refused as busy, nothing ran: no alert, and the caller asks again.
            if !error.to_string().contains(tentanas_helper::elastic::ELASTIC_BUSY) {
                let text = store::AlertText::new("elastic_result_unconfirmed", "Niepotwierdzony wynik macierzy", &detail)
                    .param("array", &spec.name)
                    .param("error", &error);
                store::raise_coded_alert(h.db(), &key, "warning", "elastic-array", &spec.name, &text)?;
            }
            Err(error)
        }
    }
}

pub fn snapraid_kind(kind: &ElasticSnapraidKind) -> &'static str {
    match kind {
        ElasticSnapraidKind::Sync => "sync",
        ElasticSnapraidKind::Scrub => "scrub",
        ElasticSnapraidKind::Fix { .. } => "fix",
    }
}

/// The disk a repair names, or `None` for the array-wide operations.
pub fn snapraid_disk(kind: &ElasticSnapraidKind) -> Option<&str> {
    match kind {
        ElasticSnapraidKind::Fix { disk } => Some(disk.as_str()),
        _ => None,
    }
}

/// The helper command of one SnapRAID run. `acknowledge_parity_fault` is
/// carried into a Sync only — the admin's confirm of a Sync over a recorded
/// Scrub or Repair fault, naming that fault's operation, which the helper
/// refuses without it — and is meaningless for the other two kinds, which
/// never carry it.
pub fn snapraid_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
    kind: &ElasticSnapraidKind,
    acknowledge_parity_fault: Option<&str>,
) -> HelperCommand {
    match kind {
        ElasticSnapraidKind::Sync => HelperCommand::ElasticSync {
            owner: owner.clone(),
            array_id: array_id.into(),
            operation_id: operation_id.into(),
            acknowledge_parity_fault: acknowledge_parity_fault.map(str::to_string),
        },
        ElasticSnapraidKind::Scrub => HelperCommand::ElasticScrub {
            owner: owner.clone(),
            array_id: array_id.into(),
            operation_id: operation_id.into(),
        },
        ElasticSnapraidKind::Fix { disk } => HelperCommand::ElasticFix {
            owner: owner.clone(),
            array_id: array_id.into(),
            operation_id: operation_id.into(),
            disk: disk.clone(),
        },
    }
}

pub fn add_disk_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
    disk: &ElasticDiskSpec,
) -> HelperCommand {
    HelperCommand::ElasticAddDisk {
        owner: owner.clone(),
        array_id: array_id.into(),
        operation_id: operation_id.into(),
        disk: disk.clone(),
    }
}

pub fn add_disk_abort_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
    disk: &ElasticDiskSpec,
) -> HelperCommand {
    HelperCommand::ElasticAddDiskAbort {
        owner: owner.clone(),
        array_id: array_id.into(),
        operation_id: operation_id.into(),
        disk: disk.clone(),
    }
}

/// The refusal a job reports for one of the helper's closed refusal codes,
/// as the code the screen words (`refusal.elastic_<code>`, `format.js`
/// `errMessage`). A code outside the vocabulary is not invented a sentence:
/// the caller's own fallback is used.
pub fn refusal_error(code: &str) -> Option<String> {
    const CODES: &[&str] = &[
        "no_parity",
        "precondition_failed",
        "unsynced_changes",
        "empty_parity",
        "operation_pending",
        "attention_other",
        "fault_unacknowledged",
        "attention_add_disk",
        "attention_parity_fault",
        "disk_claimed",
        "add_joined",
        "disk_absent",
    ];
    CODES.contains(&code).then(|| format!("refusal:elastic_{code}"))
}

pub fn dissolve_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
) -> HelperCommand {
    HelperCommand::ElasticDestroy {
        owner: owner.clone(),
        array_id: array_id.into(),
        operation_id: operation_id.into(),
    }
}

pub fn validate_snapraid_result(
    spec: &ElasticCreateSpec,
    operation_id: &str,
    kind: &ElasticSnapraidKind,
    result: &ElasticSnapraidResult,
) -> Result<()> {
    let repairing = matches!(kind, ElasticSnapraidKind::Fix { .. });
    if result.run.outcome == ElasticSnapraidOutcome::Refused {
        validate_observation(spec, &result.state)?;
    } else {
        validate_result(spec, &result.state)?;
    }
    let run = &result.run;
    ensure!(
        run.operation_id == operation_id && run.kind == *kind,
        "Obca operacja SnapRAID"
    );
    tentanas_helper::elastic::validate_elastic_uuid(&run.operation_id)?;
    ensure!(
        operation_id != spec.operation_id,
        "Operacja SnapRAID nie może zastępować Create"
    );
    let started = chrono::DateTime::parse_from_rfc3339(&run.started_at)?;
    let finished = chrono::DateTime::parse_from_rfc3339(
        run.finished_at
            .as_deref()
            .ok_or_else(|| anyhow!("Brak końca operacji SnapRAID"))?,
    )?;
    ensure!(
        finished >= started && run.detail.as_ref().is_none_or(|d| d.len() < 8192),
        "Niespójny czas lub opis wyniku SnapRAID"
    );
    match run.outcome {
        // A REFUSAL WROTE NOTHING (I5), whatever stage the array stands in:
        // the helper answers its admission gate with the journal exactly as
        // it read it — an array that needs attention for a cause the run does
        // not resolve, one with an intent pending, one an add is unfinished
        // on — and the core closes the run `failed` with the array untouched.
        ElasticSnapraidOutcome::Refused => {
            ensure!(
                run.exit_code.is_none()
                    && run.total_blocks.is_none()
                    && run.checked_blocks.is_none()
                    && run.accessed_mb.is_none()
                    && run.errors_file.is_none()
                    && run.errors_io.is_none()
                    && run.errors_data.is_none()
                    && matches!(
                        run.detail.as_deref(),
                        Some(
                            "no_parity"
                                | "precondition_failed"
                                | "unsynced_changes"
                                | "empty_parity"
                                // The admission gate's own vocabulary
                                // (`ElasticRefusal`) for a Sync, Scrub or Fix.
                                | "operation_pending"
                                | "attention_other"
                                | "fault_unacknowledged"
                                | "attention_add_disk"
                        )
                    )
                    // A Sync is the only run an acknowledgement is about.
                    && (run.detail.as_deref() != Some("fault_unacknowledged")
                        || *kind == ElasticSnapraidKind::Sync)
                    && result.state.last_run.as_ref() != Some(run),
                "Niepotwierdzona odmowa przed scrub"
            );
        }
        ElasticSnapraidOutcome::Succeeded => {
            let empty_sync = *kind == ElasticSnapraidKind::Sync
                && run.total_blocks == Some(0)
                && run.checked_blocks.is_none()
                && run.accessed_mb.is_none()
                && run.errors_file.is_none()
                && run.errors_io.is_none()
                && run.errors_data.is_none();
            ensure!(
                result.state.stage == ElasticStage::Ready
                    && result.state.last_run.as_ref() == Some(run)
                    && run.exit_code == Some(0)
                    // A REPAIR reports the errors it repaired, so its counters
                    // are its work and not its failure — demanding three zeroes
                    // here would reject every run that actually rebuilt
                    // anything. Its success is the exit code plus the array
                    // coming back Ready, which is what `perform_maintenance`
                    // and the helper's own summary contract decided.
                    && (repairing
                        || empty_sync
                        || (run.errors_file == Some(0)
                            && run.errors_io == Some(0)
                            && run.errors_data == Some(0))),
                "Niepotwierdzony sukces SnapRAID"
            );
            if *kind == ElasticSnapraidKind::Scrub {
                ensure!(
                    run.total_blocks
                        .zip(run.checked_blocks)
                        .is_some_and(|(total, checked)| checked > 0 && checked <= total)
                        && run.accessed_mb.is_some(),
                    "Scrub nie potwierdził pełnego zakresu"
                );
            }
            if *kind == ElasticSnapraidKind::Sync {
                ensure!(
                    result.state.sync_completed_at == run.finished_at,
                    "Inny checkpoint sync"
                );
            }
            if let ElasticSnapraidKind::Fix { disk } = kind {
                // A repair rebuilds data from the parity checkpoint that is
                // already on disk; it writes no new checkpoint, so claiming
                // one would date the array's protection to the repair.
                ensure!(
                    result.state.sync_completed_at.as_ref() != run.finished_at.as_ref(),
                    "Naprawa nie zapisuje checkpointu sync"
                );
                ensure!(
                    spec.data
                        .iter()
                        .enumerate()
                        .any(|(index, _)| tentanas_helper::elastic::data_branch_name(index + 1)
                            == *disk),
                    "Naprawa wskazała dysk, którego macierz nie ma"
                );
            }
        }
        // A Sync that ran with the share writable and skipped the files that
        // changed under it. The array serves on with parity marked out of date,
        // and the checkpoint stays where the last complete Sync put it.
        ElasticSnapraidOutcome::Partial => {
            ensure!(
                *kind == ElasticSnapraidKind::Sync
                    && result.state.stage == ElasticStage::Ready
                    && result.state.last_run.as_ref() == Some(run)
                    && partial_sync_counters(run)
                    && result.state.parity_stale
                    && result.state.sync_completed_at != run.finished_at,
                "Niepotwierdzony częściowy sync"
            );
        }
        ElasticSnapraidOutcome::Failed | ElasticSnapraidOutcome::NeedsAttention => {
            ensure!(
                result.state.stage == ElasticStage::NeedsAttention
                    && result.state.last_run.as_ref() == Some(run),
                "Błąd SnapRAID nie zachował nieukończonej operacji"
            );
        }
        // A repair that ran to its end and wrote NOTHING. It is only ever a
        // repair, it reports what it found, and the array is exactly as it was
        // — which is what keeps the fault it was started for in place.
        ElasticSnapraidOutcome::NothingRepaired => {
            ensure!(
                repairing
                    && result.state.last_run.as_ref() == Some(run)
                    && run.exit_code == Some(0)
                    && run.detail.as_ref().is_some_and(|detail| !detail.is_empty())
                    && result.state.sync_completed_at.as_ref() != run.finished_at.as_ref(),
                "Niepotwierdzona naprawa bez efektu"
            );
        }
        ElasticSnapraidOutcome::Running => {
            return Err(anyhow!("Helper nie dostarczył terminalnego wyniku"));
        }
    }
    Ok(())
}

async fn execute_snapraid_job(
    h: &jobs::JobHandle,
    spec: &ElasticCreateSpec,
    operation_id: &str,
    kind: &ElasticSnapraidKind,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let output = run.await?;
    ensure!(
        output.success() && output.stdout.len() < 64 * 1024,
        "Brak wiarygodnego wyniku SnapRAID"
    );
    let result: ElasticSnapraidResult = serde_json::from_str(&output.stdout)?;
    // While an add is pinned the helper's journal counts its disk too, and a
    // run refused over it answers about THAT array (K5).
    let pinned = store::unfinished_elastic_add_disk(h.db(), &spec.owner, &spec.array_id)?;
    validate_snapraid_result(&answered_spec(spec, pinned.as_ref(), &result.state)?, operation_id, kind, &result)?;
    store::record_snapraid_result(h.db(), &spec.owner, operation_id, &result)?;
    // A REFUSAL is worded by the screen from its closed code: the helper
    // changed nothing, and the reason is what the admin acts on.
    if result.run.outcome == ElasticSnapraidOutcome::Refused {
        if let Some(refusal) = result.run.detail.as_deref().and_then(refusal_error) {
            return Err(anyhow!(refusal));
        }
    }
    ensure!(
        matches!(result.run.outcome, ElasticSnapraidOutcome::Succeeded | ElasticSnapraidOutcome::Partial),
        "{}",
        result
            .run
            .detail
            .as_deref()
            .unwrap_or("Operacja SnapRAID nie zakończyła się sukcesem")
    );
    Ok(())
}

pub fn mover_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
    resume_operation_id: &str,
    rules: &MoverRules,
    coupled_sync: bool,
) -> HelperCommand {
    HelperCommand::ElasticMover {
        array_id: array_id.into(),
        owner: owner.clone(),
        operation_id: operation_id.into(),
        resume_operation_id: resume_operation_id.into(),
        rules: rules.clone(),
        coupled_sync,
    }
}

/// The mover's answer, against the intent that asked for it.
///
/// The SHAPE of `last_mover` — both uuids canonical, the resume distinct from
/// the run and from Create, the times in order, an unfinished run without a
/// finish time, and the coupled sync belonging to this operation and sitting
/// inside the mover's own window — is already enforced by
/// `validate_observation`, which every Elastic answer passes through. This
/// routes the mover result through that same validator instead of growing a
/// second one that could drift from it.
pub fn validate_mover_result(
    spec: &ElasticCreateSpec,
    operation_id: &str,
    result: &ElasticMoverResult,
) -> Result<()> {
    // A completed run leaves the array serving, so it is held to the full
    // `Ready` contract — which is what proves no Hold stands and the union is
    // read-write; one that stopped part-way is only an observation.
    if result.run.phase == ElasticMoverPhase::Complete {
        validate_result(spec, &result.state)?;
        ensure!(
            result.state.stage == ElasticStage::Ready,
            "Ukończony mover bez macierzy gotowej do pracy"
        );
    } else {
        validate_observation(spec, &result.state)?;
    }
    let run = &result.run;
    // THE ONE CHECK `validate_observation` CANNOT MAKE: it never sees the job's
    // operation id, so it cannot tell this answer apart from an older run the
    // helper still reports as `last_mover`. Without it a mover job could be
    // closed by the result of the previous one.
    ensure!(run.operation_id == operation_id, "Obca operacja movera");
    ensure!(
        result.state.last_mover.as_ref() == Some(run),
        "Wynik movera nie jest stanem macierzy"
    );
    ensure!(
        run.detail.as_ref().is_none_or(|d| d.len() < 8192),
        "Zbyt długi opis wyniku movera"
    );
    // A terminal answer is Complete, or NeedsAttention: closed, or left for the
    // next run with something unresolved. Anything still moving is the helper
    // failing to deliver a verdict.
    ensure!(
        matches!(run.phase, ElasticMoverPhase::Complete | ElasticMoverPhase::NeedsAttention),
        "Helper nie dostarczył terminalnego wyniku movera"
    );
    Ok(())
}

async fn execute_mover_job(
    h: &jobs::JobHandle,
    spec: &ElasticCreateSpec,
    operation_id: &str,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let output = run.await?;
    ensure!(
        output.success() && output.stdout.len() < 64 * 1024,
        "Brak wiarygodnego wyniku movera"
    );
    let result: ElasticMoverResult = serde_json::from_str(&output.stdout)?;
    validate_mover_result(spec, operation_id, &result)?;
    store::record_mover_result(h.db(), &spec.owner, operation_id, &result)?;
    if let Err(error) = record_conflict_alert(h.db(), &spec.name, &result.state.conflicts) {
        tracing::warn!("tentanas mover: conflict alert of {} not recorded: {error}", spec.name);
    }
    ensure!(
        result.run.phase == ElasticMoverPhase::Complete,
        "{}",
        result
            .run
            .detail
            .as_deref()
            .unwrap_or("Mover nie zakończył przenoszenia")
    );
    Ok(())
}

pub fn spawn_mover(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    // A SECOND reservation, distinct from the run's own and from Create: the
    // helper refuses a command whose resume repeats its operation, and
    // `validate_observation` refuses such an answer. The helper records it
    // with the run; a restart of the same operation must carry it.
    let resume_operation_id = uuid::Uuid::now_v7().to_string();
    let rules = array.mover_rules();
    let coupled_sync = array.mover.coupled_sync;
    let intent = jobs::ElasticJobIntent::Mover {
        owner: spec.owner.clone(),
        array_id: spec.array_id.clone(),
        operation_id: operation_id.clone(),
        resume_operation_id: resume_operation_id.clone(),
        rules: rules.clone(),
        coupled_sync,
    };
    jobs::spawn(
        db,
        "elastic_mover",
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command = mover_command(
                &spec.owner,
                &spec.array_id,
                &operation_id,
                &resume_operation_id,
                &rules,
                coupled_sync,
            );
            // A Hold an older helper's mover left is released by the helper
            // itself before it moves anything; the result is then held to the
            // `Ready` contract, which proves the union serves read-write.
            let run = jobs::run_step(
                &h,
                &command,
                explicit.as_deref(),
                Duration::from_secs(24 * 60 * 60),
            );
            execute_mover_job(&h, &spec, &operation_id, run).await
        },
    )
}

/// Starts one Sync, Scrub or Fix. `acknowledge_parity_fault` is the admin's
/// confirm of a Sync over a recorded fault; the scheduler passes `false`
/// always, and only the manual Sync request may pass `true`.
pub fn spawn_snapraid(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    kind: ElasticSnapraidKind,
    acknowledge_parity_fault: Option<String>,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let acknowledge_parity_fault = acknowledge_parity_fault.filter(|_| kind == ElasticSnapraidKind::Sync);
    let intent = jobs::ElasticJobIntent::Snapraid {
        owner: spec.owner.clone(),
        array_id: spec.array_id.clone(),
        operation_id: operation_id.clone(),
        kind: kind.clone(),
        acknowledge_parity_fault: acknowledge_parity_fault.clone(),
    };
    jobs::spawn(
        db,
        &format!("elastic_{}", snapraid_kind(&kind)),
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command =
                snapraid_command(&spec.owner, &spec.array_id, &operation_id, &kind, acknowledge_parity_fault.as_deref());
            let run = jobs::run_step(
                &h,
                &command,
                explicit.as_deref(),
                Duration::from_secs(24 * 60 * 60),
            );
            execute_snapraid_job(&h, &spec, &operation_id, &kind, run).await
        },
    )
}

/// The array the add would produce, as its persisted intention: the row's own
/// spec with one more data disk on the end.
///
/// It exists because BOTH sides need exactly this object and neither may build
/// its own: the helper's answer is validated against it, and the disk row is
/// written from the same slot it puts the disk in. Two constructions of "the
/// array plus one disk" that disagreed by a slot would write a row for one
/// array and validate the answer of another.
pub fn spec_with_added_disk(
    spec: &ElasticCreateSpec,
    disk: &ElasticDiskSpec,
) -> Result<ElasticCreateSpec> {
    let mut after = spec.clone();
    after.data.push(disk.clone());
    after.validate()?;
    Ok(after)
}

/// THE ONE ANSWER to "which array does this helper answer describe" (K5).
///
/// The core's spec is the array WITHOUT an add that has not finished: the
/// member row is written only once the add succeeds. The helper's journal
/// grows the moment the add starts, so while an add is pinned
/// (`db::unfinished_elastic_add_disk`) every answer about the array — an
/// inspection, a Restore, a SnapRAID run, a refusal — may count one data disk
/// more. That answer is valid exactly then, and only for the pinned disk; any
/// other member count is an answer about some other array.
pub fn answered_spec(
    spec: &ElasticCreateSpec,
    pinned: Option<&ElasticDiskSpec>,
    result: &ElasticResult,
) -> Result<ElasticCreateSpec> {
    let members = member_count(spec);
    if result.disks.len() == members {
        return Ok(spec.clone());
    }
    match pinned {
        Some(disk) if result.disks.len() == members + 1 => spec_with_added_disk(spec, disk),
        _ => Err(anyhow!("Niepełny zestaw obserwacji dysków")),
    }
}

/// `answered_spec` for a RECORDED answer, which may be older than the spec:
/// a run recorded before an add succeeded describes the array with fewer
/// data disks. Such a record is judged against the array as it was then —
/// the spec's own data disks, in slot order, less the ones added since.
pub fn recorded_spec(
    spec: &ElasticCreateSpec,
    pinned: Option<&ElasticDiskSpec>,
    result: &ElasticResult,
) -> Result<ElasticCreateSpec> {
    let members = member_count(spec);
    if result.disks.len() < members && members - result.disks.len() < spec.data.len() {
        let mut earlier = spec.clone();
        earlier.data.truncate(spec.data.len() - (members - result.disks.len()));
        return Ok(earlier);
    }
    answered_spec(spec, pinned, result)
}

/// The array one slot's disk replaced by another produces, validated as a
/// whole. It is the same shape `spec_with_added_disk` has, and for the same
/// reason: the rules that matter — identity uniqueness, the device ceiling,
/// the sizes — are rules about the ARRAY, not about the disk.
pub fn spec_with_replaced_disk(
    spec: &ElasticCreateSpec,
    branch: &str,
    disk: &ElasticDiskSpec,
) -> Result<ElasticCreateSpec> {
    tentanas_helper::elastic::validate_data_branch_name(branch)?;
    let slot = spec
        .data
        .iter()
        .enumerate()
        .position(|(index, _)| tentanas_helper::elastic::data_branch_name(index + 1) == branch)
        .ok_or_else(|| anyhow!("Macierz nie ma dysku danych '{branch}'"))?;
    let old = &spec.data[slot];
    ensure!(
        disk.bytes >= old.bytes,
        "Dysk zamienny jest mniejszy niż slot '{branch}' ({} < {})",
        disk.bytes,
        old.bytes
    );
    if let Some(parity) = spec.parity.iter().map(|parity| parity.bytes).min() {
        ensure!(
            disk.bytes <= parity,
            "Dysk zamienny jest większy niż parity macierzy; parity nie pokryłaby go w całości"
        );
    }
    let mut after = spec.clone();
    after.data[slot] = disk.clone();
    after.validate()?;
    Ok(after)
}

pub fn replace_disk_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
    rebuild_operation_id: &str,
    sync_operation_id: &str,
    branch: &str,
    disk: &ElasticDiskSpec,
    accept_stale_parity: bool,
) -> HelperCommand {
    HelperCommand::ElasticReplaceDisk {
        owner: owner.clone(),
        array_id: array_id.into(),
        operation_id: operation_id.into(),
        rebuild_operation_id: rebuild_operation_id.into(),
        sync_operation_id: sync_operation_id.into(),
        branch: branch.into(),
        disk: disk.clone(),
        accept_stale_parity,
    }
}

/// What the helper must have done for a replacement to count: the array serves
/// again with the NEW disk in that slot, the rebuild wrote blocks back, and the
/// Sync that followed recorded them.
pub fn validate_replace_result(
    spec_after: &ElasticCreateSpec,
    branch: &str,
    disk: &ElasticDiskSpec,
    operation_ids: (&str, &str),
    result: &tentanas_helper::elastic::ElasticReplaceResult,
) -> Result<()> {
    ensure!(
        result.branch == branch && result.disk == *disk,
        "Wymiana dotyczy innego slotu lub dysku"
    );
    validate_result(spec_after, &result.state)?;
    let (rebuild_id, sync_id) = operation_ids;
    let rebuild_kind = ElasticSnapraidKind::Fix { disk: branch.to_string() };
    ensure!(
        result.rebuild.operation_id == rebuild_id && result.rebuild.kind == rebuild_kind,
        "Obca operacja odbudowy"
    );
    ensure!(
        result.sync.operation_id == sync_id && result.sync.kind == ElasticSnapraidKind::Sync,
        "Obca operacja sync po odbudowie"
    );
    // The rebuild WROTE the disk back: a run that recovered nothing is not a
    // rebuild, and the helper reports that as its own outcome.
    ensure!(
        result.rebuild.outcome == ElasticSnapraidOutcome::Succeeded,
        "Odbudowa dysku nie zakończyła się sukcesem"
    );
    ensure!(
        matches!(result.sync.outcome, ElasticSnapraidOutcome::Succeeded | ElasticSnapraidOutcome::Partial),
        "Sync po odbudowie nie zakończył się sukcesem"
    );
    // The array's own state has to agree that the Sync is the last one it ran.
    ensure!(
        result.state.last_run.as_ref() == Some(&result.sync),
        "Stan macierzy nie potwierdza sync po odbudowie"
    );
    Ok(())
}

/// Why a REPLACEMENT is pointless on this slot, or `None` when it is the
/// operation the slot needs. It reads the OBSERVED array, like
/// `repair_blocker`, because presence is a measurement and not a database
/// column — and it answers only when there IS a measurement: an array whose
/// disk is gone often cannot be inspected at all after a boot, and that
/// silence must not be read as "the disk is fine".
pub fn replacement_blocker(array: &NasElasticArray, branch: &str) -> Option<String> {
    let slot = array.data_disks.iter().find(|member| member.name == branch)?;
    if slot.device_present == Some(true) && slot.health != "failing" {
        return Some(format!(
            "dysk '{branch}' jest obecny na tym nodzie i nie zgłasza awarii: jeśli ma błędy, \
             użyj naprawy z parity, a wymianę uruchom po jego odłączeniu"
        ));
    }
    None
}

/// DORMANT: disk replacement is withdrawn (round 4, the owner's decision) and
/// nothing calls this. `dispatch::tentanas::elastic_replace_disk` refuses the
/// request before any write, `db::insert_job` refuses the intent so no
/// `replace_disk` operation can be opened, and the helper's `actions::run`
/// refuses the command. The helper-side banner on
/// `tentanas_helper::elastic::execution::replace_data_disk` lists what the task
/// that finishes the feature still has to solve.
pub fn spawn_replace_disk(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    branch: String,
    disk: ElasticDiskSpec,
    accept_stale_parity: bool,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec_before = array.persisted_spec()?.clone();
    let spec_after = spec_with_replaced_disk(&spec_before, &branch, &disk)?;
    let operation_id = uuid::Uuid::now_v7().to_string();
    // The two SnapRAID runs the flow records get their reservations here, so
    // the request row names every operation the helper will write.
    let rebuild_operation_id = uuid::Uuid::now_v7().to_string();
    let sync_operation_id = uuid::Uuid::now_v7().to_string();
    let intent = jobs::ElasticJobIntent::ReplaceDisk {
        owner: spec_after.owner.clone(),
        array_id: spec_after.array_id.clone(),
        operation_id: operation_id.clone(),
        rebuild_operation_id: rebuild_operation_id.clone(),
        sync_operation_id: sync_operation_id.clone(),
        branch: branch.clone(),
        disk: disk.clone(),
        accept_stale_parity,
    };
    jobs::spawn(
        db,
        "elastic_replace_disk",
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command = replace_disk_command(
                &spec_after.owner,
                &spec_after.array_id,
                &operation_id,
                &rebuild_operation_id,
                &sync_operation_id,
                &branch,
                &disk,
                accept_stale_parity,
            );
            // A rebuild reads every other disk of the array and writes a whole
            // disk: it takes as long as the array is large, so it gets the same
            // day-long ceiling the other Elastic operations have.
            let run = jobs::run_step(&h, &command, explicit.as_deref(), Duration::from_secs(24 * 60 * 60));
            execute_replace_disk_job(
                &h,
                &spec_before,
                &spec_after,
                &operation_id,
                (&rebuild_operation_id, &sync_operation_id),
                &branch,
                &disk,
                run,
            )
            .await
        },
    )
}

async fn execute_replace_disk_job(
    h: &jobs::JobHandle,
    spec_before: &ElasticCreateSpec,
    spec_after: &ElasticCreateSpec,
    operation_id: &str,
    snapraid_ids: (&str, &str),
    branch: &str,
    disk: &ElasticDiskSpec,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let key = format!("elastic:{}:replace-disk", spec_after.array_id);
    let result = async {
        let out = run.await?;
        ensure!(out.success(), "Helper Elastic zwrócił błąd {}", out.code);
        ensure!(out.stdout.len() < 256 * 1024, "Odpowiedź Elastic przekracza limit");
        let result: tentanas_helper::elastic::ElasticReplaceResult = serde_json::from_str(&out.stdout)?;
        validate_replace_result(spec_after, branch, disk, snapraid_ids, &result)?;
        Ok::<_, anyhow::Error>(result)
    }
    .await;
    let outcome = match result {
        Ok(result) => store::finish_elastic_replace_disk(
            h.db(),
            &spec_after.owner,
            operation_id,
            branch,
            disk,
            &result.state,
        )
        .map(|()| result),
        Err(error) => Err(error),
    };
    match outcome {
        Ok(result) => {
            store::resolve_alert(h.db(), &key)?;
            h.log(format!(
                "odbudowano {branch}: {}",
                result.rebuild.detail.as_deref().unwrap_or("bez opisu")
            ));
            h.progress(100);
            Ok(())
        }
        Err(error) => {
            // The JOURNAL, not this row, is what a repeat resumes from: the
            // helper swapped the slot's identity before it formatted anything,
            // so this operation is closed as needing attention and the same
            // command may be asked again with the SAME identity — which the
            // request row above is what records.
            let _ = spec_before;
            let detail = format!("{error}; wymiana dysku nie została potwierdzona");
            store::finish_elastic_operation(h.db(), &spec_after.owner, operation_id, Err(&detail))?;
            let text = store::AlertText::new("elastic_replace_unconfirmed", "Niepotwierdzona wymiana dysku", &detail)
                .param("array", &spec_after.name)
                .param("error", &error);
            store::raise_coded_alert(h.db(), &key, "warning", "elastic-array", &spec_after.name, &text)?;
            Err(error)
        }
    }
}

/// Members a persisted intention describes, across every role.
fn member_count(spec: &ElasticCreateSpec) -> usize {
    spec.data.len() + usize::from(spec.cache.is_some()) + spec.parity.len()
}

/// The job error of an add that stopped part-way, worded by the screen.
const ADD_STOPPED: &str = "refusal:elastic_add_stopped";
/// ...and of an undo that stopped after its first effect.
const UNDO_STOPPED: &str = "refusal:elastic_undo_stopped";

/// What one add's answer came to.
#[derive(Debug)]
enum AddVerdict {
    /// The add succeeded: the helper's array is the one with the disk.
    Joined(ElasticResult),
    /// The helper REFUSED before it changed anything (I5): the answer is the
    /// journal exactly as it was, and the job closes `failed` with the
    /// refusal's code, pinning nothing. `None` for an answer that counted the
    /// array without the disk and named no code (an older helper's).
    Refused(Option<tentanas_helper::elastic::ElasticRefusal>),
    /// The add ran and stopped part-way: the disk is the helper's pending
    /// add, the operation stays needs_attention and pins it, and the resume
    /// (or, before the disk joined the share, the undo) is the way on.
    Stopped(ElasticResult),
}

/// Reads one add's answer. WHICH array the answer describes decides which
/// spec validates it: a refusal answers with the array as it was — without
/// the disk, or with the disk when it is some OTHER add's resume that was
/// refused — and a run that got through answers with the disk. Holding a
/// refusal to `spec_after` replaced its own sentence with "Niepełny zestaw
/// obserwacji dysków", which told the admin the helper was inconsistent
/// instead of telling them why it said no.
fn add_disk_verdict(
    spec_before: &ElasticCreateSpec,
    spec_after: &ElasticCreateSpec,
    result: ElasticResult,
) -> Result<AddVerdict> {
    let grown = result.disks.len() == member_count(spec_after);
    if grown {
        validate_observation(spec_after, &result)?;
    } else {
        validate_observation(spec_before, &result)?;
    }
    if result.refused.is_some() || !grown {
        return Ok(AddVerdict::Refused(result.refused));
    }
    if result.stage == ElasticStage::Ready && result.pending_add.is_none() {
        validate_result(spec_after, &result)?;
        return Ok(AddVerdict::Joined(result));
    }
    Ok(AddVerdict::Stopped(result))
}

async fn execute_add_disk_job(
    h: &jobs::JobHandle,
    spec_before: &ElasticCreateSpec,
    spec_after: &ElasticCreateSpec,
    operation_id: &str,
    disk: &ElasticDiskSpec,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let key = format!("elastic:{}:add-disk", spec_after.array_id);
    let verdict = async {
        let out = run.await?;
        ensure!(out.success(), "Helper Elastic zwrócił błąd {}", out.code);
        ensure!(out.stdout.len() < 64 * 1024, "Odpowiedź Elastic przekracza limit");
        let result: ElasticResult = serde_json::from_str(&out.stdout)?;
        add_disk_verdict(spec_before, spec_after, result)
    }
    .await;
    let outcome = match verdict {
        // K3: A REFUSAL IS NOT A FAULT. The row closes `failed`, which pins
        // nothing (`db::unfinished_elastic_add_disk` reads only a stopped
        // add), the array keeps its state, and no add alert is raised: the
        // job's own error is the refusal, worded by the screen.
        Ok(AddVerdict::Refused(refusal)) => {
            let error = refusal
                .and_then(|refusal| refusal_error(refusal.as_str()))
                .unwrap_or_else(|| "refusal:elastic_precondition_failed".to_string());
            store::refuse_elastic_operation(h.db(), &spec_after.owner, operation_id, &error)?;
            return Err(anyhow!(error));
        }
        // A stopped add: the helper says where, and the member row is NOT
        // written. The helper recorded the slot and the add's cause, so the
        // resume of this very disk is what finishes it.
        // The job's error is a code the screen words; the helper's own text
        // stays with the operation row and the alert (node text).
        Ok(AddVerdict::Stopped(result)) => {
            let detail = result
                .detail
                .unwrap_or_else(|| "Dodanie dysku wymaga interwencji; rezerwacje zachowane".into());
            let detail = format!("{detail}; dysk nie został dopisany do macierzy");
            store::finish_elastic_operation(h.db(), &spec_after.owner, operation_id, Err(&detail))?;
            let text = store::AlertText::new("elastic_add_disk_unconfirmed", "Niepotwierdzone dodanie dysku", &detail)
                .param("array", &spec_after.name)
                .param("error", &detail);
            store::raise_coded_alert(h.db(), &key, "warning", "elastic-array", &spec_after.name, &text)?;
            return Err(anyhow!(ADD_STOPPED));
        }
        Ok(AddVerdict::Joined(result)) => {
            // ONE transaction writes the member row and closes the operation,
            // so the instance database never holds an array whose spec and
            // whose finished operation disagree about how many disks it has.
            store::finish_elastic_add_disk(h.db(), &spec_after.owner, operation_id, disk, &result)
                .map(|()| result)
        }
        Err(error) => Err(error),
    };
    match outcome {
        Ok(_) => {
            store::resolve_alert(h.db(), &key)?;
            h.progress(100);
            Ok(())
        }
        Err(error) => {
            let detail = format!("{error}; dysk nie został dopisany do macierzy");
            store::finish_elastic_operation(h.db(), &spec_after.owner, operation_id, Err(&detail))?;
            let text = store::AlertText::new("elastic_add_disk_unconfirmed", "Niepotwierdzone dodanie dysku", &detail)
                .param("array", &spec_after.name)
                .param("error", &error);
            store::raise_coded_alert(h.db(), &key, "warning", "elastic-array", &spec_after.name, &text)?;
            Err(error)
        }
    }
}

pub fn spawn_add_disk(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    disk: ElasticDiskSpec,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec_before = array.persisted_spec()?.clone();
    let spec_after = spec_with_added_disk(&spec_before, &disk)?;
    let operation_id = uuid::Uuid::now_v7().to_string();
    let intent = jobs::ElasticJobIntent::AddDisk {
        owner: spec_after.owner.clone(),
        array_id: spec_after.array_id.clone(),
        operation_id: operation_id.clone(),
        disk: disk.clone(),
    };
    jobs::spawn(
        db,
        "elastic_add_disk",
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command =
                add_disk_command(&spec_after.owner, &spec_after.array_id, &operation_id, &disk);
            let run = jobs::run_step(
                &h,
                &command,
                explicit.as_deref(),
                Duration::from_secs(24 * 60 * 60),
            );
            execute_add_disk_job(&h, &spec_before, &spec_after, &operation_id, &disk, run).await
        },
    )
}

/// The undo of an unfinished add. `spec` is the array WITHOUT the disk (the
/// core never wrote its member row), `disk` the pinned add's identity.
async fn execute_add_disk_abort_job(
    h: &jobs::JobHandle,
    spec: &ElasticCreateSpec,
    operation_id: &str,
    disk: &ElasticDiskSpec,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let key = format!("elastic:{}:add-disk", spec.array_id);
    let answer = async {
        let out = run.await?;
        ensure!(out.success(), "Helper Elastic zwrócił błąd {}", out.code);
        ensure!(out.stdout.len() < 64 * 1024, "Odpowiedź Elastic przekracza limit");
        let result: ElasticResult = serde_json::from_str(&out.stdout)?;
        validate_observation(&answered_spec(spec, Some(disk), &result)?, &result)?;
        Ok::<_, anyhow::Error>(result)
    }
    .await;
    let error = match answer {
        // Refused before its first effect: the add stays exactly as it was,
        // and pinned; this row closes `failed` and changes nothing else.
        Ok(result) if result.refused.is_some() => {
            let error = result
                .refused
                .and_then(|refusal| refusal_error(refusal.as_str()))
                .unwrap_or_else(|| "refusal:elastic_precondition_failed".to_string());
            store::refuse_elastic_operation(h.db(), &spec.owner, operation_id, &error)?;
            return Err(anyhow!(error));
        }
        // The slot is gone: the helper's array is the core's again.
        Ok(result) if result.disks.len() == member_count(spec) && result.pending_add.is_none() => {
            store::finish_elastic_add_disk_abort(h.db(), &spec.owner, operation_id, &result)?;
            store::resolve_alert(h.db(), &key)?;
            h.progress(100);
            return Ok(());
        }
        // Stopped after its first effect: the add is still recorded, so it
        // stays pinned and may be undone again or resumed.
        Ok(result) => anyhow!(result
            .detail
            .unwrap_or_else(|| "Wycofanie dodawania dysku wymaga interwencji".into())),
        Err(error) => error,
    };
    let detail = format!("{error}; dodawanie dysku pozostaje niedokończone");
    store::finish_elastic_operation(h.db(), &spec.owner, operation_id, Err(&detail))?;
    let text = store::AlertText::new(
        "elastic_add_disk_abort_unconfirmed",
        "Niepotwierdzone wycofanie dodawania dysku",
        &detail,
    )
    .param("array", &spec.name)
    .param("error", &error);
    store::raise_coded_alert(h.db(), &key, "warning", "elastic-array", &spec.name, &text)?;
    Err(anyhow!(UNDO_STOPPED))
}

pub fn spawn_add_disk_abort(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    disk: ElasticDiskSpec,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let intent = jobs::ElasticJobIntent::AddDiskAbort {
        owner: spec.owner.clone(),
        array_id: spec.array_id.clone(),
        operation_id: operation_id.clone(),
        disk: disk.clone(),
    };
    jobs::spawn(
        db,
        "elastic_add_disk_abort",
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command = add_disk_abort_command(&spec.owner, &spec.array_id, &operation_id, &disk);
            let run = jobs::run_step(&h, &command, explicit.as_deref(), Duration::from_secs(60 * 60));
            execute_add_disk_abort_job(&h, &spec, &operation_id, &disk, run).await
        },
    )
}

/// The dissolve's answer, against the array that asked for it.
///
/// `data_kept` is checked rather than assumed: it is the promise the dialog
/// made to the admin — the disks keep their filesystems and the array import
/// takes the array back — and a helper that ever stopped keeping them must
/// fail the job instead of having the promise quietly become false.
pub fn validate_dissolve_result(
    spec: &ElasticCreateSpec,
    operation_id: &str,
    result: &ElasticDissolveResult,
) -> Result<()> {
    ensure!(
        result.array_id == spec.array_id
            && result.owner == spec.owner
            && result.name == spec.name
            && result.operation_id == operation_id,
        "Odpowiedź rozwiązania dotyczy innej macierzy lub operacji"
    );
    ensure!(result.data_kept, "Helper nie potwierdził zachowania danych");
    ensure!(
        result.released.first().map(String::as_str)
            == Some(tentanas_helper::elastic::union_path(&spec.name).as_str()),
        "Rozwiązanie nie zwolniło unii jako pierwszej"
    );
    ensure!(result.steps.len() < 64 * 1024, "Za duży plan rozwiązania");
    Ok(())
}

async fn execute_dissolve_job(
    h: &jobs::JobHandle,
    spec: &ElasticCreateSpec,
    operation_id: &str,
    alerts: &[String],
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let out = run.await?;
    ensure!(out.success(), "Helper Elastic zwrócił błąd {}", out.code);
    ensure!(out.stdout.len() < 64 * 1024, "Odpowiedź Elastic przekracza limit");
    let result: ElasticDissolveResult = serde_json::from_str(&out.stdout)?;
    validate_dissolve_result(spec, operation_id, &result)?;
    for line in result.steps.lines() {
        h.log(line);
    }
    for path in &result.released {
        h.log(format!("zwolniono {path}"));
    }
    // ONLY after the node has stopped serving the array. Deleting the rows
    // first would leave a union with no supervision: nothing would know the
    // mount existed, and `block_elastic_teardown` would let an uninstall
    // proceed over a live share.
    store::delete_elastic_array(h.db(), &spec.owner, &spec.array_id)?;
    // Alerts outlive their subject: a branch alert of an array nothing has a
    // row for any more can never be resolved by a later reconcile, so it would
    // sit red on the dashboard forever.
    for key in alerts {
        store::resolve_alert(h.db(), key)?;
    }
    h.progress(100);
    Ok(())
}

pub fn spawn_dissolve(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let mut alerts = alert_keys(array);
    alerts.push(format!("elastic:{}:restore", spec.array_id));
    alerts.push(format!("elastic:{}:add-disk", spec.array_id));
    let intent = jobs::ElasticJobIntent::Dissolve {
        owner: spec.owner.clone(),
        array_id: spec.array_id.clone(),
        operation_id: operation_id.clone(),
    };
    jobs::spawn(
        db,
        "elastic_destroy",
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command = dissolve_command(&spec.owner, &spec.array_id, &operation_id);
            let run = jobs::run_step(
                &h,
                &command,
                explicit.as_deref(),
                Duration::from_secs(60 * 60),
            );
            execute_dissolve_job(&h, &spec, &operation_id, &alerts, run).await
        },
    )
}

pub async fn create_job(h: jobs::JobHandle, spec: ElasticCreateSpec,
    explicit: Option<Arc<ElevationToken>>) -> Result<()> {
    let command = HelperCommand::ElasticCreate { operation: spec.clone() };
    let run = jobs::run_step(&h,&command,explicit.as_deref(),Duration::from_secs(24 * 60 * 60));
    execute_job(&h, spec.clone(), spec.operation_id.clone(), run).await
}

/// The ONE command a restore job sends. After a reboot the helper answers a
/// private array nothing but a Restore or a Resume
/// (`tentanas_helper::elastic::runs_after_a_boot`), so a restore job that sent
/// anything first — an Inspect to look before it acts — would fail on every
/// array at every startup. A Hold an older helper's mover left is released by
/// the helper's Restore itself.
pub fn restore_command(spec: &ElasticCreateSpec) -> HelperCommand {
    HelperCommand::ElasticRestore { array_id: spec.array_id.clone(), owner: spec.owner.clone() }
}

pub fn spawn_restore(db: &DbPool, array: &ElasticArrayRow, started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    completion: Option<tokio::sync::oneshot::Sender<Result<()>>>) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let intent = jobs::ElasticJobIntent::Restore { owner: spec.owner.clone(),
        array_id: spec.array_id.clone(), operation_id: operation_id.clone() };
    jobs::spawn(db,"elastic_restore",&array.name,started_by,Some(intent),completion,move |h| async move {
        let command = restore_command(&spec);
        let run = jobs::run_step(&h,&command,explicit.as_deref(),Duration::from_secs(24 * 60 * 60));
        execute_job(&h,spec,operation_id,run).await
    })
}

async fn observe_array(db: &DbPool, array: &ElasticArrayRow) -> Result<ElasticResult> {
    let spec = array.persisted_spec()?;
    let (out, _) = super::broker::run_privileged(db, &HelperCommand::ElasticInspect {
        array_id: spec.array_id.clone(),owner:spec.owner.clone() }, None,Duration::from_secs(30)).await?;
    ensure!(out.success() && out.stdout.len() < 64 * 1024, "Nie można odczytać macierzy");
    let result: ElasticResult = serde_json::from_str(&out.stdout)?;
    validate_observation(&answered_spec(spec, array.pending_add.as_ref(), &result)?, &result)?;
    Ok(result)
}

/// Whether the helper answers about an array whose pinned add it no longer
/// holds: the array counted without the disk, no add recorded as the cause,
/// none reported unfinished. That is an undo (or a 0.13.0 refusal) whose end
/// the core did not get to record — a core restart during the undo job
/// (`fail_orphaned_jobs`) leaves exactly this (M4 of the release review).
pub fn add_undone_by_helper(array: &ElasticArrayRow, result: &ElasticResult) -> bool {
    let Ok(spec) = array.persisted_spec() else { return false };
    array.pending_add.is_some()
        && result.disks.len() == member_count(spec)
        && result.pending_add.is_none()
        && !matches!(result.attention, Some(tentanas_helper::elastic::ElasticAttention::AddDisk { .. }))
}

/// Settles such an add from the helper's own answer, and re-reads the row so
/// the wire says what the array now is. A failure changes nothing: the next
/// read tries again.
fn reconcile_undone_add(db: &DbPool, array: &mut ElasticArrayRow, result: &ElasticResult) {
    if !add_undone_by_helper(array, result) {
        return;
    }
    let Ok(spec) = array.persisted_spec().cloned() else { return };
    match store::settle_undone_add(db, &spec.owner, &spec.array_id, result) {
        Ok(()) => {
            if let Ok(Some(fresh)) = store::elastic_array(db, &spec.owner, &array.name) {
                *array = fresh;
            }
        }
        Err(error) => tracing::warn!("elastic {}: an undone add not settled: {error}", array.name),
    }
}

/// A recorded answer that counted one data disk more than its spec, from an
/// add no longer pinned (finished and undone since): its extra slot is
/// dropped before the record is judged, so one old row cannot make the whole
/// array unreadable (minor 6 of the release review).
pub fn strip_unpinned_slot(spec: &ElasticCreateSpec, pinned: Option<&ElasticDiskSpec>, result: &mut ElasticResult) {
    if pinned.is_none() && result.disks.len() == member_count(spec) + 1 {
        let extra = u16::try_from(spec.data.len() + 1).ok().map(ElasticRole::Data);
        result.disks.retain(|disk| Some(disk.role) != extra);
        result.pending_add = None;
    }
}

/// What a mover run would find on this array's cache right now, measured by the
/// helper without moving anything, under the rules a run would use.
///
/// Expensive by construction — a root walk of the whole cache and of every
/// process's descriptors — which is why the scheduler calls it at most once per
/// `MOVER_RETRIGGER_COOLDOWN` per array and never while an operation runs.
pub async fn observe_cache_age(db: &DbPool, array: &ElasticArrayRow) -> Result<ElasticCacheAge> {
    let spec = array.persisted_spec()?;
    let command = HelperCommand::ElasticCacheAge {
        array_id: spec.array_id.clone(),
        owner: spec.owner.clone(),
        rules: array.mover_rules(),
    };
    let (out, _) =
        super::broker::run_privileged(db, &command, None, Duration::from_secs(10 * 60)).await?;
    ensure!(
        out.success() && out.stdout.len() < 4 * 1024,
        "Nie można zmierzyć plików czekających na cache (kod {})",
        out.code
    );
    Ok(serde_json::from_str(&out.stdout)?)
}

/// Just the branch measurements of one array, for the scheduler's
/// cache-pressure trigger.
///
/// It asks the helper the same question the full read does and keeps only the
/// probes, because that is all `mover_trigger` consults. The state, the
/// protection window and the parity history are deliberately not computed
/// here: none of them decides whether the cache is low, and the tick would be
/// paying for them once a minute per array.
pub async fn observe_cache(db: &DbPool, array: &ElasticArrayRow) -> Result<ArrayObservation> {
    let result = observe_array(db, array).await?;
    let mut observed = ArrayObservation {
        mount_table_known: result.union_mounted.is_some()
            && result.disks.iter().all(|d| d.mounted.is_some()),
        union_mounted: result.union_mounted,
        conflicts: result.conflicts,
        ..Default::default()
    };
    observed.record_disks(&array.name, result.disks);
    Ok(observed)
}

fn observed_protocol(array: &ElasticArrayRow, disks: &BTreeMap<String,NasDisk>,
    result: Result<ElasticResult>, features: &[FeatureState]) -> NasElasticArray {
    let mut observed = ArrayObservation::default();
    let mut failure;
    let mut root_stage = None;
    let helper = result.as_ref().ok().cloned();
    match result {
        Ok(result) => {
            observed.mount_table_known = result.union_mounted.is_some()
                && result.disks.iter().all(|d| d.mounted.is_some());
            observed.union_mounted = result.union_mounted;
            observed.last_sync_at = result.sync_completed_at;
            observed.parity_stale = result.parity_stale;
            // Not stale is the helper AFFIRMING that no mover-moved bytes are
            // outstanding — a measurement, not a gap. `None` here would make
            // every healthy array read "unknown" and the protected arm dead.
            observed.moved_unsynced_bytes =
                if result.parity_stale { result.stale_parity_bytes } else { Some(0) };
            // Parity errors come from the runs themselves, over the window the
            // wire advertises — see `parity_errors_in_window`.
            observed.parity_errors = parity_errors_in_window(array, chrono::Utc::now());
            observed.last_mover = result.last_mover.clone();
            observed.conflicts = result.conflicts.clone();
            root_stage = Some(result.stage);
            // The helper's own sentence has no code: the screen says that the
            // helper reported a failure, and the sentence is its tooltip.
            failure = result.detail.map(|detail| CodedText::new("helper_failed", &[], detail)).or_else(|| {
                if result.stage == ElasticStage::Ready
                    && result.service.as_ref().is_some_and(|service| {
                        service.mode != ElasticServiceMode::Online
                            || service.pending
                            || result.union_readonly != Some(false)
                    })
                {
                    Some(CodedText::new("service_not_online", &[], "Service nie potwierdził trybu Online RW"))
                } else {
                    None
                }
            });
            // A recovery after a boot stopped part-way: only a reboot retries it.
            // The code says the one thing the admin must do; the cause the
            // helper gave stays in the sentence.
            if result.restart_required {
                failure = Some(match failure {
                    Some(detail) => CodedText::new(
                        "restart_required",
                        &[],
                        format!("Wymagany restart noda: {}", detail.text),
                    ),
                    None => CodedText::new("restart_required", &[], "Wymagany restart noda"),
                });
            }
            let service_safe = result.service.as_ref().is_none_or(|service| {
                service.mode == ElasticServiceMode::Online
                    && !service.pending
                    && result.union_readonly == Some(false)
            });
            if result.stage == ElasticStage::Ready && !service_safe {
                root_stage = Some(ElasticStage::NeedsAttention);
            }
            observed.record_disks(&array.name, result.disks);
        }
        Err(error) => failure = Some(CodedText::new("helper_failed", &[], error.to_string())),
    }
    let installed = |id: &str| features.iter().any(|f| f.id == id && f.status == "ok");
    let snapraid = features.iter().find(|f| f.id == SNAPRAID_FEATURE_ID);
    let (mut state, mut detail, _) = array_state(array,&observed,&installed);
    if let Some(stage) = root_stage {
        if stage != ElasticStage::Ready {
            state = "needs_attention";
            detail = failure.unwrap_or_else(|| {
                CodedText::new("checkpoint_unfinished", &[], "Nieukończony checkpoint helpera")
            });
        } else if array.state != "active" {
            state = if array.state == "creating" { "creating" } else { "needs_attention" };
            // A sentence the database holds (an operation's own words) has
            // no code; the empty one is the node's own and has.
            detail = if array.state_detail.is_empty() {
                CodedText::new(
                    "awaiting_confirmation",
                    &[],
                    "Oczekuje na trwałe potwierdzenie operacji w bazie instancji",
                )
            } else {
                // Stored with its codes since migration 23 (an operation's
                // error: `operation_failed` {operation}; a lost supervision:
                // `supervision_lost`), so the screen words it and keeps the
                // stored sentence for the tooltip.
                CodedText { text: array.state_detail.clone(), reasons: array.state_reasons.clone() }
            };
        }
    } else {
        state = "unknown";
        detail = failure.unwrap_or_default();
    }
    let mut wire = to_protocol(array,disks,&observed,installed(SNAPRAID_FEATURE_ID),
        snapraid.and_then(|f| f.version.as_deref()).unwrap_or(""),(state,detail.text.as_str()));
    wire.state_reasons = detail.reasons;
    if let Some(result) = &helper {
        apply_helper_gates(&mut wire, result);
    }
    wire
}

pub async fn list(db: &DbPool, owner: &ElasticOwner) -> Result<Vec<NasElasticArray>> {
    let rows = store::elastic_arrays(db,owner)?;
    let environment = super::environment::cached_or_probe(db).await;
    let features = environment.as_ref().map(|e| e.features.as_slice()).unwrap_or(&[]);
    let disks = super::disks::snapshot().0.into_iter().map(|d| (d.disk_id.clone(),d)).collect();
    let mut arrays = Vec::with_capacity(rows.len());
    for mut array in rows {
        let observed = match &environment {
            Ok(_) => observe_array(db,&array).await,
            Err(error) => Err(anyhow!("Brak pomiaru środowiska: {error}")),
        };
        if let Ok(result) = &observed {
            reconcile_undone_add(db, &mut array, result);
        }
        let mut wire = observed_protocol(&array,&disks,observed,features);
        fill_last_known_names(&mut wire, |id| store::disk_last_name(db, id).ok().flatten());
        arrays.push(wire);
    }
    Ok(arrays)
}

pub async fn get(db: &DbPool, owner: &ElasticOwner, name: &str) -> Result<Option<NasElasticArray>> {
    Ok(list(db,owner).await?.into_iter().find(|a| a.name == name))
}

async fn restore_startup_rows<F>(rows: &[ElasticArrayRow], start: F) -> Result<()>
where
    F: FnMut(
        &ElasticArrayRow,
        tokio::sync::oneshot::Sender<Result<()>>,
    ) -> Result<tentaflow_protocol::tentanas::NasJob>,
{
    restore_startup_rows_waiting(rows, STARTUP_BUSY_WAIT, STARTUP_RESTORE_WAIT, start).await
}

/// How many times a startup Restore the helper refused as busy is asked again,
/// and how long it waits before each. Something else held the node lock for a
/// moment — a UI read, another owner's command — and nothing ran.
const STARTUP_BUSY_ATTEMPTS: u32 = 6;
const STARTUP_BUSY_WAIT: Duration = Duration::from_secs(10);

/// How long the queue waits for ONE array's Restore before it gives up on it
/// and attempts the next (W-C of the third review). A Restore mounts the
/// branches, starts mergerfs and settles at most one file record; it does not
/// sync. A job that has not answered in this long is not going to, and the
/// whole node's Sync, Scrub and mover passes are waiting behind it.
const STARTUP_RESTORE_WAIT: Duration = Duration::from_secs(10 * 60);

/// The longest the scheduler's Elastic passes wait for the startup queue. Past
/// it they run anyway: a queue this old is stuck on one array, and the cache of
/// every other array still has to be drained.
const STARTUP_GATE_MAX: Duration = Duration::from_secs(20 * 60);

/// Startup restore queues still running in this process, and when the oldest
/// of them began.
static STARTUP_RESTORES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static STARTUP_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// Whether a startup restore queue is still running. The scheduler's Elastic
/// passes wait for it (A1 of the second review) — but never longer than
/// `STARTUP_GATE_MAX`, because a queue stuck on one array must not stop every
/// other array's Sync, Scrub and mover (W-C of the third review).
pub fn startup_restores_pending() -> bool {
    startup_restores_pending_for(STARTUP_GATE_MAX)
}

/// The same, with the bound as a parameter: a test cannot wind the clock.
pub fn startup_restores_pending_for(max: Duration) -> bool {
    if STARTUP_RESTORES.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        return false;
    }
    STARTUP_SINCE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some_and(|since| since.elapsed() < max)
}

/// Held for as long as one startup restore queue runs, panic included.
pub(crate) struct StartupRestores;

impl StartupRestores {
    pub(crate) fn begin() -> Self {
        let mut since = STARTUP_SINCE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if STARTUP_RESTORES.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            *since = Some(Instant::now());
        }
        StartupRestores
    }
}

impl Drop for StartupRestores {
    fn drop(&mut self) {
        let mut since = STARTUP_SINCE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if STARTUP_RESTORES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            *since = None;
        }
    }
}

async fn restore_startup_rows_waiting<F>(
    rows: &[ElasticArrayRow],
    busy_wait: Duration,
    restore_wait: Duration,
    mut start: F,
) -> Result<()>
where
    F: FnMut(
        &ElasticArrayRow,
        tokio::sync::oneshot::Sender<Result<()>>,
    ) -> Result<tentaflow_protocol::tentanas::NasJob>,
{
    static STARTUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _startup = STARTUP.lock().await;
    // Every array is attempted: one array that cannot come back — refused,
    // failed, its job not accepted — must not keep every array after it down.
    // Each failure is already recorded on its own job and alert; this collects
    // them. Only a completion that never arrives stops the queue: the job's end
    // is then unknown, and its helper may still hold the node lock the next
    // restore needs.
    let mut failures = Vec::new();
    for row in rows {
      let mut attempt = 1;
      loop {
        let (completion, finished) = tokio::sync::oneshot::channel();
        if let Err(error) = start(row, completion) {
            tracing::warn!("tentanas Elastic startup: {}: {error}", row.name);
            failures.push(format!("{}: {error}", row.name));
            break;
        }
        // BOUNDED (W-C): a job that never answers is one array's failure, not
        // the node's. Its helper may still hold the node lock, so the arrays
        // after it meet a busy refusal — which is retried above.
        let answer = match tokio::time::timeout(restore_wait, finished).await {
            Ok(answer) => answer,
            Err(_) => {
                let text = format!("{}: przywracanie nie odpowiedziało w czasie {restore_wait:?}", row.name);
                tracing::warn!("tentanas Elastic startup: {text}");
                failures.push(text);
                break;
            }
        };
        match answer {
            Ok(Ok(())) => {}
            // Refused as busy: nothing ran. It is asked again, a few times, so
            // one lock collision does not leave the array unrestored.
            Ok(Err(error))
                if error.to_string().contains(tentanas_helper::elastic::ELASTIC_BUSY)
                    && attempt < STARTUP_BUSY_ATTEMPTS =>
            {
                tracing::info!("tentanas Elastic startup: {}: helper busy, attempt {attempt}", row.name);
                attempt += 1;
                tokio::time::sleep(busy_wait).await;
                continue;
            }
            Ok(Err(error)) => {
                tracing::warn!("tentanas Elastic startup: {}: {error}", row.name);
                failures.push(format!("{}: {error}", row.name));
            }
            Err(_) => {
                failures.push(format!("{}: Brak potwierdzenia zakończenia przywracania", row.name));
                return Err(anyhow::anyhow!(failures.join("; ")));
            }
        }
        break;
      }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(failures.join("; ")))
    }
}

/// The startup queue's order. A4 of the second review: share configs are
/// written once after the arrays are back — a share's state reads whether its
/// union is mounted — whatever the restores ended with, and while the queue
/// still holds the scheduler's Elastic passes, so a share an older core
/// generated gains the veto that keeps the mover's temporary and quarantined
/// files out of SMB clients' reach before anything moves.
async fn restore_then_rewrite_shares<A, F>(
    restore: impl std::future::Future<Output = Result<()>>,
    rewrite: A,
) -> Result<()>
where
    A: FnOnce() -> F,
    F: std::future::Future<Output = Result<Vec<String>>>,
{
    let restored = restore.await;
    if let Err(error) = rewrite().await {
        tracing::warn!("tentanas Elastic startup: share configs not rewritten: {error}");
    }
    restored
}

/// Who the Tasks tab shows as the starter of a boot-time Restore: the node
/// itself, not an account, exactly like `scheduler::STARTED_BY`. Every
/// author check that lets the scheduler through must let this one through
/// too (see `dispatch::tentanas::is_system_author`).
pub const STARTED_BY_STARTUP: &str = "startup";

pub fn start_restore(main_db: DbPool, db: DbPool, owner: ElasticOwner) {
    static STARTING: OnceLock<Mutex<BTreeSet<(String,String)>>> = OnceLock::new();
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return; };
    let starting = STARTING.get_or_init(|| Mutex::new(BTreeSet::new()));
    let key = (owner.org_id.clone(),owner.addon_id.clone());
    if !starting.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone()) { return; }
    // Taken HERE, before the task is spawned: the scheduler's first tick may
    // run on another thread the moment it is, and it must already wait.
    let pending = StartupRestores::begin();
    runtime.spawn(async move {
        let _pending = pending;
        let outcome = async {
            if !super::instance_should_run(&main_db,&db) { return Ok(()); }
            let rows = store::elastic_arrays(&db,&owner)?;
            let status = super::elevation::status(&db).await;
            let allowed = status.mode == "helper" && status.helper_state == "ok" && status.core_compatible;
            if allowed {
                return restore_then_rewrite_shares(
                    restore_startup_rows(&rows, |row, completion| {
                        if !super::instance_should_run(&main_db,&db) {
                            anyhow::bail!("Instancja nie jest aktywna; przywracanie zatrzymane");
                        }
                        spawn_restore(&db,row,STARTED_BY_STARTUP,None,Some(completion))
                    }),
                    || super::shares::apply(&db, &main_db, &owner.addon_id, None, super::shares::ApplyTrigger::Startup),
                ).await;
            }
            for row in rows {
                let spec = row.persisted_spec()?;
                let text = store::AlertText::new("elastic_restore_waiting", "Macierz oczekuje na przywrócenie",
                    "Brak bezobsługowego kanału roota; wymagane jawne Przywróć").param("array", &row.name);
                store::raise_coded_alert(&db,&format!("elastic:{}:restore",spec.array_id),"warning",
                    "elastic-array",&row.name,&text)?;
            }
            Ok::<_,anyhow::Error>(())
        }.await;
        if let Err(error) = outcome { tracing::warn!("tentanas Elastic startup: {error}"); }
        starting.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One observed array on the wire, healthy, as the three lifecycle gates
    /// read it. Only the fields those gates consult are filled.
    fn observed_array() -> NasElasticArray {
        NasElasticArray {
            name: "media".into(),
            kind: KIND.into(),
            state: "active".into(),
            enabled: true,
            data_disks: vec![NasElasticBranch {
                disk_id: "media-data".into(),
                name: "d1".into(),
                role: "data".into(),
                mounted: Some(true),
                device_present: Some(true),
                health: "ok".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn parity_run(kind: &str, outcome: &str) -> tentaflow_protocol::tentanas::NasSnapraidRun {
        tentaflow_protocol::tentanas::NasSnapraidRun {
            kind: kind.into(),
            outcome: outcome.into(),
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:10:00Z".into()),
            // A scrub that ended without success and counted errors is what
            // marks blocks for a repair to write back.
            errors: (kind == "scrub" && matches!(outcome, "failed" | "needs_attention")).then_some(7),
            ..Default::default()
        }
    }

    /// THE DEFECT THIS PINS: a repair was armed by `unresolved_operation`,
    /// which is equally true for a mover that stopped part-way and for an
    /// add-disk that failed — so the product offered to overwrite a data disk
    /// from parity over a mover's problem, and called it
    /// "nierozwiązana operacja parity". A parity repair needs parity evidence.
    #[test]
    fn a_repair_is_armed_by_parity_evidence_and_never_by_another_operation() {
        // Healthy: nothing to recover.
        assert_eq!(repair_evidence(&observed_array()), None);

        // `unresolved_operation` on its own is NOT evidence. It is what a
        // stuck mover and a failed add both set, and neither is repairable
        // from parity.
        let mut stuck = observed_array();
        stuck.unresolved_operation = true;
        stuck.state = "needs_attention".into();
        assert_eq!(
            repair_evidence(&stuck),
            None,
            "a mover or an add that stopped is not a parity fault"
        );

        // A PARITY RUN that ended without success is.
        for outcome in ["failed", "needs_attention"] {
            let mut hurt = observed_array();
            hurt.snapraid.history = vec![parity_run("scrub", outcome)];
            assert!(
                repair_evidence(&hurt).is_some_and(|why| why.contains("scrub") && why.contains('7')),
                "{outcome}: {:?}",
                repair_evidence(&hurt)
            );
        }
        // ONLY A SCRUB THAT COUNTED ERRORS. A repair writes back the blocks
        // snapraid MARKED as bad, and a scrub is what marks them: measured on
        // rig11, `-e fix` with no scrub behind it writes nothing at all and
        // still reports "Everything OK". So neither a failed Sync, nor a
        // scrub that counted no errors, nor a parity-error figure from the
        // reporting window arms a repair.
        let mut failed_sync = observed_array();
        failed_sync.snapraid.history = vec![parity_run("sync", "failed")];
        assert_eq!(repair_evidence(&failed_sync), None, "a failed sync marks no blocks");
        let mut uncounted = observed_array();
        uncounted.snapraid.history = vec![tentaflow_protocol::tentanas::NasSnapraidRun {
            errors: Some(0),
            ..parity_run("scrub", "failed")
        }];
        assert_eq!(repair_evidence(&uncounted), None, "a scrub that found nothing marks nothing");
        let mut counted = observed_array();
        counted.snapraid.parity_errors = Some(4);
        assert_eq!(
            repair_evidence(&counted),
            None,
            "the reporting window's figure is not a mark in the content file"
        );

        // A run that was merely REFUSED is not a fault, and neither is one
        // still going.
        for outcome in ["refused", "running", "ok", "nothing_repaired"] {
            let mut other = observed_array();
            other.snapraid.history = vec![parity_run("scrub", outcome)];
            assert_eq!(repair_evidence(&other), None, "{outcome}");
        }
        // A repair that wrote NOTHING settles nothing: the scrub's marks stand
        // and the repair is still what the array needs.
        let mut nothing = observed_array();
        nothing.snapraid.history = vec![parity_run("fix", "nothing_repaired"), parity_run("scrub", "failed")];
        assert!(repair_evidence(&nothing).is_some(), "a repair that did nothing did not settle the scrub");

        // The history arrives NEWEST FIRST, so a successful repair above an
        // unsuccessful run is one that came after it and settled it — the rule
        // `db::UNRESOLVED_ELASTIC_OPERATION` applies in SQL over the same rows.
        let mut healed = observed_array();
        healed.snapraid.history = vec![parity_run("fix", "ok"), parity_run("scrub", "failed")];
        assert_eq!(repair_evidence(&healed), None);
        let mut hurt_again = observed_array();
        hurt_again.snapraid.history = vec![
            parity_run("scrub", "failed"),
            parity_run("fix", "ok"),
            parity_run("scrub", "failed"),
        ];
        assert!(repair_evidence(&hurt_again).is_some(), "a newer fault stands again");
    }

    /// DL1 of the third review: a repair that recovered NOTHING must not read
    /// as a success. snapraid reports such a run "Everything OK" with exit 0
    /// (measured on rig11: `-e fix` with no scrub behind it), so the product
    /// has to judge it by its counters — and then leave the array, its state
    /// and the fault that armed the repair exactly as they were, WITHOUT
    /// turning the repair itself into an unresolved operation that only a
    /// successful repair could clear.
    #[tokio::test]
    async fn a_repair_that_wrote_nothing_is_not_a_success_and_settles_nothing() {
        let spec = mover_spec("nothing-repaired");
        let db = settled_database(&spec);
        // The state a scrub that found errors leaves: the array needs
        // attention and its operation is unresolved.
        let scrub_id = uuid::Uuid::now_v7().to_string();
        let scrub = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_scrub".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &db,
            &scrub,
            Some(&jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: scrub_id.clone(),
                kind: ElasticSnapraidKind::Scrub,
                acknowledge_parity_fault: None,
            }),
        )
        .unwrap();
        store::record_snapraid_result(
            &db,
            &spec.owner,
            &scrub_id,
            &snapraid_result(&spec, &scrub_id, ElasticSnapraidKind::Scrub, ElasticSnapraidOutcome::Failed),
        )
        .unwrap();
        store::finish_job(&db, &scrub.job_id, "failed", Some("data_error")).unwrap();
        let hurt = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(hurt.state, "needs_attention");
        assert!(hurt.unresolved_operation);

        // THE REPAIR RUNS AND WRITES NOTHING.
        let repair_id = uuid::Uuid::now_v7().to_string();
        let kind = ElasticSnapraidKind::Fix { disk: "d1".into() };
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_fix".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &db,
            &job,
            Some(&jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: repair_id.clone(),
                kind: kind.clone(),
                acknowledge_parity_fault: None,
            }),
        )
        .unwrap();
        let mut result = snapraid_result(&spec, &repair_id, kind.clone(), ElasticSnapraidOutcome::Failed);
        result.run.outcome = ElasticSnapraidOutcome::NothingRepaired;
        result.run.exit_code = Some(0);
        result.run.detail = Some("nic nie naprawiono: parity nie ma zaznaczonych błędów".into());
        // The array is exactly as the scrub left it — that is the point.
        result.state.stage = ElasticStage::NeedsAttention;
        result.state.detail = Some("scrub zgłosił błędy danych".into());
        result.state.last_run = Some(result.run.clone());
        validate_snapraid_result(&spec, &repair_id, &kind, &result).expect("a recorded run with nothing done");
        store::record_snapraid_result(&db, &spec.owner, &repair_id, &result).unwrap();
        store::finish_job(&db, &job.job_id, "failed", None).unwrap();

        let after = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        // NOT a success, and the scrub's fault still holds the array.
        assert_eq!(after.state, "needs_attention");
        assert!(after.unresolved_operation, "the scrub that found the errors still stands");
        let repair_state: String = db
            .read()
            .unwrap()
            .query_row(
                "SELECT state FROM nas_elastic_operations WHERE operation_id=?1",
                [&repair_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(repair_state, "failed", "and the repair is not itself an unresolved operation");
        assert_eq!(after.snapraid_history[0].outcome, "nothing_repaired");
        // The repair is still the way forward: the scrub's marks are what a
        // repair writes back, and nothing has written them.
        let wire = to_protocol(
            &after,
            &BTreeMap::new(),
            &ArrayObservation::default(),
            true,
            "13.0",
            ("needs_attention", ""),
        );
        assert!(repair_evidence(&wire).is_some(), "the repair is still offered");
    }

    /// Two data disks, parity and a cache — an array a slot can be replaced in.
    pub(crate) fn replaceable_spec(name: &str) -> ElasticCreateSpec {
        let mut spec = mover_spec(name);
        let mut second = spec.data[0].clone();
        second.disk_id = format!("{name}-data2");
        second.wwn = Some(format!("wwn-{name}-data2"));
        second.serial = Some(format!("serial-{name}-data2"));
        second.expected_uuid = uuid::Uuid::new_v4().to_string();
        spec.data.push(second);
        // Parity has to cover the largest data disk.
        spec.parity[0].bytes = 64 * 1024 * 1024 * 1024;
        spec.validate().expect("array a slot can be replaced in");
        spec
    }

    pub(crate) fn fresh_disk(name: &str, bytes: u64) -> ElasticDiskSpec {
        ElasticDiskSpec {
            disk_id: format!("{name}-fresh"),
            wwn: Some(format!("wwn-{name}-fresh")),
            serial: Some(format!("serial-{name}-fresh")),
            bytes,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// The array a replacement produces, and every size rule it is held to.
    #[test]
    fn a_replacement_produces_a_legal_array_and_nothing_else_moves() {
        let spec = replaceable_spec("swap");
        let fresh = fresh_disk("swap", spec.data[0].bytes);
        let after = spec_with_replaced_disk(&spec, "d1", &fresh).expect("a legal replacement");
        assert_eq!(after.data[0], fresh);
        assert_eq!(after.data[1], spec.data[1]);
        assert_eq!((&after.cache, &after.parity), (&spec.cache, &spec.parity));
        assert_eq!(
            (after.name.as_str(), after.array_id.as_str(), after.operation_id.as_str()),
            (spec.name.as_str(), spec.array_id.as_str(), spec.operation_id.as_str())
        );
        // The second slot is a different slot.
        let second = spec_with_replaced_disk(&spec, "d2", &fresh).expect("the other slot");
        assert_eq!((&second.data[0], &second.data[1]), (&spec.data[0], &fresh));
        // A slot the array does not have.
        assert!(spec_with_replaced_disk(&spec, "d9", &fresh).is_err());
        assert!(spec_with_replaced_disk(&spec, "c1", &fresh).is_err(), "a cache disk is not a data slot");
        // SIZES: at least what the slot held, never more than parity covers.
        let mut small = fresh.clone();
        small.bytes = spec.data[0].bytes - 1;
        assert!(spec_with_replaced_disk(&spec, "d1", &small)
            .expect_err("too small")
            .to_string()
            .contains("mniejszy"));
        let mut huge = fresh.clone();
        huge.bytes = spec.parity[0].bytes + 1;
        assert!(spec_with_replaced_disk(&spec, "d1", &huge)
            .expect_err("larger than parity")
            .to_string()
            .contains("parity"));
        // A disk the array already holds in another role is a repeated
        // identity, which the produced array refuses as a whole.
        let member = spec.data[1].clone();
        assert!(spec_with_replaced_disk(&spec, "d1", &member).is_err());
        let cache = spec.cache.clone().expect("cache");
        assert!(spec_with_replaced_disk(&spec, "d1", &cache).is_err());
    }

    /// What the helper must have done for a replacement to count.
    #[test]
    fn a_replacement_result_is_only_accepted_with_a_rebuild_and_a_sync() {
        let spec = replaceable_spec("rebuilt");
        let fresh = fresh_disk("rebuilt", spec.data[0].bytes);
        let after = spec_with_replaced_disk(&spec, "d1", &fresh).expect("after");
        let (rebuild_id, sync_id) = (uuid::Uuid::now_v7().to_string(), uuid::Uuid::now_v7().to_string());
        let run = |operation_id: &str, kind: ElasticSnapraidKind, outcome: ElasticSnapraidOutcome| {
            tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: operation_id.into(),
                kind,
                started_at: "2026-09-17T18:00:00Z".into(),
                finished_at: Some("2026-09-17T19:00:00Z".into()),
                outcome,
                exit_code: Some(0),
                total_blocks: Some(313),
                checked_blocks: None,
                accessed_mb: Some(1),
                errors_file: None,
                errors_io: None,
                errors_data: None,
                detail: Some("naprawiono 313 bloków".into()),
            }
        };
        let good = |rebuild: ElasticSnapraidOutcome, sync: ElasticSnapraidOutcome| {
            let mut state = ready_result(&after);
            let sync_run = run(&sync_id, ElasticSnapraidKind::Sync, sync);
            state.last_run = Some(sync_run.clone());
            tentanas_helper::elastic::ElasticReplaceResult {
                state,
                branch: "d1".into(),
                disk: fresh.clone(),
                rebuild: run(&rebuild_id, ElasticSnapraidKind::Fix { disk: "d1".into() }, rebuild),
                sync: sync_run,
            }
        };
        let ids = (rebuild_id.as_str(), sync_id.as_str());
        validate_replace_result(&after, "d1", &fresh, ids, &good(ElasticSnapraidOutcome::Succeeded, ElasticSnapraidOutcome::Succeeded))
            .expect("a rebuild and a sync");
        // A Sync that skipped files changing under it is still a Sync.
        validate_replace_result(&after, "d1", &fresh, ids, &good(ElasticSnapraidOutcome::Succeeded, ElasticSnapraidOutcome::Partial))
            .expect("a partial sync");
        // A REBUILD THAT WROTE NOTHING is not a rebuild.
        for outcome in [
            ElasticSnapraidOutcome::NothingRepaired,
            ElasticSnapraidOutcome::Failed,
            ElasticSnapraidOutcome::NeedsAttention,
            ElasticSnapraidOutcome::Refused,
        ] {
            assert!(
                validate_replace_result(&after, "d1", &fresh, ids, &good(outcome, ElasticSnapraidOutcome::Succeeded)).is_err(),
                "{outcome:?}"
            );
        }
        assert!(validate_replace_result(&after, "d1", &fresh, ids, &good(ElasticSnapraidOutcome::Succeeded, ElasticSnapraidOutcome::Failed)).is_err());
        // Another slot, another disk, another operation: all refused.
        let accepted = good(ElasticSnapraidOutcome::Succeeded, ElasticSnapraidOutcome::Succeeded);
        assert!(validate_replace_result(&after, "d2", &fresh, ids, &accepted).is_err());
        assert!(validate_replace_result(&after, "d1", &spec.data[1], ids, &accepted).is_err());
        assert!(validate_replace_result(&after, "d1", &fresh, (&sync_id, &rebuild_id), &accepted).is_err());
        // And the array the answer describes has to be the array WITH the new
        // disk: the spec from before the swap no longer validates it.
        assert!(validate_replace_result(&spec, "d1", &fresh, ids, &accepted).is_err());
    }

    /// A repair WRITES the disk it names, so a data disk that is missing or
    /// unmounted makes it impossible — and that is the very state a repair is
    /// reached for. The blocker is what turns that into a sentence the admin
    /// can act on instead of the helper's `precondition_failed` after a job
    /// has already been started.
    #[test]
    fn a_repair_is_blocked_by_the_disk_it_would_have_to_write() {
        assert_eq!(repair_blocker(&observed_array()), None);

        // The fixture's member is not in any inventory, so the sentence names
        // its part in the array — never the slot key `d1`.
        let mut absent = observed_array();
        absent.data_disks[0].device_present = Some(false);
        assert!(repair_blocker(&absent).is_some_and(|why| why.contains("dysk danych 1")
            && !why.contains("'d1'") && why.contains("podłącz")));

        let mut cold = observed_array();
        cold.data_disks[0].mounted = Some(false);
        cold.data_disks[0].disk_name = "sdg".into();
        assert!(repair_blocker(&cold).is_some_and(|why| why.contains("'sdg'")
            && why.contains("odtwórz montowania")));

        // Both are tri-states and only `Some(false)` counts: `None` is
        // "nothing looked", the state of every array on a node whose disks
        // have not been read, and reading it as absence would block them all.
        let mut unmeasured = observed_array();
        unmeasured.data_disks[0].device_present = None;
        unmeasured.data_disks[0].mounted = None;
        assert_eq!(repair_blocker(&unmeasured), None);

        // SMART alone does not block it: a disk the vendor calls failing is
        // still readable and writable, and a repair onto it is exactly what an
        // admin may want before replacing it.
        let mut failing = observed_array();
        failing.data_disks[0].health = "critical".into();
        assert_eq!(repair_blocker(&failing), None);
    }

    /// A replacement is for a slot whose disk is GONE (or one the disk itself
    /// reports as failing). A present, healthy disk is the repair's business,
    /// and formatting a replacement over it would abandon its data.
    #[test]
    fn a_replacement_is_refused_while_the_slot_still_has_its_disk() {
        let present = observed_array();
        assert!(replacement_blocker(&present, "d1").is_some_and(|why| why.contains("obecny")
            && why.contains("naprawy")));

        // GONE: the case the operation exists for.
        let mut absent = observed_array();
        absent.data_disks[0].device_present = Some(false);
        assert_eq!(replacement_blocker(&absent, "d1"), None);

        // The disk itself says it is failing: a replacement is what that is
        // for, whatever else the node can still read from it.
        let mut failing = observed_array();
        failing.data_disks[0].health = "failing".into();
        assert_eq!(replacement_blocker(&failing, "d1"), None);

        // UNMEASURED IS NOT PRESENT: `None` is "nothing looked", which is the
        // state of an array whose disks have not been read — the blocker only
        // answers on a measurement.
        let mut unmeasured = observed_array();
        unmeasured.data_disks[0].device_present = None;
        assert_eq!(replacement_blocker(&unmeasured, "d1"), None);

        // A slot this array does not have is not this function's refusal: the
        // handler answers that from the array's own row, so it holds even when
        // nothing could be measured.
        assert_eq!(replacement_blocker(&present, "d9"), None);
    }

    pub(crate) fn create_spec(name: &str) -> ElasticCreateSpec {
        let disk = |role: &str| tentanas_helper::elastic::ElasticDiskSpec {
            disk_id:format!("{name}-{role}"),wwn:Some(format!("wwn-{name}-{role}")),
            serial:Some(format!("serial-{name}-{role}")),bytes:32 * 1024 * 1024 * 1024,
            expected_uuid:uuid::Uuid::new_v4().to_string(),
        };
        ElasticCreateSpec { array_id:uuid::Uuid::new_v4().to_string(),
            operation_id:uuid::Uuid::new_v4().to_string(),owner:ElasticOwner {org_id:"org".into(),addon_id:"nas".into()},
            name:name.into(),filesystem:tentanas_helper::elastic::ElasticFilesystem::Ext4,
            data:vec![disk("data")],cache:None,parity:vec![disk("parity")] }
    }

    pub(crate) fn ready_result(spec: &ElasticCreateSpec) -> ElasticResult {
        ElasticResult { array_id:spec.array_id.clone(),operation_id:spec.operation_id.clone(),owner:spec.owner.clone(),
            stage:ElasticStage::Ready,union_mounted:Some(true),service:None,union_readonly:None,sync_completed_at:Some(store::now()),detail:None,last_run:None,last_mover:None,stale_parity_bytes:None,parity_stale:false,stuck_records:Vec::new(),stuck_hidden:0,stuck_evicted:0,restart_required:false,conflicts:Vec::new(),
            attention:None,operation_pending:false,transfer_open:false,pending_add:None,refused:None,
            disks:spec.data.iter().enumerate().map(|(i,d)| (ElasticRole::Data((i+1) as u16),d))
                .chain(spec.cache.iter().map(|d| (ElasticRole::Cache,d)))
                .chain(spec.parity.iter().enumerate().map(|(i,d)| (ElasticRole::Parity((i+1) as u8),d)))
                .map(|(role,d)| tentanas_helper::elastic::ElasticDiskObservation { role,
                    kernel_name:Some("sdz".into()),device:Some("/dev/sdz".into()),observed_uuid:Some(d.expected_uuid.clone()),
                    filesystem:Some(spec.filesystem.as_str().into()),device_present:Some(true),mounted:Some(true),
                size_bytes:Some(d.bytes),used_bytes:Some(4096),free_bytes:Some(d.bytes-4096),detail:None }).collect() }
    }

    #[test]
    fn observed_protocol_maps_last_mover_and_carries_only_recorded_history() {
        let spec = create_spec("mover-observation");
        let mut result = ready_result(&spec);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::NeedsAttention,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: None,
            moved_files: 2,
            moved_bytes: 128,
            skipped_files: 1,
            skipped_bytes: 64,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: false,
            detail: Some("częściowy transfer".into()),
            coupled_sync: Some(tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: MOVER_TEST_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T01:00:01Z".into(),
                finished_at: Some("2026-09-08T01:00:02Z".into()),
                outcome: ElasticSnapraidOutcome::Failed,
                exit_code: Some(1),
                total_blocks: None,
                checked_blocks: None,
                accessed_mb: None,
                errors_file: Some(0),
                errors_io: Some(0),
                errors_data: Some(1),
                detail: Some("sync failed".into()),
            }),
        });
        result.stage = ElasticStage::NeedsAttention;
        validate_observation(&spec, &result).expect("typed mover observation");
        let mut row = array();
        row.create_spec = Some(spec);
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        let run = wire.mover.last_run.expect("ostatni wynik movera");
        assert_eq!(run.outcome, "needs_attention");
        assert_eq!(run.moved_files, 2);
        assert_eq!(run.moved_bytes, 128);
        assert_eq!(run.skipped_files, 1);
        assert_eq!(run.skipped_bytes, 64);
        assert!(!run.counts_known);
        assert_eq!(run.detail, "częściowy transfer");
        let sync = run.coupled_sync.expect("wynik sprzężonego sync");
        assert_eq!(sync.kind, "sync");
        assert_eq!(sync.outcome, "failed");
        assert_eq!(sync.errors_data, Some(1));
        // Observation never SYNTHESISES history: what the helper reports live is
        // `last_run`, and the strip carries only what this node recorded. The
        // row under test has recorded nothing, so the strip is empty — and the
        // panel says "brak przebiegów" in words rather than printing a zero.
        assert!(wire.mover.history.is_empty());

        // ...but a row that HAS recorded runs reaches the wire unchanged. This
        // half is what stops the panel claiming no runs happened directly under
        // a populated "Ostatni przebieg" (round-2 MAJ-1).
        let mut recorded = array();
        recorded.create_spec = row.create_spec.clone();
        recorded.mover_history = vec![NasMoverRun {
            started_at: "2026-09-08T11:00:00Z".into(),
            finished_at: Some("2026-09-08T11:02:00Z".into()),
            outcome: "ok".into(),
            moved_bytes: 12 * 1024 * 1024 * 1024,
            moved_files: 3,
            counts_known: true,
            ..Default::default()
        }];
        let wire = observed_protocol(&recorded, &BTreeMap::new(), Err(anyhow!("brak pomiaru")), &[]);
        assert_eq!(wire.mover.history.len(), 1);
        assert_eq!(wire.mover.history[0].moved_bytes, 12 * 1024 * 1024 * 1024);
    }

    #[test]
    fn an_open_window_names_both_halves_when_both_are_open() {
        let array = array();
        let eighteen_gib = 18 * 1024 * 1024 * 1024;
        // Only the cache half: the canonical clause, verbatim.
        let canon = "na cache bez parity (czeka na mover)";
        let cache_only = protection(&array, &all_mounted(&array, eighteen_gib));
        assert_eq!(cache_only.status, "window_open");
        assert!(cache_only.detail.contains(canon), "{}", cache_only.detail);
        assert!(!cache_only.detail.contains("przeniesione przez mover"), "{}", cache_only.detail);
        // Only the moved half — and the cache is MEASURED as empty, so nothing
        // may claim files are waiting on it.
        let mut moved = all_mounted(&array, 0);
        moved.parity_stale = true;
        moved.moved_unsynced_bytes = Some(4096);
        let moved_only = protection(&array, &moved);
        assert_eq!(moved_only.status, "window_open");
        assert_eq!(moved_only.cache_unprotected_bytes, Some(0));
        assert!(moved_only.detail.contains("przeniesione przez mover"), "{}", moved_only.detail);
        assert!(!moved_only.detail.contains("na cache"), "pusty cache nie czeka: {}", moved_only.detail);
        // Both halves: the cache half must not promise the window closes for
        // files whose coupled sync already failed.
        let mut both = all_mounted(&array, eighteen_gib);
        both.parity_stale = true;
        both.moved_unsynced_bytes = Some(4096);
        let open = protection(&array, &both);
        assert_eq!(open.status, "window_open");
        assert_eq!(open.cache_unprotected_bytes, Some(eighteen_gib));
        assert_eq!(open.moved_unsynced_bytes, Some(4096));
        assert!(open.detail.contains(canon), "{}", open.detail);
        assert!(open.detail.contains("przeniesione przez mover"), "{}", open.detail);
        // The fourth combination: the cache figure is unknown AND the moved
        // half is known bad. The known fact must survive, beside the bytes the
        // card shows for it.
        let mut blind = all_mounted(&array, 0);
        blind.parity_stale = true;
        blind.moved_unsynced_bytes = Some(4096);
        blind.probes.insert(
            cache_branch_path("media", "nvme2n1"),
            BranchProbe {
                mounted: Some(true),
                device_present: Some(true),
                size_bytes: None,
                used_bytes: None,
                free_bytes: None,
            },
        );
        let unmeasured = protection(&array, &blind);
        assert_eq!(unmeasured.cache_unprotected_bytes, None, "figura cache pozostaje nieznana");
        assert_eq!(unmeasured.moved_unsynced_bytes, Some(4096));
        assert_eq!(unmeasured.status, "window_open", "znany zły fakt bije niezmierzony");
        assert!(unmeasured.detail.contains("przeniesione przez mover"), "{}", unmeasured.detail);
        assert!(unmeasured.detail.contains("nie zmierzył"), "{}", unmeasured.detail);
        assert!(!unmeasured.detail.contains(canon), "nie wiadomo, ile czeka: {}", unmeasured.detail);
        // A stale sync with NOTHING measured as moved must not claim moved
        // files, whatever the cache half says.
        let quantity = "przeniesione przez mover";
        let mut nothing_moved = all_mounted(&array, eighteen_gib);
        nothing_moved.parity_stale = true;
        nothing_moved.moved_unsynced_bytes = Some(0);
        let waiting_only = protection(&array, &nothing_moved);
        assert_eq!(waiting_only.status, "window_open");
        assert_eq!(waiting_only.moved_unsynced_bytes, Some(0));
        assert!(waiting_only.detail.contains(canon), "{}", waiting_only.detail);
        assert!(!waiting_only.detail.contains(quantity), "zero nie jest ilością: {}", waiting_only.detail);
        assert!(waiting_only.detail.contains("nie potwierdził ochrony"), "{}", waiting_only.detail);
        // Both halves at zero: an empty cache and a stale sync that moved
        // nothing. Neither sentence may assert a quantity.
        let mut both_zero = all_mounted(&array, 0);
        both_zero.parity_stale = true;
        both_zero.moved_unsynced_bytes = Some(0);
        let silent = protection(&array, &both_zero);
        assert_eq!(silent.status, "window_open");
        assert_eq!((silent.cache_unprotected_bytes, silent.moved_unsynced_bytes), (Some(0), Some(0)));
        assert!(!silent.detail.contains("na cache"), "{}", silent.detail);
        assert!(!silent.detail.contains(quantity), "{}", silent.detail);
        assert!(silent.detail.contains("nie potwierdził ochrony"), "{}", silent.detail);
        // An unmeasured cache beside a stale sync that moved nothing says both
        // of those things and claims neither figure.
        let mut blind_zero = both_zero.clone();
        blind_zero.probes.insert(
            cache_branch_path("media", "nvme2n1"),
            BranchProbe {
                mounted: Some(true),
                device_present: Some(true),
                size_bytes: None,
                used_bytes: None,
                free_bytes: None,
            },
        );
        let blind_silent = protection(&array, &blind_zero);
        // The closed window belongs in the sweep below too: its wording changed
        // this round, and it is subject to the same rules as the open ones.
        let mut clean = all_mounted(&array, 0);
        clean.moved_unsynced_bytes = Some(0);
        let protected = protection(&array, &clean);
        assert_eq!(protected.status, "protected", "{}", protected.detail);
        assert!(!protected.detail.contains("pomiar nie wykazał"), "{}", protected.detail);
        assert_eq!(blind_silent.cache_unprotected_bytes, None);
        assert!(blind_silent.detail.contains("nie zmierzył"), "{}", blind_silent.detail);
        assert!(!blind_silent.detail.contains(quantity), "{}", blind_silent.detail);
        // The banned phrasing stays banned in every combination, every sentence
        // on this card is in one language, and all of them read as fragments
        // that follow a figure.
        for detail in [
            cache_only.detail,
            moved_only.detail,
            open.detail,
            unmeasured.detail,
            waiting_only.detail,
            silent.detail,
            blind_silent.detail,
            protected.detail,
        ] {
            assert!(!detail.contains("godz") && !detail.contains("niezsynchronizowan"), "{detail}");
            assert!(!detail.contains("the cache") && !detail.contains("parity disk"), "{detail}");
            assert!(
                detail.chars().next().is_some_and(|first| !first.is_uppercase()),
                "zdania tej karty zaczynają się małą literą: {detail}"
            );
        }
    }

    /// Every arm of this card, not only the open window, follows one
    /// convention: a Polish fragment that reads after a figure.
    #[test]
    fn every_protection_sentence_is_a_lowercase_polish_fragment() {
        let array = array();
        let mut cases = vec![all_mounted(&array, 0), all_mounted(&array, 4096)];
        let mut stale = all_mounted(&array, 0);
        stale.parity_stale = true;
        stale.moved_unsynced_bytes = Some(0);
        cases.push(stale);
        let mut unknown_moved = all_mounted(&array, 0);
        unknown_moved.moved_unsynced_bytes = None;
        cases.push(unknown_moved);
        let mut errors = all_mounted(&array, 0);
        errors.parity_errors = Some(3);
        cases.push(errors);
        let mut unsynced = all_mounted(&array, 0);
        unsynced.last_sync_at = None;
        cases.push(unsynced);
        let mut no_parity = array.clone();
        no_parity.parity.clear();
        for observed in &cases {
            for row in [&array, &no_parity] {
                let detail = protection(row, observed).detail;
                assert!(
                    detail.chars().next().is_some_and(|first| !first.is_uppercase()),
                    "{detail}"
                );
                assert!(!detail.contains("godz") && !detail.contains("niezsynchronizowan"), "{detail}");
            }
        }
    }

    /// A row and a helper result that agree about the same array: one data
    /// disk, one parity disk and no cache, which is the shape `create_spec`
    /// and `ready_result` produce, so the probe keys the producer writes line
    /// up with the names the card reads.
    fn healthy_pair(name: &str) -> (ElasticArrayRow, ElasticResult) {
        let spec = create_spec(name);
        let result = ready_result(&spec);
        let mut row = array();
        row.name = name.to_string();
        row.branches = vec![BranchRow {
            disk_id: "id-d1".to_string(),
            name: "d1".to_string(),
            device: "/dev/sdg".to_string(),
            role: "data".to_string(),
        }];
        row.parity = vec![ParityRow {
            disk_id: "id-sdj".to_string(),
            name: "sdj".to_string(),
            device: "/dev/sdj".to_string(),
            index: 1,
        }];
        row.create_spec = Some(spec);
        (row, result)
    }

    fn snapraid_run(
        kind: &str,
        outcome: &str,
        finished: Option<chrono::DateTime<chrono::Utc>>,
        errors: (Option<u64>, Option<u64>, Option<u64>),
    ) -> tentaflow_protocol::tentanas::NasSnapraidRun {
        tentaflow_protocol::tentanas::NasSnapraidRun {
            kind: kind.to_string(),
            started_at: "2026-09-01T00:00:00Z".to_string(),
            finished_at: finished.map(|at| at.to_rfc3339()),
            outcome: outcome.to_string(),
            errors_file: errors.0,
            errors_io: errors.1,
            errors_data: errors.2,
            ..Default::default()
        }
    }

    /// THE end-to-end case: a clean completed check inside the window is what
    /// finally makes `"protected"` reachable in production.
    #[test]
    fn a_clean_run_inside_the_window_renders_protected_through_the_producer() {
        let (mut row, result) = healthy_pair("protected-window");
        row.parity_window_runs = vec![snapraid_run(
            "scrub",
            "ok",
            Some(chrono::Utc::now() - chrono::Duration::days(1)),
            (Some(0), Some(0), Some(0)),
        )];
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(wire.snapraid.parity_errors, Some(0));
        assert_eq!(wire.snapraid.parity_errors_window_days, PARITY_ERRORS_WINDOW_DAYS);
        assert_eq!(wire.protection.status, "protected", "{}", wire.protection.detail);
        assert_eq!(wire.protection.fault_tolerance, Some(1));
    }

    #[test]
    fn errors_inside_the_window_reach_the_card_through_the_producer() {
        let (mut row, result) = healthy_pair("errors-window");
        // Two file errors and one data error: all three counters are summed,
        // because the helper disqualifies a run on any of them.
        row.parity_window_runs = vec![snapraid_run(
            "sync",
            "failed",
            Some(chrono::Utc::now() - chrono::Duration::days(2)),
            (Some(2), Some(0), Some(1)),
        )];
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(wire.snapraid.parity_errors, Some(3));
        assert_eq!(wire.protection.status, "unknown");
        assert!(
            wire.protection.detail.contains("wykryto błędy parity"),
            "{}",
            wire.protection.detail
        );
    }

    #[test]
    fn nothing_completed_inside_the_window_stays_unknown_through_the_producer() {
        let older = Some(chrono::Utc::now() - chrono::Duration::days(60));
        let inside = Some(chrono::Utc::now() - chrono::Duration::days(1));
        for (case, history) in [
            ("nic nie sprawdzało", Vec::new()),
            (
                "tylko przebieg starszy niż okno",
                vec![snapraid_run("scrub", "ok", older, (Some(0), Some(0), Some(0)))],
            ),
            (
                "przebieg wciąż trwa",
                vec![snapraid_run("scrub", "running", None, (None, None, None))],
            ),
            // A terminal timestamp is not enough: a run that was cancelled or
            // refused before it read a block says nothing, even with counters.
            // MIN-4: the store cannot emit "cancelled" — `elastic_runs` writes
            // only ok / refused / failed / needs_attention / running — so this
            // case is a GUARD on the outcome filter for whoever adds an outcome
            // later, NOT end-to-end coverage of a state production can reach.
            // The "refused" case below is the reachable one.
            (
                "przebieg anulowany (strażnik filtru, nieosiągalny ze store)",
                vec![snapraid_run("scrub", "cancelled", inside, (Some(0), Some(0), Some(0)))],
            ),
            (
                "przebieg odrzucony",
                vec![snapraid_run("sync", "refused", inside, (Some(0), Some(0), Some(0)))],
            ),
            (
                "przebieg skończył się bez liczników",
                vec![snapraid_run("scrub", "ok", inside, (None, None, None))],
            ),
        ] {
            let (mut row, result) = healthy_pair("unknown-window");
            row.parity_window_runs = history;
            let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
            assert_eq!(wire.snapraid.parity_errors, None, "{case}");
            assert_eq!(wire.protection.status, "unknown", "{case}");
            assert!(
                wire.protection.detail.contains("brak pełnego pomiaru"),
                "{case}: {}",
                wire.protection.detail
            );
        }
    }

    /// THE false-zero this round exists to kill: a run WITH errors near the far
    /// edge of the window, buried behind far more newer clean runs than the
    /// display history retains. Driven through the producer.
    #[test]
    fn an_error_at_the_far_edge_of_the_window_outlives_many_newer_clean_runs() {
        let (mut row, result) = healthy_pair("far-edge-window");
        let now = chrono::Utc::now();
        let edge = now - chrono::Duration::days(i64::from(PARITY_ERRORS_WINDOW_DAYS))
            + chrono::Duration::hours(2);
        let mut history: Vec<_> = (1..=40)
            .map(|hours| {
                snapraid_run(
                    "sync",
                    "ok",
                    Some(now - chrono::Duration::hours(hours)),
                    (Some(0), Some(0), Some(0)),
                )
            })
            .collect();
        history.push(snapraid_run("scrub", "failed", Some(edge), (Some(0), Some(0), Some(4))));
        assert!(
            history.len() > 20,
            "the fixture must be longer than the display history's retention"
        );
        row.parity_window_runs = history;
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(wire.snapraid.parity_errors, Some(4));
        assert_eq!(wire.protection.status, "unknown");
        assert!(
            wire.protection.detail.contains("wykryto błędy parity"),
            "{}",
            wire.protection.detail
        );
    }

    /// A window that came back at the cap may be missing its oldest rows, so it
    /// cannot prove a zero — it answers unknown instead.
    #[test]
    fn a_window_at_the_row_cap_is_unknown_rather_than_a_confident_zero() {
        let clean: Vec<_> = (1..=i64::from(PARITY_ERRORS_MAX_ROWS))
            .map(|hours| {
                snapraid_run(
                    "sync",
                    "ok",
                    Some(chrono::Utc::now() - chrono::Duration::hours(hours)),
                    (Some(0), Some(0), Some(0)),
                )
            })
            .collect();
        let (mut row, result) = healthy_pair("under-the-cap");
        row.parity_window_runs = clean[1..].to_vec();
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(
            wire.snapraid.parity_errors,
            Some(0),
            "one row below the cap the window is whole and still measures"
        );
        assert_eq!(wire.protection.status, "protected", "{}", wire.protection.detail);

        let (mut row, result) = healthy_pair("at-the-cap");
        row.parity_window_runs = clean;
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(wire.snapraid.parity_errors, None);
        assert_eq!(wire.protection.status, "unknown");
        assert!(
            wire.protection.detail.contains("brak pełnego pomiaru"),
            "{}",
            wire.protection.detail
        );
    }

    /// Goes through the real producer (`observed_protocol`), not a hand-built
    /// `ArrayObservation`: a helper that reports parity as current is
    /// AFFIRMING that no mover-moved bytes are outstanding.
    #[test]
    fn a_healthy_array_reports_a_measured_zero_through_the_producer() {
        let spec = create_spec("healthy-producer");
        let result = ready_result(&spec);
        assert!(!result.parity_stale && result.stale_parity_bytes.is_none());
        validate_observation(&spec, &result).expect("zdrowa macierz");
        let mut row = array();
        row.create_spec = Some(spec);
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(
            wire.protection.moved_unsynced_bytes,
            Some(0),
            "brak nieaktualnej parity to zmierzone zero, nie brak pomiaru"
        );
    }

    #[test]
    fn restart_required_reaches_the_array_state_and_is_never_ready() {
        let spec = create_spec("restart-required");
        let mut result = ready_result(&spec);
        result.restart_required = true;
        assert!(validate_observation(&spec, &result).is_err(), "Ready nie czeka na restart");
        result.stage = ElasticStage::NeedsAttention;
        result.detail = Some("mount: Input/output error".into());
        validate_observation(&spec, &result).expect("stan oczekujący na restart");
        let mut row = array();
        row.create_spec = Some(spec);
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result.clone()), &[]);
        assert_eq!(wire.state, "needs_attention");
        assert_eq!(wire.state_detail, "Wymagany restart noda: mount: Input/output error");
        // The prefix comes from the flag, never from recognising a word in the
        // helper's own sentence: a cause that happens to mention a restart is
        // still prefixed exactly once.
        let mut mentions = result;
        mentions.detail = Some("restart demona mergerfs nie powiódł się".into());
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(mentions), &[]);
        assert_eq!(
            wire.state_detail,
            "Wymagany restart noda: restart demona mergerfs nie powiódł się"
        );
    }

    /// A3 of the second review: a file a stuck record left in two versions
    /// makes the array's health a warning — never "ok" — and the alert names
    /// the file, where clients see it and where the other version is. With
    /// the second version gone the health and the alert clear.
    #[test]
    fn a_file_left_in_two_versions_is_a_warning_and_an_alert_naming_both_places() {
        let spec = mover_spec("conflicted");
        let db = settled_database(&spec);
        let array = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        let conflict = ElasticConflict {
            operation_id: uuid::Uuid::now_v7().to_string(),
            path: "docs/report.odt".into(),
            visible: "/mnt/conflicted/docs/report.odt".into(),
            kept: "/mnt/tentanas-branches/conflicted/cache/c1/.tentanas-quarantine-x-1".into(),
        };
        let _ = array;
        let protected = NasElasticProtection { status: "protected".into(), ..Default::default() };
        assert_eq!(health_of(&protected, &[], &[], &[], "active", 0).0, "ok");
        let (health, reason) = health_of(&protected, &[], &[], &[], "active", 1);
        assert_eq!(health, "warning");
        assert!(reason.contains("dwóch wersjach"), "{reason}");
        let mut result = ready_result(&spec);
        result.conflicts = vec![conflict.clone()];
        let observed = observed_protocol(&store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap(), &BTreeMap::new(), Ok(result), &[]);
        assert!(
            observed.health_reason.contains("dwóch wersjach"),
            "the observation carries the conflicts to the health: {} {}",
            observed.health,
            observed.health_reason
        );
        let conflict_alerts = |db: &DbPool| {
            store::list_alerts(db, true)
                .unwrap()
                .into_iter()
                .filter(|alert| alert.subject_id == spec.name && alert.title.starts_with("Pliki zachowane w dwóch wersjach"))
                .collect::<Vec<_>>()
        };
        record_conflict_alert(&db, &spec.name, &[conflict.clone()]).unwrap();
        let alerts = conflict_alerts(&db);
        assert_eq!(alerts.len(), 1);
        for place in ["docs/report.odt", "/mnt/conflicted/docs/report.odt", "kopia w kwarantannie na dysku cache c1"] {
            assert!(alerts[0].detail.contains(place), "{place}: {}", alerts[0].detail);
        }
        // Wave-4 critic M2: the quarantine name carries the operation's uuid,
        // and the node's sentence never carries it.
        assert!(!alerts[0].detail.contains(".tentanas-quarantine-"), "{}", alerts[0].detail);
        // Coded: the count, and each file's three paths as data.
        assert_eq!(alerts[0].code, "elastic_conflict");
        assert_eq!(alerts[0].params.get("count").map(String::as_str), Some("1"));
        assert_eq!(alerts[0].reasons.len(), 1);
        assert_eq!(alerts[0].reasons[0].code, "conflict_file");
        assert_eq!(
            alerts[0].reasons[0].params.clone().into_iter().collect::<Vec<_>>(),
            vec![
                ("kept_disk".to_string(), "c1".to_string()),
                ("kept_kind".to_string(), "quarantine".to_string()),
                ("kept_path".to_string(), conflict.kept.clone()),
                ("path".to_string(), conflict.path.clone()),
                ("visible".to_string(), conflict.visible.clone()),
            ]
        );
        // Only the copy-path value carries the quarantine name; every
        // parameter a screen words is free of it.
        assert!(
            alerts[0].reasons[0]
                .params
                .iter()
                .filter(|(k, _)| k.as_str() != "kept_path")
                .all(|(_, v)| !v.contains(&conflict.operation_id) && !v.contains("quarantine-")),
            "no worded parameter carries the quarantine name: {:?}",
            alerts[0].reasons[0].params
        );
        record_conflict_alert(&db, &spec.name, &[]).unwrap();
        assert!(conflict_alerts(&db).is_empty());
    }

    /// The three places the helper's `kept` path can name, and a shape it
    /// does not: only the disk's name leaves the node, never the path.
    #[test]
    fn the_other_version_of_a_conflicted_file_is_named_by_its_disk_never_by_its_quarantine_name() {
        let uuid = "01a0cf8c-5a61-7283-8410-924a0fceb01f";
        assert_eq!(
            kept_place("media", &format!("/mnt/tentanas-branches/media/cache/nvme2n1/.tentanas-quarantine-{uuid}-3")),
            ("quarantine", Some("nvme2n1".to_string()))
        );
        assert_eq!(
            kept_place("media", "/mnt/tentanas-branches/media/data/d2/docs/a.odt"),
            ("data", Some("d2".to_string()))
        );
        // Another array's branch, a nested name under the cache, a path
        // outside the branches: the place is not guessed.
        for other in [
            "/mnt/tentanas-branches/other/cache/c1/.tentanas-quarantine-x-1".to_string(),
            format!("/mnt/tentanas-branches/media/cache/c1/sub/.tentanas-quarantine-{uuid}-1"),
            "/srv/elsewhere/a.odt".to_string(),
            "/mnt/tentanas-branches/media/cache/c1/".to_string(),
        ] {
            assert_eq!(kept_place("media", &other), ("branch", None), "{other}");
        }
        let conflict = |kept: String| ElasticConflict {
            operation_id: uuid.into(),
            path: "docs/a.odt".into(),
            visible: "/mnt/media/docs/a.odt".into(),
            kept,
        };
        let text = conflict_alert("media", &[
            conflict(format!("/mnt/tentanas-branches/media/cache/nvme2n1/.tentanas-quarantine-{uuid}-3")),
            conflict("/srv/elsewhere/a.odt".into()),
        ])
        .expect("an alert");
        assert!(!text.detail.contains(uuid) && !text.detail.contains("/srv/elsewhere"), "{}", text.detail);
        for line in &text.reasons {
            assert!(
                line.params.iter().filter(|(k, _)| k.as_str() != "kept_path").all(|(_, v)| !v.contains(uuid)),
                "{:?}",
                line.params
            );
        }
        // The quarantined copy can be copied by its path (the row's "copy
        // path" control); a place outside this array's branches cannot —
        // it could name another array.
        assert_eq!(
            text.reasons[0].params.get("kept_path").map(String::as_str),
            Some(format!("/mnt/tentanas-branches/media/cache/nvme2n1/.tentanas-quarantine-{uuid}-3").as_str())
        );
        assert_eq!(text.reasons[1].params.get("kept_kind").map(String::as_str), Some("branch"));
        assert!(!text.reasons[1].params.contains_key("kept_disk"));
        assert!(!text.reasons[1].params.contains_key("kept_path"));
    }

    #[test]
    fn stuck_records_travel_in_the_mover_detail() {
        let record = tentanas_helper::elastic::ElasticStuckRecord {
            operation_id: MOVER_TEST_OPERATION.into(),
            path: "foto/a.jpg".into(),
            path_sha256: "0".repeat(64),
            target: Some("d1".into()),
            temporary: None,
            source: tentanas_helper::elastic::ElasticFilePin { device: 1, inode: 2, size: Some(3), sha256: None },
            temporary_copy: None,
            destination: None,
            reason: "rekord nierozwiązany: EIO".into(),
            skipped: true,
        };
        let run = ElasticMoverRun {
            operation_id: uuid::Uuid::new_v4().to_string(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::Complete,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:10:00Z".into()),
            moved_files: 0,
            moved_bytes: 0,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 1,
            issues: Vec::new(),
            stuck_records: vec![record],
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: true,
            detail: None,
            coupled_sync: None,
        };
        let wire = mover_to_protocol(&run);
        assert_eq!(wire.outcome, "partial");
        assert!(wire.detail.contains("utknięte rekordy: 1: foto/a.jpg"), "{}", wire.detail);
        let mut evicted = run.clone();
        evicted.stuck_evicted = 3;
        assert!(
            mover_to_protocol(&evicted).detail.contains("wyparto z listy pominięć: 3"),
            "{}",
            mover_to_protocol(&evicted).detail
        );
        // A later run that stuck nothing says nothing, whatever this array
        // evicted in the past: the count on a run is that run's own.
        let mut clean = run.clone();
        clean.stuck_records = Vec::new();
        clean.stuck_hidden = 0;
        // Even carrying a count: the summary is entered by this run's own stuck
        // facts, never by an eviction figure on its own.
        clean.stuck_evicted = 5;
        clean.refused_files = 0;
        clean.phase = ElasticMoverPhase::Complete;
        let detail = mover_to_protocol(&clean).detail;
        assert!(!detail.contains("utknięte rekordy"), "{detail}");
        assert!(!detail.contains("wyparto"), "{detail}");
        let mut summarised = run.clone();
        summarised.stuck_hidden = 2;
        assert!(
            mover_to_protocol(&summarised).detail.contains("utknięte rekordy: 1 (+2 bez pełnego rekordu)"),
            "{}",
            mover_to_protocol(&summarised).detail
        );
        assert!(wire.detail.contains(MOVER_TEST_OPERATION), "{}", wire.detail);
    }

    #[test]
    fn observed_protocol_reads_mover_from_persisted_array_spec() {
        let conn = rusqlite::Connection::open_in_memory().expect("połączenie testowe");
        store::migrate(&conn).expect("migracje testowe");
        let db = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let spec = create_spec("mover-db");
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::new_v4().to_string(),
            kind: "elastic_create".into(),
            subject: spec.name.clone(),
            status: "queued".into(),
            started_by: "test".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&db, &job, Some(&jobs::ElasticJobIntent::Create(spec.clone())))
            .expect("rezerwacja testowa");
        let row = store::elastic_array(&db, &spec.owner, &spec.name)
            .expect("odczyt macierzy")
            .expect("utrwalona macierz");
        let persisted = row.persisted_spec().expect("utrwalona specyfikacja");
        assert_eq!(persisted, &spec);
        let mut result = ready_result(persisted);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::Complete,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:01:00Z".into()),
            moved_files: 1,
            moved_bytes: 64,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: true,
            detail: None,
            coupled_sync: Some(tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: MOVER_TEST_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T01:00:30Z".into(),
                finished_at: Some("2026-09-08T01:00:31Z".into()),
                outcome: ElasticSnapraidOutcome::Succeeded,
                exit_code: Some(0),
                total_blocks: Some(0),
                checked_blocks: None,
                accessed_mb: None,
                errors_file: None,
                errors_io: None,
                errors_data: None,
                detail: None,
            }),
        });
        validate_observation(persisted, &result).expect("poprawny wynik z DB");
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(wire.mover.last_run.expect("wynik movera").outcome, "ok");
    }

    #[test]
    fn observation_rejects_complete_mover_with_reported_sync_error() {
        let spec = create_spec("mover-error-count");
        let mut result = ready_result(&spec);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::Complete,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:01:00Z".into()),
            moved_files: 1,
            moved_bytes: 64,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: true,
            detail: None,
            coupled_sync: Some(tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: MOVER_TEST_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T01:00:30Z".into(),
                finished_at: Some("2026-09-08T01:00:31Z".into()),
                outcome: ElasticSnapraidOutcome::Succeeded,
                exit_code: Some(0),
                total_blocks: Some(1),
                checked_blocks: None,
                accessed_mb: None,
                errors_file: Some(0),
                errors_io: Some(0),
                errors_data: Some(1),
                detail: None,
            }),
        });
        assert!(validate_observation(&spec, &result).is_err());
    }

    #[test]
    fn observation_rejects_mover_with_malformed_operation_uuid() {
        let spec = create_spec("mover-invalid");
        let mut result = ready_result(&spec);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: "not-an-uuid".into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::Moving,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: None,
            moved_files: 0,
            moved_bytes: 0,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: false,
            detail: None,
            coupled_sync: None,
        });
        assert!(validate_observation(&spec, &result).is_err());
    }

    #[test]
    fn observation_rejects_mover_resume_uuid_equal_to_create() {
        let spec = create_spec("mover-foreign");
        let mut result = ready_result(&spec);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: spec.operation_id.clone(),
            phase: ElasticMoverPhase::Moving,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: None,
            moved_files: 0,
            moved_bytes: 0,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: false,
            detail: None,
            coupled_sync: None,
        });
        assert!(validate_observation(&spec, &result).is_err());
    }

    #[test]
    fn observation_accepts_distinct_mover_operation_uuid() {
        let spec = create_spec("mover-foreign-operation");
        let mut result = ready_result(&spec);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::Moving,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: None,
            moved_files: 0,
            moved_bytes: 0,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: false,
            detail: None,
            coupled_sync: None,
        });
        validate_observation(&spec, &result).expect("odrębna operacja movera jest legalna");
    }

    const MOVER_TEST_OPERATION: &str = "a1a1a1a1-a1a1-4a1a-8a1a-a1a1a1a1a1a1";

    #[test]
    fn observation_requires_the_coupled_sync_of_the_same_operation() {
        let spec = create_spec("mover-foreign-sync");
        let mut result = ready_result(&spec);
        result.stage = ElasticStage::NeedsAttention;
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::NeedsAttention,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: None,
            moved_files: 1,
            moved_bytes: 64,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: true,
            detail: None,
            coupled_sync: Some(tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: uuid::Uuid::new_v4().to_string(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T01:00:30Z".into(),
                finished_at: Some("2026-09-08T01:00:31Z".into()),
                outcome: ElasticSnapraidOutcome::Failed,
                exit_code: Some(1),
                total_blocks: None,
                checked_blocks: None,
                accessed_mb: None,
                errors_file: None,
                errors_io: None,
                errors_data: None,
                detail: Some("sync failed".into()),
            }),
        });
        assert!(validate_observation(&spec, &result).is_err(), "Sync obcej operacji");
        result.last_mover.as_mut().unwrap().coupled_sync.as_mut().unwrap().operation_id =
            MOVER_TEST_OPERATION.into();
        validate_observation(&spec, &result).expect("Sync tej samej operacji");
    }

    #[test]
    fn stale_parity_from_the_helper_reaches_protection_as_unsynced_bytes() {
        let spec = create_spec("mover-stale");
        let mut row = array();
        row.create_spec = Some(spec.clone());
        // Moving only empty files leaves zero bytes, yet parity is still stale.
        for bytes in [4096, 0] {
            let mut result = ready_result(&spec);
            result.stale_parity_bytes = Some(bytes);
            result.parity_stale = true;
            validate_observation(&spec, &result).expect("nieaktualna parity");
            let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result.clone()), &[]);
            assert_eq!(wire.protection.moved_unsynced_bytes, Some(bytes));
            assert_ne!(wire.protection.status, "protected", "{bytes} B");
            let mut without_parity = spec.clone();
            without_parity.parity.clear();
            assert!(validate_observation(&without_parity, &result).is_err());
        }
        // Where every other measurement is complete, the verdict alone decides:
        // zero unsynced bytes read as protected only while parity is current.
        let a = array();
        let mut verified = all_mounted(&a, 0);
        verified.moved_unsynced_bytes = Some(0);
        assert_eq!(protection(&a, &verified).status, "protected");
        verified.parity_stale = true;
        assert_eq!(protection(&a, &verified).status, "window_open");
        // A count without the verdict is inconsistent, never "protected".
        let mut unflagged = ready_result(&spec);
        unflagged.stale_parity_bytes = Some(0);
        assert!(validate_observation(&spec, &unflagged).is_err());
    }

    #[test]
    fn mover_outcome_separates_partial_held_and_closed_runs() {
        let spec = create_spec("mover-outcomes");
        let run = |phase, finished: Option<&str>, skipped, refused| ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: finished.map(str::to_string),
            moved_files: 3,
            moved_bytes: 96,
            skipped_files: skipped,
            skipped_bytes: skipped * 8,
            refused_files: refused,
            issues: (0..refused)
                .map(|index| tentanas_helper::elastic::ElasticMoverIssue {
                    path: format!("foto/link-{index}.jpg"),
                    kind: ElasticMoverIssueKind::Refused,
                    reason: "mover odmawia symlinku".into(),
                })
                .collect(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: true,
            detail: None,
            coupled_sync: None,
        };
        let cases = [
            (run(ElasticMoverPhase::Moving, None, 0, 0), "running"),
            (run(ElasticMoverPhase::NeedsAttention, None, 0, 0), "needs_attention"),
            (run(ElasticMoverPhase::NeedsAttention, Some("2026-09-08T01:05:00Z"), 0, 0), "failed"),
            (run(ElasticMoverPhase::Complete, Some("2026-09-08T01:05:00Z"), 0, 0), "ok"),
            (run(ElasticMoverPhase::Complete, Some("2026-09-08T01:05:00Z"), 1, 0), "partial"),
            (run(ElasticMoverPhase::Complete, Some("2026-09-08T01:05:00Z"), 0, 2), "partial"),
        ];
        for (mover, outcome) in cases {
            let mut result = ready_result(&spec);
            result.stage = ElasticStage::NeedsAttention;
            result.last_mover = Some(mover.clone());
            validate_observation(&spec, &result).expect("typed mover outcome");
            let wire = mover_to_protocol(&mover);
            assert_eq!(wire.outcome, outcome, "{:?}", mover.phase);
            if mover.refused_files > 0 {
                assert!(wire.detail.contains("odmówiono 2"), "{}", wire.detail);
                assert!(wire.detail.contains("foto/link-0.jpg: mover odmawia symlinku"), "{}", wire.detail);
            } else {
                assert!(wire.detail.is_empty(), "{}", wire.detail);
            }
        }
        let mut overreported = run(ElasticMoverPhase::Complete, Some("2026-09-08T01:05:00Z"), 0, 1);
        overreported.refused_files = 0;
        let mut result = ready_result(&spec);
        result.last_mover = Some(overreported);
        assert!(validate_observation(&spec, &result).is_err());
    }

    #[test]
    fn observation_rejects_complete_mover_after_failed_sync() {
        let spec = create_spec("mover-sync-failure");
        let mut result = ready_result(&spec);
        result.last_mover = Some(ElasticMoverRun {
            operation_id: MOVER_TEST_OPERATION.into(),
            resume_operation_id: uuid::Uuid::new_v4().to_string(),
            phase: ElasticMoverPhase::Complete,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:01:00Z".into()),
            moved_files: 1,
            moved_bytes: 64,
            skipped_files: 0,
            skipped_bytes: 0,
            refused_files: 0,
            issues: Vec::new(),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
            counts_known: true,
            detail: None,
            coupled_sync: Some(tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: MOVER_TEST_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T01:00:30Z".into(),
                finished_at: Some("2026-09-08T01:00:31Z".into()),
                outcome: ElasticSnapraidOutcome::Failed,
                exit_code: Some(1),
                total_blocks: None,
                checked_blocks: None,
                accessed_mb: None,
                errors_file: None,
                errors_io: None,
                errors_data: None,
                detail: Some("sync failed".into()),
            }),
        });
        assert!(validate_observation(&spec, &result).is_err());
    }

    pub(crate) fn snapraid_result(
        spec: &ElasticCreateSpec,
        operation_id: &str,
        kind: ElasticSnapraidKind,
        outcome: ElasticSnapraidOutcome,
    ) -> ElasticSnapraidResult {
        let mut state = ready_result(spec);
        let success = outcome == ElasticSnapraidOutcome::Succeeded;
        let refused = outcome == ElasticSnapraidOutcome::Refused;
        let scrubbing = kind == ElasticSnapraidKind::Scrub;
        let syncing = kind == ElasticSnapraidKind::Sync;
        let run = tentanas_helper::elastic::ElasticSnapraidRun {
            operation_id: operation_id.into(),
            kind,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:01:00Z".into()),
            outcome,
            exit_code: (!refused).then_some(if success { 0 } else { 1 }),
            total_blocks: success.then_some(80),
            checked_blocks: (success && scrubbing).then_some(68),
            accessed_mb: (success && scrubbing).then_some(17),
            errors_file: (!refused).then_some(0),
            errors_io: (!refused).then_some(0),
            errors_data: (!refused).then_some(if success { 0 } else { 1 }),
            detail: (!success).then(|| {
                if refused {
                    "unsynced_changes"
                } else {
                    "data_error"
                }
                .into()
            }),
        };
        if !refused {
            state.last_run = Some(run.clone());
        }
        if !success && !refused {
            state.stage = ElasticStage::NeedsAttention;
            state.detail = run.detail.clone();
        }
        if success && syncing {
            state.sync_completed_at = run.finished_at.clone();
        }
        ElasticSnapraidResult { state, run }
    }

    /// A spec with a cache branch — the only shape a mover run makes sense on.
    pub(crate) fn mover_spec(name: &str) -> ElasticCreateSpec {
        let mut spec = create_spec(name);
        spec.cache = Some(tentanas_helper::elastic::ElasticDiskSpec {
            disk_id: format!("{name}-cache"),
            wwn: Some(format!("wwn-{name}-cache")),
            serial: Some(format!("serial-{name}-cache")),
            bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        });
        spec.validate().expect("specyfikacja z cache");
        spec
    }

    pub(crate) fn mover_result(
        spec: &ElasticCreateSpec,
        operation_id: &str,
        resume_operation_id: &str,
        phase: ElasticMoverPhase,
        counts_known: bool,
    ) -> ElasticMoverResult {
        let mut state = ready_result(spec);
        let complete = phase == ElasticMoverPhase::Complete;
        let run = ElasticMoverRun {
            operation_id: operation_id.into(),
            resume_operation_id: resume_operation_id.into(),
            phase,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:06:00Z".into()),
            moved_files: 7,
            moved_bytes: 42 * 1024 * 1024 * 1024,
            skipped_files: if counts_known { 2 } else { 0 },
            skipped_bytes: if counts_known { 64 } else { 0 },
            refused_files: 0,
            issues: Vec::new(),
            counts_known,
            detail: (!complete).then(|| "przerwany transfer".to_string()),
            coupled_sync: Some(tentanas_helper::elastic::ElasticSnapraidRun {
                operation_id: operation_id.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T01:00:01Z".into(),
                finished_at: Some("2026-09-08T01:05:00Z".into()),
                outcome: if complete {
                    ElasticSnapraidOutcome::Succeeded
                } else {
                    ElasticSnapraidOutcome::Failed
                },
                exit_code: Some(i32::from(!complete)),
                total_blocks: None,
                checked_blocks: None,
                accessed_mb: None,
                errors_file: Some(0),
                errors_io: Some(0),
                errors_data: Some(u64::from(!complete)),
                detail: None,
            }),
            stuck_records: Vec::new(),
            stuck_hidden: 0,
            stuck_evicted: 0,
        };
        if !complete {
            state.stage = ElasticStage::NeedsAttention;
        }
        state.last_mover = Some(run.clone());
        ElasticMoverResult { state, run }
    }

    #[test]
    fn mover_validator_binds_the_answer_to_its_job_and_still_applies_the_shared_rules() {
        let spec = mover_spec("mover-validate");
        let operation_id = uuid::Uuid::new_v4().to_string();
        let resume_id = uuid::Uuid::new_v4().to_string();
        let good = mover_result(
            &spec,
            &operation_id,
            &resume_id,
            ElasticMoverPhase::Complete,
            true,
        );
        validate_mover_result(&spec, &operation_id, &good).expect("poprawny wynik movera");

        // The job's own operation is what binds the answer: the PREVIOUS run's
        // result, which the helper still reports as `last_mover`, must not be
        // able to close this job.
        let foreign = uuid::Uuid::new_v4().to_string();
        assert!(validate_mover_result(&spec, &foreign, &good).is_err());

        // The run must BE the array's last mover, not merely resemble it.
        let mut detached = good.clone();
        detached.state.last_mover = None;
        assert!(validate_mover_result(&spec, &operation_id, &detached).is_err());

        // The shared observation rules, reached through this entry point.
        let mut shared_resume = good.clone();
        shared_resume.run.resume_operation_id = operation_id.clone();
        shared_resume.state.last_mover = Some(shared_resume.run.clone());
        assert!(validate_mover_result(&spec, &operation_id, &shared_resume).is_err());

        let mut create_resume = good.clone();
        create_resume.run.resume_operation_id = spec.operation_id.clone();
        create_resume.state.last_mover = Some(create_resume.run.clone());
        assert!(validate_mover_result(&spec, &operation_id, &create_resume).is_err());

        let mut inverted = good.clone();
        inverted.run.finished_at = Some("2026-09-08T00:00:00Z".into());
        inverted.state.last_mover = Some(inverted.run.clone());
        assert!(validate_mover_result(&spec, &operation_id, &inverted).is_err());

        // A completed run whose coupled sync failed did not complete.
        let mut bad_sync = good.clone();
        bad_sync.run.coupled_sync.as_mut().unwrap().outcome = ElasticSnapraidOutcome::Failed;
        bad_sync.state.last_mover = Some(bad_sync.run.clone());
        assert!(validate_mover_result(&spec, &operation_id, &bad_sync).is_err());

        // Still moving is not a verdict.
        let mut moving = good.clone();
        moving.run.phase = ElasticMoverPhase::Moving;
        moving.run.finished_at = None;
        moving.state.last_mover = Some(moving.run.clone());
        assert!(validate_mover_result(&spec, &operation_id, &moving).is_err());
    }

    #[tokio::test]
    async fn spawn_mover_reserves_a_separate_resume_and_persists_the_rules_it_will_run() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        let db = Arc::new(crate::db::Db::from_connection(conn));
        let spec = mover_spec("mover-spawn");
        let create = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&db, &create, Some(&jobs::ElasticJobIntent::Create(spec.clone()))).unwrap();
        store::finish_elastic_operation(&db, &spec.owner, &spec.operation_id, Ok(&ready_result(&spec)))
            .unwrap();
        store::finish_job(&db, &create.job_id, "succeeded", None).unwrap();

        let mut row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        row.folders = vec![
            FolderRow { name: "foto".into(), cache_policy: "only".into(), ..Default::default() },
            FolderRow { name: "backup".into(), cache_policy: "no".into(), ..Default::default() },
            FolderRow { name: "filmy".into(), cache_policy: "yes".into(), ..Default::default() },
        ];
        row.mover = MoverConfig {
            min_age_secs: 3600,
            cache_min_free_pct: 25,
            coupled_sync: true,
            ..Default::default()
        };
        let job = spawn_mover(&db, &row, "test", None).unwrap();
        assert_eq!(job.kind, "elastic_mover");
        // The row is written before the body runs, so the reservation is
        // readable without waiting for a helper that is not there.
        let request: String = db
            .read()
            .unwrap()
            .query_row(
                "SELECT request_json FROM nas_elastic_operations WHERE kind='mover'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let HelperCommand::ElasticMover {
            operation_id, resume_operation_id, rules, coupled_sync, array_id, ..
        } = serde_json::from_str(&request).unwrap() else {
            panic!("zapisano inną intencję niż mover")
        };
        assert_eq!(array_id, spec.array_id);
        // Three distinct reservations: the run, its resume, and Create.
        assert_ne!(operation_id, resume_operation_id);
        assert_ne!(resume_operation_id, spec.operation_id);
        assert_ne!(operation_id, spec.operation_id);
        // The folders' cache policies ARE the rules — nothing rebuilds them later.
        assert_eq!(rules.pinned_folders, vec!["foto".to_string()]);
        assert_eq!(rules.eager_folders, vec!["backup".to_string()]);
        assert_eq!(rules.min_age_secs, 3600);
        assert_eq!(rules.min_free_pct, 25);
        assert!(rules.skip_open_files, "§5.3: otwarte pliki zawsze pomijane");
        assert!(coupled_sync);
    }

    /// An array created and settled in this database, the way every job test
    /// here starts.
    fn settled_database(spec: &ElasticCreateSpec) -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        let db = Arc::new(crate::db::Db::from_connection(conn));
        let create = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&db, &create, Some(&jobs::ElasticJobIntent::Create(spec.clone()))).unwrap();
        store::finish_elastic_operation(&db, &spec.owner, &spec.operation_id, Ok(&ready_result(spec))).unwrap();
        store::finish_job(&db, &create.job_id, "succeeded", None).unwrap();
        db
    }

    fn reply(value: &impl serde::Serialize) -> Result<super::super::broker::CommandOutput> {
        Ok(super::super::broker::CommandOutput { code: 0, stdout: serde_json::to_string(value).unwrap(), stderr: String::new() })
    }

    /// M4 of the 2026-09-17 review: the age probe walks a cache under the
    /// helper's lock, and a manual run started meanwhile is refused "busy"
    /// before it does anything. That refusal is a failed job, not an
    /// unresolved operation: the array stays active and the run can be asked
    /// again at once.
    #[tokio::test]
    async fn a_run_the_helper_refused_as_busy_leaves_nothing_unresolved() {
        let busy = format!(
            "elastic_mover exited with 69: tentanas-helper: elastic_mover: {} Resource temporarily unavailable (os error 11)",
            tentanas_helper::elastic::ELASTIC_BUSY
        );
        for kind in ["mover", "sync", "restore"] {
            let spec = mover_spec("busy-run");
            let db = settled_database(&spec);
            let operation_id = uuid::Uuid::now_v7().to_string();
            let (completion, finished) = tokio::sync::oneshot::channel();
            let (work_spec, work_id, error) = (spec.clone(), operation_id.clone(), busy.clone());
            let intent = match kind {
                "mover" => jobs::ElasticJobIntent::Mover {
                    owner: spec.owner.clone(),
                    array_id: spec.array_id.clone(),
                    operation_id: operation_id.clone(),
                    resume_operation_id: uuid::Uuid::now_v7().to_string(),
                    rules: MoverRules::default(),
                    coupled_sync: true,
                },
                "sync" => jobs::ElasticJobIntent::Snapraid {
                    owner: spec.owner.clone(),
                    array_id: spec.array_id.clone(),
                    operation_id: operation_id.clone(),
                    kind: ElasticSnapraidKind::Sync,
                    acknowledge_parity_fault: None,
                },
                _ => jobs::ElasticJobIntent::Restore {
                    owner: spec.owner.clone(),
                    array_id: spec.array_id.clone(),
                    operation_id: operation_id.clone(),
                },
            };
            jobs::spawn(&db, &format!("elastic_{kind}"), &spec.name, "test", Some(intent), Some(completion), move |h| async move {
                let run = async move { Err::<super::super::broker::CommandOutput, _>(anyhow!(error)) };
                match kind {
                    "mover" => execute_mover_job(&h, &work_spec, &work_id, run).await,
                    "sync" => execute_snapraid_job(&h, &work_spec, &work_id, &ElasticSnapraidKind::Sync, run).await,
                    _ => execute_job(&h, work_spec, work_id, run).await,
                }
            })
            .unwrap();
            let outcome = tokio::time::timeout(Duration::from_secs(2), finished).await.unwrap().unwrap();
            assert!(outcome.is_err(), "{kind}");
            let array = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
            assert_eq!(array.state, "active", "{kind}");
            assert!(!array.unresolved_operation, "{kind}");
            let state: String = db
                .read()
                .unwrap()
                .query_row("SELECT state FROM nas_elastic_operations WHERE operation_id=?1", [&operation_id], |r| r.get(0))
                .unwrap();
            assert_eq!(state, "failed", "{kind}");
        }
    }

    /// Files changing under the coupled Sync complete the run: the operation
    /// succeeds, the array stays active, nothing is left unresolved for the
    /// stuck alert to blame, and the run reads as partial, not failed.
    #[tokio::test]
    async fn a_mover_whose_sync_met_changing_files_completes_and_blames_no_operation() {
        let spec = mover_spec("partial-mover");
        let db = settled_database(&spec);
        let (operation_id, resume_id) = (uuid::Uuid::now_v7().to_string(), uuid::Uuid::now_v7().to_string());
        let mut result = mover_result(&spec, &operation_id, &resume_id, ElasticMoverPhase::Complete, true);
        {
            let sync = result.run.coupled_sync.as_mut().unwrap();
            sync.outcome = ElasticSnapraidOutcome::Partial;
            sync.exit_code = Some(1);
            sync.errors_file = Some(3);
            sync.detail = Some("pliki zmieniały się podczas Sync; parity obejmie je następny Sync".into());
        }
        result.state.stale_parity_bytes = Some(42);
        result.state.parity_stale = true;
        result.state.last_mover = Some(result.run.clone());
        validate_mover_result(&spec, &operation_id, &result).expect("a partial Sync completes a run");
        let mut unmarked = result.clone();
        unmarked.state.stale_parity_bytes = None;
        unmarked.state.parity_stale = false;
        assert!(validate_mover_result(&spec, &operation_id, &unmarked).is_err(), "parity must stay marked stale");
        let (completion, finished) = tokio::sync::oneshot::channel();
        let (work_spec, work_id, work_result) = (spec.clone(), operation_id.clone(), result.clone());
        jobs::spawn(
            &db,
            "elastic_mover",
            &spec.name,
            "test",
            Some(jobs::ElasticJobIntent::Mover {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: operation_id.clone(),
                resume_operation_id: resume_id.clone(),
                rules: MoverRules::default(),
                coupled_sync: true,
            }),
            Some(completion),
            move |h| async move { execute_mover_job(&h, &work_spec, &work_id, async { reply(&work_result) }).await },
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), finished).await.unwrap().unwrap().expect("job");
        let array = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(array.state, "active");
        assert!(!array.unresolved_operation, "nothing for the stuck alert to blame");
        assert_eq!(array.mover_history[0].outcome, "partial");
        assert_eq!(mover_to_protocol(&result.run).outcome, "partial");
        let CacheStuckVerdict::Raise(store::AlertText { detail, .. }) = cache_stuck_verdict(
            &array,
            &ElasticCacheAge { due_files: 0, due_bytes: 0, held_files: 1, oldest_secs: Some(10 * 86_400) },
            false,
        ) else {
            panic!("a file ten days old is past the threshold");
        };
        assert!(!detail.contains("nierozwiązana operacja"), "{detail}");
    }

    /// The manual Sync runs with the share writable too. One that met changing
    /// files is recorded as partial and succeeded: the array stays active, its
    /// file errors are not parity errors, and it is not the last complete Sync.
    #[tokio::test]
    async fn a_manual_sync_that_met_changing_files_is_partial_and_counts_no_parity_error() {
        let spec = create_spec("partial-sync");
        let db = settled_database(&spec);
        let operation_id = uuid::Uuid::now_v7().to_string();
        let mut result = snapraid_result(&spec, &operation_id, ElasticSnapraidKind::Sync, ElasticSnapraidOutcome::Succeeded);
        result.run.outcome = ElasticSnapraidOutcome::Partial;
        result.run.exit_code = Some(1);
        result.run.errors_file = Some(9815);
        result.run.detail = Some("pliki zmieniały się podczas Sync; parity obejmie je następny Sync".into());
        result.state.sync_completed_at = Some("2026-09-01T00:00:00Z".into());
        result.state.stale_parity_bytes = Some(0);
        result.state.parity_stale = true;
        result.state.last_run = Some(result.run.clone());
        validate_snapraid_result(&spec, &operation_id, &ElasticSnapraidKind::Sync, &result).expect("partial");
        let mut claimed = result.clone();
        claimed.state.sync_completed_at = claimed.run.finished_at.clone();
        assert!(validate_snapraid_result(&spec, &operation_id, &ElasticSnapraidKind::Sync, &claimed).is_err(),
            "a partial Sync writes no checkpoint");
        let mut io = result.clone();
        io.run.errors_io = Some(1);
        io.state.last_run = Some(io.run.clone());
        assert!(validate_snapraid_result(&spec, &operation_id, &ElasticSnapraidKind::Sync, &io).is_err());
        let (completion, finished) = tokio::sync::oneshot::channel();
        let (work_spec, work_id, work_result) = (spec.clone(), operation_id.clone(), result.clone());
        jobs::spawn(
            &db,
            "elastic_sync",
            &spec.name,
            "test",
            Some(jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: operation_id.clone(),
                kind: ElasticSnapraidKind::Sync,
                acknowledge_parity_fault: None,
            }),
            Some(completion),
            move |h| async move {
                execute_snapraid_job(&h, &work_spec, &work_id, &ElasticSnapraidKind::Sync, async { reply(&work_result) }).await
            },
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), finished).await.unwrap().unwrap().expect("job");
        let array = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(array.state, "active");
        assert!(!array.unresolved_operation);
        assert_eq!(array.snapraid_history[0].outcome, "partial");
        assert!(array.last_sync_run.is_none(), "a partial Sync is not the last complete one");
        assert_eq!(parity_errors_in_window(&array, chrono::Utc::now()), Some(0), "changed files are not parity errors");
    }

    #[tokio::test]
    async fn mover_job_closes_its_operation_and_stays_out_of_every_snapraid_query() {
        for case in ["complete", "attention", "malformed"] {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            store::migrate(&conn).unwrap();
            let db = Arc::new(crate::db::Db::from_connection(conn));
            let spec = mover_spec("mover-job");
            let create = tentaflow_protocol::tentanas::NasJob {
                job_id: uuid::Uuid::now_v7().to_string(),
                kind: "elastic_create".into(),
                subject: spec.name.clone(),
                status: "running".into(),
                started_at: store::now(),
                ..Default::default()
            };
            store::insert_job(&db, &create, Some(&jobs::ElasticJobIntent::Create(spec.clone())))
                .unwrap();
            store::finish_elastic_operation(
                &db,
                &spec.owner,
                &spec.operation_id,
                Ok(&ready_result(&spec)),
            )
            .unwrap();
            store::finish_job(&db, &create.job_id, "succeeded", None).unwrap();

            let operation_id = uuid::Uuid::now_v7().to_string();
            let resume_id = uuid::Uuid::now_v7().to_string();
            let (completion, finished) = tokio::sync::oneshot::channel();
            let work_spec = spec.clone();
            let work_id = operation_id.clone();
            let work_resume = resume_id.clone();
            let job = jobs::spawn(
                &db,
                "elastic_mover",
                &spec.name,
                "test",
                Some(jobs::ElasticJobIntent::Mover {
                    owner: spec.owner.clone(),
                    array_id: spec.array_id.clone(),
                    operation_id: operation_id.clone(),
                    resume_operation_id: resume_id.clone(),
                    rules: MoverRules {
                        min_age_secs: 7200,
                        min_free_pct: 20,
                        pinned_folders: Vec::new(),
                        eager_folders: Vec::new(),
                        skip_open_files: true,
                    },
                    coupled_sync: true,
                }),
                Some(completion),
                move |h| async move {
                    let phase = if case == "attention" {
                        ElasticMoverPhase::NeedsAttention
                    } else {
                        ElasticMoverPhase::Complete
                    };
                    let result = mover_result(
                        &work_spec,
                        &work_id,
                        &work_resume,
                        phase,
                        case != "attention",
                    );
                    execute_mover_job(&h, &work_spec, &work_id, async {
                        Ok(super::super::broker::CommandOutput {
                            code: 0,
                            stdout: if case == "malformed" {
                                "{".into()
                            } else {
                                serde_json::to_string(&result).unwrap()
                            },
                            stderr: String::new(),
                        })
                    })
                    .await
                },
            )
            .unwrap();
            // A mover carries an intent, so it is not cancellable.
            assert!(!jobs::cancel(&job.job_id), "{case}");
            let outcome = tokio::time::timeout(Duration::from_secs(2), finished)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(outcome.is_ok(), case == "complete", "{case}");
            let terminal = store::job(&db, &job.job_id).unwrap().unwrap();
            assert_eq!(
                terminal.status,
                if case == "complete" { "succeeded" } else { "failed" },
                "{case}"
            );
            let array = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
            assert_eq!(
                array.state,
                if case == "complete" { "active" } else { "needs_attention" },
                "{case}"
            );
            let state: String = db
                .read()
                .unwrap()
                .query_row(
                    "SELECT state FROM nas_elastic_operations WHERE kind='mover'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                state,
                if case == "complete" { "succeeded" } else { "needs_attention" },
                "{case}"
            );
            // THE LEAK THIS GUARDS: a 'mover' row must never be read as a
            // SnapRAID run — not in the display history, not as the last sync
            // or scrub, and not inside the parity-error window, where it would
            // turn an unmeasured window into a confident green zero.
            assert!(array.snapraid_history.is_empty(), "{case}");
            assert!(array.parity_window_runs.is_empty(), "{case}");
            assert!(array.last_sync_run.is_none(), "{case}");
            assert!(array.last_scrub_run.is_none(), "{case}");
            assert_eq!(parity_errors_in_window(&array, chrono::Utc::now()), None, "{case}");
        }
    }

    #[test]
    fn snapraid_validator_distinguishes_holes_refusal_failure_and_foreign_results() {
        let spec = create_spec("snapraid-result");
        let id = uuid::Uuid::new_v4().to_string();
        let good = snapraid_result(
            &spec,
            &id,
            ElasticSnapraidKind::Scrub,
            ElasticSnapraidOutcome::Succeeded,
        );
        validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &good).unwrap();
        for case in 0..8 {
            let mut bad = good.clone();
            match case {
                0 => bad.run.operation_id = uuid::Uuid::new_v4().to_string(),
                1 => bad.run.kind = ElasticSnapraidKind::Sync,
                2 => bad.state.owner.org_id = "foreign".into(),
                3 => bad.run.checked_blocks = Some(81),
                4 => bad.run.errors_data = None,
                5 => bad.run.finished_at = None,
                6 => bad.state.last_run = None,
                _ => bad.run.exit_code = Some(1),
            }
            assert!(
                validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &bad).is_err(),
                "{case}"
            );
        }
        let mut refused = snapraid_result(
            &spec,
            &id,
            ElasticSnapraidKind::Scrub,
            ElasticSnapraidOutcome::Refused,
        );
        refused.state.union_mounted = Some(false);
        validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &refused).unwrap();
        refused.run.checked_blocks = Some(0);
        assert!(
            validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &refused).is_err()
        );
        let failed = snapraid_result(
            &spec,
            &id,
            ElasticSnapraidKind::Scrub,
            ElasticSnapraidOutcome::Failed,
        );
        validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &failed).unwrap();
        let mut empty = snapraid_result(
            &spec,
            &id,
            ElasticSnapraidKind::Sync,
            ElasticSnapraidOutcome::Succeeded,
        );
        empty.run.total_blocks = Some(0);
        empty.run.errors_file = None;
        empty.run.errors_io = None;
        empty.run.errors_data = None;
        empty.state.last_run = Some(empty.run.clone());
        validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Sync, &empty).unwrap();
        let mut unchanged = snapraid_result(
            &spec,
            &id,
            ElasticSnapraidKind::Sync,
            ElasticSnapraidOutcome::Succeeded,
        );
        unchanged.run.total_blocks = None;
        unchanged.run.checked_blocks = None;
        unchanged.run.accessed_mb = None;
        unchanged.state.last_run = Some(unchanged.run.clone());
        validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Sync, &unchanged).unwrap();
        unchanged.run.errors_data = None;
        unchanged.state.last_run = Some(unchanged.run.clone());
        assert!(
            validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Sync, &unchanged).is_err()
        );
        empty.run.total_blocks = Some(1);
        empty.state.last_run = Some(empty.run.clone());
        assert!(validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Sync, &empty).is_err());
        let mut zero_scrub = good.clone();
        zero_scrub.run.checked_blocks = Some(0);
        zero_scrub.state.last_run = Some(zero_scrub.run.clone());
        assert!(
            validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &zero_scrub).is_err()
        );
        let mut sub_mb = good;
        sub_mb.run.accessed_mb = Some(0);
        sub_mb.state.last_run = Some(sub_mb.run.clone());
        validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &sub_mb).unwrap();
    }

    #[test]
    fn result_requires_complete_unique_roles_and_confirmed_ready() {
        let spec = create_spec("result");
        let good = ready_result(&spec);
        validate_result(&spec,&good).unwrap();
        for mutation in 0..8 {
            let mut bad = good.clone();
            match mutation {
                0 => bad.owner.org_id = "foreign".into(),
                1 => bad.operation_id = uuid::Uuid::new_v4().to_string(),
                2 => { bad.disks.pop(); },
                3 => bad.disks[1].role = ElasticRole::Data(1),
                4 => bad.disks[0].observed_uuid = Some(uuid::Uuid::new_v4().to_string()),
                5 => bad.disks[0].mounted = None,
                6 => bad.union_mounted = None,
                _ => bad.sync_completed_at = None,
            }
            assert!(validate_result(&spec,&bad).is_err(),"{mutation}");
        }
    }

    #[test]
    fn observation_validates_cache_presence_identity_and_role() {
        let mut spec = create_spec("cache-observation");
        spec.cache = Some(spec.data[0].clone());
        spec.cache.as_mut().unwrap().disk_id = "cache-observation".into();
        spec.cache.as_mut().unwrap().expected_uuid = uuid::Uuid::new_v4().to_string();
        spec.cache.as_mut().unwrap().wwn = Some("wwn-cache-observation".into());
        spec.cache.as_mut().unwrap().serial = Some("serial-cache-observation".into());
        spec.validate().unwrap();
        let good = ready_result(&spec);
        validate_result(&spec, &good).unwrap();

        let mut missing = good.clone();
        missing.disks.retain(|disk| disk.role != ElasticRole::Cache);
        assert!(validate_result(&spec, &missing).is_err());

        let mut wrong_uuid = good.clone();
        wrong_uuid.disks.iter_mut().find(|disk| disk.role == ElasticRole::Cache).unwrap().observed_uuid = Some(uuid::Uuid::new_v4().to_string());
        assert!(validate_result(&spec, &wrong_uuid).is_err());

        let mut wrong_filesystem = good.clone();
        wrong_filesystem.disks.iter_mut().find(|disk| disk.role == ElasticRole::Cache).unwrap().filesystem = Some("xfs".into());
        assert!(validate_result(&spec, &wrong_filesystem).is_err());

        let mut foreign_role = good;
        foreign_role.disks.iter_mut().find(|disk| disk.role == ElasticRole::Cache).unwrap().role = ElasticRole::Data(99);
        assert!(validate_result(&spec, &foreign_role).is_err());
    }

    fn observation_fixture() -> (ElasticCreateSpec, ElasticArrayRow, Vec<FeatureState>) {
        let spec = create_spec("observation");
        let row = ElasticArrayRow {
            name: spec.name.clone(),
            create_spec: Some(spec.clone()),
            enabled: true,
            state: "active".into(),
            filesystem: spec.filesystem.as_str().into(),
            branches: vec![BranchRow {
                disk_id: spec.data[0].disk_id.clone(),
                name: "d1".into(),
                role: "data".into(),
                ..Default::default()
            }],
            parity: vec![ParityRow {
                disk_id: spec.parity[0].disk_id.clone(),
                name: "p1".into(),
                index: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        let features = [MERGERFS_FEATURE_ID, SNAPRAID_FEATURE_ID]
            .into_iter()
            .map(|id| FeatureState {
                id: id.into(),
                status: "ok".into(),
                ..Default::default()
            })
            .collect();
        (spec, row, features)
    }

    #[test]
    fn observation_keeps_unmounted_ready_checkpoint_pending_but_not_committable() {
        let (spec, row, features) = observation_fixture();
        for unmounted_branch in [false, true] {
            let mut result = ready_result(&spec);
            result.union_mounted = Some(false);
            if unmounted_branch {
                result.disks[0].mounted = Some(false);
                result.disks[0].size_bytes = None;
                result.disks[0].used_bytes = None;
                result.disks[0].free_bytes = None;
            }
            assert!(validate_result(&spec, &result).is_err());
            let measured = validate_observation(&spec, &result).map(|()| result);
            assert!(measured.is_ok());
            let wire = observed_protocol(&row, &BTreeMap::new(), measured, &features);
            assert_eq!(wire.state, "pending");
            assert_eq!(wire.data_disks[0].mounted, Some(!unmounted_branch));
            assert_eq!(wire.data_disks[0].device_present, Some(true));
            assert_eq!(
                wire.data_disks[0].size_bytes,
                if unmounted_branch {
                    None
                } else {
                    Some(spec.data[0].bytes)
                }
            );
        }
    }

    /// n11 names each member by a LIVE kernel name: the inventory's for its
    /// `disk_id` first, then the one the helper resolved the member's device
    /// to in the same read — which is what names every member right after a
    /// core start, before the inventory's first pass. The slot and the
    /// by-uuid path are keys and never a name.
    #[test]
    fn a_member_is_named_by_the_inventory_then_by_the_helper_s_own_observation() {
        let (spec, row, features) = observation_fixture();
        let data_id = spec.data[0].disk_id.clone();
        let inventory: BTreeMap<String, NasDisk> = [(
            data_id.clone(),
            NasDisk { disk_id: data_id, name: "sdg".into(), ..Default::default() },
        )]
        .into_iter()
        .collect();
        let mut result = ready_result(&spec);
        result.disks[0].kernel_name = Some("sdz".into());
        let parity = result
            .disks
            .iter_mut()
            .find(|d| matches!(d.role, ElasticRole::Parity(_)))
            .expect("a parity observation");
        parity.kernel_name = Some("sdo".into());
        let wire = observed_protocol(&row, &inventory, Ok(result.clone()), &features);
        // The inventory wins where it has the disk…
        assert_eq!((wire.data_disks[0].name.as_str(), wire.data_disks[0].disk_name.as_str()), ("d1", "sdg"));
        // …and the helper's live observation names the one it does not hold.
        assert_eq!((wire.parity_disks[0].name.as_str(), wire.parity_disks[0].disk_name.as_str()), ("p1", "sdo"));
        assert_eq!(wire.parity_disks[0].device_present, Some(true));

        // An EMPTY inventory (a core that has just started): every present
        // member is still named, none reads as missing.
        let cold = observed_protocol(&row, &BTreeMap::new(), Ok(result.clone()), &features);
        assert_eq!(cold.data_disks[0].disk_name, "sdz");
        assert_eq!(cold.parity_disks[0].disk_name, "sdo");

        // A member the helper found no device for has no live name at all.
        let mut gone = result;
        gone.disks[0].kernel_name = None;
        gone.disks[0].device_present = Some(false);
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(gone), &features);
        assert_eq!(wire.data_disks[0].disk_name, "");
    }

    /// The third step of the naming rule: a member no live source names gets
    /// the name it was last seen under — in its OWN field, so the screen can
    /// say "last seen as", and never in `disk_name`.
    #[test]
    fn a_member_no_live_source_names_carries_its_last_known_name_apart() {
        let mut array = NasElasticArray {
            data_disks: vec![
                NasElasticBranch { disk_id: "wwn-live".into(), disk_name: "sdg".into(), ..Default::default() },
                NasElasticBranch { disk_id: "wwn-gone".into(), ..Default::default() },
            ],
            parity_disks: vec![NasElasticParity { disk_id: "wwn-never".into(), ..Default::default() }],
            ..Default::default()
        };
        fill_last_known_names(&mut array, |id| match id {
            "wwn-live" => panic!("a live member must not be looked up"),
            "wwn-gone" => Some("sdq".into()),
            _ => None,
        });
        assert_eq!((array.data_disks[0].disk_name.as_str(), array.data_disks[0].disk_last_name.as_str()), ("sdg", ""));
        assert_eq!((array.data_disks[1].disk_name.as_str(), array.data_disks[1].disk_last_name.as_str()), ("", "sdq"));
        assert_eq!((array.parity_disks[0].disk_name.as_str(), array.parity_disks[0].disk_last_name.as_str()), ("", ""));
    }

    #[test]
    fn observation_rejects_identity_roles_bounds_and_unproven_mounts() {
        let (spec, row, features) = observation_fixture();
        for mutation in 0..15 {
            let mut result = ready_result(&spec);
            match mutation {
                0 => result.owner.org_id = "foreign".into(),
                1 => result.array_id = uuid::Uuid::new_v4().to_string(),
                2 => result.operation_id = uuid::Uuid::new_v4().to_string(),
                3 => result.disks[1].role = ElasticRole::Data(1),
                4 => {
                    result.disks.pop();
                }
                5 => result.disks[0].role = ElasticRole::Data(0),
                6 => result.disks[0].observed_uuid = Some(uuid::Uuid::new_v4().to_string()),
                7 => result.disks[0].filesystem = Some("xfs".into()),
                8 => result.disks[0].used_bytes = Some(spec.data[0].bytes + 1),
                9 => result.disks[0].free_bytes = Some(spec.data[0].bytes + 1),
                10 => result.disks[0].observed_uuid = None,
                11 => result.disks[0].filesystem = None,
                12 => result.disks[0].device_present = Some(false),
                13 => result.disks[0].device_present = None,
                _ => {
                    result.disks[0].mounted = Some(false);
                    result.disks[0].observed_uuid = None;
                }
            }
            assert!(validate_result(&spec, &result).is_err(), "{mutation}");
            let measured = validate_observation(&spec, &result).map(|()| result);
            assert!(measured.is_err(), "{mutation}");
            let wire = observed_protocol(&row, &BTreeMap::new(), measured, &features);
            assert_eq!(wire.state, "unknown", "{mutation}");
            assert_eq!(wire.data_disks[0].mounted, None, "{mutation}");
        }
    }

    #[test]
    fn observation_preserves_unknown_measurement_and_local_operation_state() {
        let (spec, mut row, features) = observation_fixture();
        let mut result = ready_result(&spec);
        result.disks[0].mounted = None;
        result.disks[0].observed_uuid = None;
        result.disks[0].filesystem = None;
        let measured = validate_observation(&spec, &result).map(|()| result);
        assert!(measured.is_ok());
        let wire = observed_protocol(&row, &BTreeMap::new(), measured, &features);
        assert_eq!(wire.state, "unknown");
        assert_eq!(wire.data_disks[0].mounted, None);
        for state in ["creating", "needs_attention"] {
            row.state = state.into();
            let result = ready_result(&spec);
            let measured = validate_observation(&spec, &result).map(|()| result);
            assert_eq!(
                observed_protocol(&row, &BTreeMap::new(), measured, &features).state,
                state
            );
        }
    }

    #[test]
    fn service_state_requires_confirmed_rw_for_ready() {
        let (spec, _, _) = observation_fixture();
        let mut result = ready_result(&spec);
        let mut service = |mode, pending, readonly| {
            result.service = Some(tentanas_helper::elastic::ElasticServiceState {
                mode,
                operation_id: uuid::Uuid::new_v4().to_string(),
                pending,
            });
            result.union_readonly = readonly;
            validate_result(&spec, &result).is_ok()
        };
        assert!(!service(ElasticServiceMode::Hold, false, Some(true)));
        assert!(!service(ElasticServiceMode::Hold, false, Some(false)));
        assert!(!service(ElasticServiceMode::Hold, false, None));
        assert!(!service(ElasticServiceMode::Online, true, Some(false)));
        assert!(!service(ElasticServiceMode::Online, false, None));
        assert!(service(ElasticServiceMode::Online, false, Some(false)));
    }

    #[test]
    fn service_state_rejects_invalid_or_create_operation_id() {
        let (spec, _, _) = observation_fixture();
        for operation_id in ["not-a-uuid".to_string(), spec.operation_id.clone()] {
            let mut result = ready_result(&spec);
            result.service = Some(tentanas_helper::elastic::ElasticServiceState {
                mode: ElasticServiceMode::Online,
                operation_id,
                pending: false,
            });
            result.union_readonly = Some(false);
            assert!(validate_observation(&spec, &result).is_err());
        }
    }

    #[test]
    fn legacy_ready_without_service_remains_valid() {
        let (spec, _, _) = observation_fixture();
        assert!(validate_result(&spec, &ready_result(&spec)).is_ok());
    }

    #[test]
    fn observed_protocol_does_not_publish_active_for_unconfirmed_service() {
        let (spec, row, features) = observation_fixture();
        let mut result = ready_result(&spec);
        result.service = Some(tentanas_helper::elastic::ElasticServiceState {
            mode: ElasticServiceMode::Hold,
            operation_id: uuid::Uuid::new_v4().to_string(),
            pending: false,
        });
        result.union_readonly = Some(false);
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(result), &features);
        assert_ne!(wire.state, "active");
    }

    #[test]
    fn observed_protocol_publishes_active_for_confirmed_online_rw_service() {
        let (spec, row, features) = observation_fixture();
        let mut result = ready_result(&spec);
        result.service = Some(tentanas_helper::elastic::ElasticServiceState {
            mode: ElasticServiceMode::Online,
            operation_id: uuid::Uuid::new_v4().to_string(),
            pending: false,
        });
        result.union_readonly = Some(false);
        let measured = validate_observation(&spec, &result).map(|()| result);
        let wire = observed_protocol(&row, &BTreeMap::new(), measured, &features);
        assert_eq!(wire.state, "active");
    }

    #[test]
    fn observation_and_mutation_keep_parity_sync_contract() {
        let (mut spec, mut row, features) = observation_fixture();
        for parity in [true, false] {
            if !parity {
                spec.parity.clear();
                row.parity.clear();
                row.create_spec = Some(spec.clone());
            }
            let mut result = ready_result(&spec);
            if !parity {
                result.sync_completed_at = None;
            }
            validate_result(&spec, &result).unwrap();
            let measured = validate_observation(&spec, &result).map(|()| result.clone());
            assert_eq!(
                observed_protocol(&row, &BTreeMap::new(), measured, &features).state,
                "active"
            );
            if parity {
                result.sync_completed_at = None;
                assert!(validate_result(&spec, &result).is_err());
            } else {
                result.sync_completed_at = Some(store::now());
                assert!(validate_observation(&spec, &result).is_err());
                assert!(validate_result(&spec, &result).is_err());
            }
            result.sync_completed_at = Some("invalid-date".into());
            assert!(validate_observation(&spec, &result).is_err());
        }
    }

    #[tokio::test]
    async fn startup_rows_wait_for_terminal_jobs_and_serialize_different_owners() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        let db = Arc::new(crate::db::Db::from_connection(conn));
        let row = |name: &str, owner: &str| {
            let mut spec = create_spec(name);
            spec.owner.addon_id = owner.into();
            ElasticArrayRow {
                name: name.into(),
                create_spec: Some(spec),
                ..Default::default()
            }
        };
        let rows = vec![
            row("startup-first", "owner-a"),
            row("startup-second", "owner-a"),
        ];
        let other_rows = vec![row("startup-other", "owner-b")];
        let (entered, entering) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let previous = Arc::new(Mutex::new(None::<String>));
        let first_db = db.clone();
        let first_previous = previous.clone();
        let first = tokio::spawn(async move {
            let mut first_gate = Some((entered, released));
            restore_startup_rows(&rows, |row, completion| {
                if let Some(id) = first_previous.lock().unwrap().as_ref() {
                    assert_eq!(store::job(&first_db, id)?.unwrap().status, "succeeded");
                    assert!(!jobs::running().lock().unwrap().contains_key(id));
                }
                let gate = first_gate.take();
                let spec = row.persisted_spec()?.clone();
                let work = spec.clone();
                let job = jobs::spawn(
                    &first_db,
                    "elastic_create",
                    &row.name,
                    "startup-test",
                    Some(jobs::ElasticJobIntent::Create(spec)),
                    Some(completion),
                    move |h| async move {
                        if let Some((entered, released)) = gate {
                            entered.send(()).unwrap();
                            released.await.unwrap();
                        }
                        store::finish_elastic_operation(
                            h.db(),
                            &work.owner,
                            &work.operation_id,
                            Ok(&ready_result(&work)),
                        )
                    },
                )?;
                *first_previous.lock().unwrap() = Some(job.job_id.clone());
                Ok(job)
            })
            .await
        });
        entering.await.unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let other = restore_startup_rows(&other_rows, |row, completion| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let id = previous.lock().unwrap().clone().unwrap();
            assert_eq!(store::job(&db, &id)?.unwrap().status, "succeeded");
            assert!(!jobs::running().lock().unwrap().contains_key(&id));
            let spec = row.persisted_spec()?.clone();
            let work = spec.clone();
            jobs::spawn(
                &db,
                "elastic_create",
                &row.name,
                "startup-other-test",
                Some(jobs::ElasticJobIntent::Create(spec)),
                Some(completion),
                move |h| async move {
                    store::finish_elastic_operation(
                        h.db(),
                        &work.owner,
                        &work.operation_id,
                        Ok(&ready_result(&work)),
                    )
                },
            )
        });
        tokio::pin!(other);
        assert!(futures::poll!(&mut other).is_pending());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(store::list_jobs(&db, 10).unwrap().len(), 1);
        release.send(()).unwrap();
        first.await.unwrap().unwrap();
        other.await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let jobs = store::list_jobs(&db, 10).unwrap();
        assert_eq!(jobs.len(), 3);
        assert!(jobs.iter().all(|job| job.status == "succeeded"));
        assert!(jobs
            .iter()
            .all(|job| !jobs::running().lock().unwrap().contains_key(&job.job_id)));
    }

    /// One array that cannot come back must not keep the arrays after it down:
    /// a body that fails, panics or cannot persist its end is recorded and the
    /// next array is still attempted, and the queue reports every failure.
    #[tokio::test]
    async fn startup_rows_attempt_every_array_after_a_failure_and_report_each() {
        for failure in 0..3 {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            store::migrate(&conn).unwrap();
            let db = Arc::new(crate::db::Db::from_connection(conn));
            let rows = vec![
                ElasticArrayRow {
                    name: "first".into(),
                    ..Default::default()
                },
                ElasticArrayRow {
                    name: "second".into(),
                    ..Default::default()
                },
            ];
            let mut calls = 0;
            let result = restore_startup_rows(&rows, |row, completion| {
                    calls += 1;
                    let first = row.name == "first";
                    jobs::spawn(&db, "startup-test", &row.name, "test", None, Some(completion), move |h| async move {
                        if !first {
                            return Ok(());
                        }
                        assert_ne!(failure, 1, "kontrolowana panika");
                        if failure == 2 {
                            h.db().write().unwrap().execute_batch(
                                "CREATE TRIGGER refuse_finish BEFORE UPDATE ON nas_jobs WHEN OLD.subject = 'first' BEGIN SELECT RAISE(ABORT,'persist denied'); END;")?;
                            return Ok(());
                        }
                        Err(anyhow!("kontrolowany błąd wykonawcy"))
                    })
                }).await;
            let error = result.expect_err("the failure is reported").to_string();
            assert!(error.starts_with("first: "), "{error}");
            assert!(!error.contains("second"), "{error}");
            assert_eq!(calls, 2, "failure={failure}: the second array is still attempted");
            let jobs = store::list_jobs(&db, 10).unwrap();
            assert_eq!(jobs.len(), 2);
            let status = |subject: &str| jobs.iter().find(|job| job.subject == subject).unwrap().status.clone();
            assert_eq!(status("first"), if failure == 2 { "running" } else { "failed" });
            assert_eq!(status("second"), "succeeded");
            assert!(jobs.iter().all(|job| !jobs::running().lock().unwrap().contains_key(&job.job_id)));
        }
    }

    /// A1 of the second review: a startup Restore the helper refused as busy
    /// ran nothing, and is asked again — a few times — instead of leaving the
    /// array down until an admin notices.
    #[tokio::test]
    async fn a_startup_restore_refused_as_busy_is_asked_again() {
        let rows = vec![ElasticArrayRow { name: "first".into(), ..Default::default() }];
        for busy_answers in [2u32, 99] {
            let mut calls = 0u32;
            let result = restore_startup_rows_waiting(&rows, Duration::ZERO, Duration::from_secs(60), |_, completion| {
                calls += 1;
                let answer = if calls <= busy_answers {
                    Err(anyhow!("elastic_restore exited with 69: {} Resource temporarily unavailable", tentanas_helper::elastic::ELASTIC_BUSY))
                } else {
                    Ok(())
                };
                completion.send(answer).unwrap();
                Ok(tentaflow_protocol::tentanas::NasJob::default())
            })
            .await;
            if busy_answers == 2 {
                result.expect("restored on the third attempt");
                assert_eq!(calls, 3);
            } else {
                assert!(result.expect_err("gives up").to_string().contains("busy"));
                assert_eq!(calls, STARTUP_BUSY_ATTEMPTS, "bounded");
            }
        }
        // Any other failure is not asked again.
        let mut calls = 0;
        let result = restore_startup_rows_waiting(&rows, Duration::ZERO, Duration::from_secs(60), |_, completion| {
            calls += 1;
            completion.send(Err(anyhow!("konfiguracja niezgodna z journalem"))).unwrap();
            Ok(tentaflow_protocol::tentanas::NasJob::default())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }

    /// W-C of the third review: a Restore that never answers is one array's
    /// failure, not the node's. The queue gives up on it, attempts the next
    /// array, and the scheduler's Elastic passes stop waiting for a queue that
    /// has been running too long — otherwise no Sync, Scrub or mover of any
    /// array runs for up to the 24 h job timeout and no cache is drained.
    #[tokio::test]
    async fn a_startup_restore_that_never_answers_is_bounded_and_so_is_the_gate() {
        let rows = vec![
            ElasticArrayRow { name: "hangs".into(), ..Default::default() },
            ElasticArrayRow { name: "second".into(), ..Default::default() },
        ];
        // The sender is KEPT: the job neither finishes nor is dropped.
        let mut held = Vec::new();
        let mut calls = Vec::new();
        let result = restore_startup_rows_waiting(&rows, Duration::ZERO, Duration::ZERO, |row, completion| {
            calls.push(row.name.clone());
            held.push(completion);
            Ok(tentaflow_protocol::tentanas::NasJob::default())
        })
        .await;
        let error = result.expect_err("the hanging array is reported").to_string();
        assert!(error.contains("hangs") && error.contains("nie odpowiedziało"), "{error}");
        assert_eq!(calls, vec!["hangs".to_string(), "second".to_string()], "the next array is still attempted");
        assert_eq!(held.len(), 2, "no completion was ever answered");
        // The gate: pending while the queue is young, open once it is too old.
        let pending = StartupRestores::begin();
        assert!(startup_restores_pending_for(Duration::from_secs(600)));
        assert!(!startup_restores_pending_for(Duration::ZERO), "a stuck queue does not hold the passes for ever");
        drop(pending);
        assert!(!startup_restores_pending_for(Duration::from_secs(600)));
    }

    /// A4 of the second review: the startup queue rewrites share configs after
    /// its restores, whatever they ended with, and while it still holds the
    /// scheduler's Elastic passes.
    #[tokio::test]
    async fn the_startup_queue_rewrites_share_configs_after_its_restores_while_the_passes_wait() {
        for restore_fails in [false, true] {
            let order = std::sync::Mutex::new(Vec::new());
            let pending = StartupRestores::begin();
            let result = restore_then_rewrite_shares(
                async {
                    order.lock().unwrap().push("restore");
                    if restore_fails { Err(anyhow!("restore")) } else { Ok(()) }
                },
                || async {
                    assert!(startup_restores_pending(), "the passes still wait");
                    order.lock().unwrap().push("shares");
                    Ok(Vec::new())
                },
            )
            .await;
            drop(pending);
            assert_eq!(result.is_err(), restore_fails);
            assert_eq!(*order.lock().unwrap(), vec!["restore", "shares"]);
        }
    }

    /// A job the store refuses for one array is that array's failure: the
    /// next array is attempted. Only a completion that never arrives stops the
    /// queue, because the job's end — and whether its helper still holds the
    /// node lock — is then unknown.
    #[tokio::test]
    async fn startup_rows_go_on_after_a_refused_job_and_stop_only_without_a_completion() {
        let rows = vec![
            ElasticArrayRow { name: "first".into(), ..Default::default() },
            ElasticArrayRow { name: "second".into(), ..Default::default() },
        ];
        for missing_completion in [false, true] {
            let mut calls = 0;
            let result = restore_startup_rows(&rows, |_, completion| {
                calls += 1;
                drop(completion);
                if missing_completion {
                    Ok(tentaflow_protocol::tentanas::NasJob::default())
                } else {
                    Err(anyhow!("Odmowa przyjęcia zadania"))
                }
            })
            .await;
            let error = result.expect_err("reported").to_string();
            if missing_completion {
                assert_eq!(calls, 1);
                assert!(error.contains("Brak potwierdzenia"), "{error}");
            } else {
                assert_eq!(calls, 2);
                assert!(error.contains("first: Odmowa") && error.contains("second: Odmowa"), "{error}");
            }
        }
    }

    /// C1 of the 2026-09-17 review, pinned against the helper's OWN admission
    /// rule rather than a faked answer: after a reboot the helper answers a
    /// private array only the commands `runs_after_a_boot` names, and the
    /// restore job — the startup one and "Przywróć" alike — sends exactly one
    /// command, which must be one of them. An Inspect sent first made every
    /// array fail at every startup.
    #[test]
    fn the_restore_job_sends_only_a_command_the_helper_runs_after_a_boot() {
        let spec = mover_spec("after-boot");
        let command = restore_command(&spec);
        assert!(tentanas_helper::elastic::runs_after_a_boot(&command), "{command:?}");
        assert!(!tentanas_helper::elastic::runs_after_a_boot(&HelperCommand::ElasticInspect {
            array_id: spec.array_id.clone(),
            owner: spec.owner.clone(),
        }));
    }

    /// The array becomes active after a mover only on a result that proves it
    /// serves read-write: a Complete run whose array still reports a Hold, or
    /// a read-only union, is not accepted.
    #[test]
    fn a_complete_mover_result_under_a_hold_or_a_readonly_union_is_refused() {
        let spec = mover_spec("held-result");
        let (operation_id, resume_id) = (uuid::Uuid::now_v7().to_string(), uuid::Uuid::now_v7().to_string());
        let mut clean = mover_result(&spec, &operation_id, &resume_id, ElasticMoverPhase::Complete, true);
        validate_mover_result(&spec, &operation_id, &clean).expect("serving");
        let mut held = clean.clone();
        held.state.stage = ElasticStage::NeedsAttention;
        held.state.service = Some(tentanas_helper::elastic::ElasticServiceState {
            mode: ElasticServiceMode::Hold,
            operation_id: operation_id.clone(),
            pending: false,
        });
        held.state.union_readonly = Some(true);
        assert!(validate_mover_result(&spec, &operation_id, &held).is_err());
        clean.state.service = Some(tentanas_helper::elastic::ElasticServiceState {
            mode: ElasticServiceMode::Online,
            operation_id: resume_id.clone(),
            pending: false,
        });
        clean.state.union_readonly = Some(true);
        assert!(validate_mover_result(&spec, &operation_id, &clean).is_err(), "a read-only union is no release");
        clean.state.union_readonly = Some(false);
        validate_mover_result(&spec, &operation_id, &clean).expect("released and serving");
    }

    #[tokio::test]
    async fn snapraid_real_job_keeps_terminal_history_atomic_and_is_not_cancellable() {
        for case in [
            "success",
            "refused",
            "failed",
            "malformed",
            "transport",
            "panic",
            "persist",
        ] {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            store::migrate(&conn).unwrap();
            let db = Arc::new(crate::db::Db::from_connection(conn));
            let spec = create_spec("maintenance");
            let create = tentaflow_protocol::tentanas::NasJob {
                job_id: uuid::Uuid::now_v7().to_string(),
                kind: "elastic_create".into(),
                subject: spec.name.clone(),
                status: "running".into(),
                started_at: store::now(),
                ..Default::default()
            };
            store::insert_job(
                &db,
                &create,
                Some(&jobs::ElasticJobIntent::Create(spec.clone())),
            )
            .unwrap();
            store::finish_elastic_operation(
                &db,
                &spec.owner,
                &spec.operation_id,
                Ok(&ready_result(&spec)),
            )
            .unwrap();
            store::finish_job(&db, &create.job_id, "succeeded", None).unwrap();
            let operation_id = uuid::Uuid::now_v7().to_string();
            let (release, held) = tokio::sync::oneshot::channel();
            let (completion, finished) = tokio::sync::oneshot::channel();
            let work_spec = spec.clone();
            let work_id = operation_id.clone();
            let job = jobs::spawn(
                &db,
                "elastic_sync",
                &spec.name,
                "test",
                Some(jobs::ElasticJobIntent::Snapraid {
                    owner: spec.owner.clone(),
                    array_id: spec.array_id.clone(),
                    operation_id: operation_id.clone(),
                    kind: ElasticSnapraidKind::Sync,
                    acknowledge_parity_fault: None,
                }),
                Some(completion),
                move |h| async move {
                    held.await.unwrap();
                    assert!(!h.cancelled());
                    if case == "panic" {
                        panic!("Kontrolowane przerwanie zadania");
                    }
                    let outcome = match case {
                        "refused" => ElasticSnapraidOutcome::Refused,
                        "failed" => ElasticSnapraidOutcome::Failed,
                        _ => ElasticSnapraidOutcome::Succeeded,
                    };
                    let result =
                        snapraid_result(&work_spec, &work_id, ElasticSnapraidKind::Sync, outcome);
                    execute_snapraid_job(
                        &h,
                        &work_spec,
                        &work_id,
                        &ElasticSnapraidKind::Sync,
                        async {
                            Ok(super::super::broker::CommandOutput {
                                code: if case == "transport" { 1 } else { 0 },
                                stdout: if case == "malformed" {
                                    "{".into()
                                } else {
                                    serde_json::to_string(&result).unwrap()
                                },
                                stderr: String::new(),
                            })
                        },
                    )
                    .await
                },
            )
            .unwrap();
            assert!(!jobs::cancel(&job.job_id));
            assert_eq!(
                store::job(&db, &job.job_id).unwrap().unwrap().status,
                "running"
            );
            if case == "persist" {
                db.write()
                    .unwrap()
                    .execute_batch(
                        "CREATE TRIGGER stop_terminal BEFORE UPDATE OF finished_at ON nas_jobs
                    BEGIN SELECT RAISE(ABORT,'odmowa finalizacji'); END;",
                    )
                    .unwrap();
            }
            release.send(()).unwrap();
            let outcome = tokio::time::timeout(Duration::from_secs(2), finished)
                .await
                .unwrap()
                .unwrap();
            assert!(!jobs::running().lock().unwrap().contains_key(&job.job_id));
            assert_eq!(outcome.is_ok(), case == "success", "{case}");
            let terminal = store::job(&db, &job.job_id).unwrap().unwrap();
            let array = store::elastic_array(&db, &spec.owner, &spec.name)
                .unwrap()
                .unwrap();
            assert_eq!(
                terminal.status,
                if case == "success" {
                    "succeeded"
                } else if case == "persist" {
                    "running"
                } else {
                    "failed"
                },
                "{case}"
            );
            assert_eq!(
                array.state,
                // Only a run the helper JUDGED and reported as failed takes the
                // array with it. A run with no result at all is interrupted and
                // leaves the array exactly as it was (W-D of the third review).
                if case == "failed" { "needs_attention" } else { "active" },
                "{case}"
            );
            let run = &array.snapraid_history[0];
            assert_eq!(run.job_id.as_deref(), Some(job.job_id.as_str()));
            assert_eq!(run.operation_id.as_deref(), Some(operation_id.as_str()));
            assert_eq!(
                run.outcome,
                match case {
                    "success" => "ok",
                    "refused" => "refused",
                    "failed" => "failed",
                    "persist" => "running",
                    // No confirmed result: interrupted, not a parity fault.
                    _ => store::INTERRUPTED_OUTCOME,
                },
                "{case}"
            );
            if matches!(
                case,
                "refused" | "malformed" | "transport" | "panic" | "persist"
            ) {
                assert!(run.errors.is_none());
            }
            if case == "success" {
                let wire =
                    observed_protocol(&array, &BTreeMap::new(), Ok(ready_result(&spec)), &[]);
                assert_eq!(wire.snapraid.history, array.snapraid_history);
                assert_eq!(
                    wire.snapraid.last_sync.as_ref().unwrap().job_id.as_deref(),
                    Some(job.job_id.as_str())
                );
                // A real, completed, clean sync inside the window IS the
                // measurement — this is the production path by which an array
                // becomes "protected", through a job rather than a fixture.
                assert_eq!(wire.snapraid.parity_errors, Some(0));
                assert_eq!(wire.protection.status, "protected", "{}", wire.protection.detail);
            }
            if case == "persist" {
                db.write()
                    .unwrap()
                    .execute_batch("DROP TRIGGER stop_terminal;")
                    .unwrap();
                store::fail_orphaned_jobs(&db).unwrap();
                let reopened = store::elastic_array(&db, &spec.owner, &spec.name)
                    .unwrap()
                    .unwrap();
                // Core lost the job after its result was recorded: the run is
                // interrupted, and it holds nothing up — the next Sync settles it.
                assert_eq!(reopened.snapraid_history[0].outcome, store::INTERRUPTED_OUTCOME);
                assert!(!reopened.unresolved_operation);
                assert!(reopened.last_sync_run.is_none());
            }
        }
    }

    #[tokio::test]
    async fn malformed_root_reply_finishes_real_job_and_reservations_as_needs_attention() {
        for reply in ["{", "{}"] {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            store::migrate(&conn).unwrap();
            let db = Arc::new(crate::db::Db::from_connection(conn));
            let spec = create_spec("malformed");
            let work_spec = spec.clone();
            let job = jobs::spawn(&db,"elastic_create",&spec.name,"test",
                Some(jobs::ElasticJobIntent::Create(spec.clone())),None,move |h| async move {
                    execute_job(&h,work_spec.clone(),work_spec.operation_id.clone(),async {
                        Ok(super::super::broker::CommandOutput { code:0,stdout:reply.into(),stderr:String::new() })
                    }).await
                }).unwrap();
            tokio::time::timeout(Duration::from_secs(2),async {
                loop {
                    if store::job(&db,&job.job_id).unwrap().unwrap().finished_at.is_some() { break; }
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            assert_eq!(store::job(&db,&job.job_id).unwrap().unwrap().status,"failed");
            assert_eq!(store::elastic_array(&db,&spec.owner,&spec.name).unwrap().unwrap().state,"needs_attention");
            let conn = db.read().unwrap();
            let state:String = conn.query_row("SELECT state FROM nas_elastic_operations",[],|r| r.get(0)).unwrap();
            assert_eq!(state,"needs_attention");
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM nas_elastic_disk_aliases",[],|r|r.get::<_,i64>(0)).unwrap(),6);
        }
    }

    fn disk(name: &str, size: u64) -> NasDisk {
        NasDisk {
            disk_id: format!("id-{name}"),
            name: name.to_string(),
            path: format!("/dev/{name}"),
            kind: "hdd".to_string(),
            size_bytes: size,
            role: "free".to_string(),
            health: "ok".to_string(),
            ..Default::default()
        }
    }

    const TB: u64 = 1_000_000_000_000;

    fn array() -> ElasticArrayRow {
        ElasticArrayRow {
            name: "media".to_string(),
            enabled: true,
            filesystem: "xfs".to_string(),
            create_policy: "mfs".to_string(),
            branches: vec![
                BranchRow {
                    disk_id: "id-sdg".to_string(),
                    name: "sdg".to_string(),
                    device: "/dev/sdg".to_string(),
                    role: "data".to_string(),
                },
                BranchRow {
                    disk_id: "id-sdh".to_string(),
                    name: "sdh".to_string(),
                    device: "/dev/sdh".to_string(),
                    role: "data".to_string(),
                },
                BranchRow {
                    disk_id: "id-nvme2n1".to_string(),
                    name: "nvme2n1".to_string(),
                    device: "/dev/nvme2n1".to_string(),
                    role: "cache".to_string(),
                },
            ],
            parity: vec![ParityRow {
                disk_id: "id-sdj".to_string(),
                name: "sdj".to_string(),
                device: "/dev/sdj".to_string(),
                index: 1,
            }],
            folders: vec![
                FolderRow {
                    name: "filmy".to_string(),
                    cache_policy: "yes".to_string(),
                    ..Default::default()
                },
                FolderRow {
                    name: "foto".to_string(),
                    cache_policy: "only".to_string(),
                    ..Default::default()
                },
                FolderRow {
                    name: "backup".to_string(),
                    cache_policy: "no".to_string(),
                    ..Default::default()
                },
            ],
            mover: MoverConfig::default(),
            snapraid: SnapraidConfig {
                scrub_percent: 8,
                scrub_older_than_days: 10,
                ..Default::default()
            },
            state: "active".to_string(),
            ..Default::default()
        }
    }

    /// Everything mounted, everything measured.
    fn all_mounted(array: &ElasticArrayRow, cache_used: u64) -> ArrayObservation {
        let mut probes = BTreeMap::new();
        for member in mountpoints_of(array, &ArrayObservation::default()) {
            probes.insert(
                member.mountpoint,
                BranchProbe {
                    mounted: Some(true),
                    device_present: Some(true),
                    size_bytes: Some(4 * TB),
                    used_bytes: Some(TB),
                    free_bytes: Some(3 * TB),
                },
            );
        }
        for branch in array.cache() {
            probes.insert(
                cache_branch_path(&array.name, &branch.name),
                BranchProbe {
                    mounted: Some(true),
                    device_present: Some(true),
                    size_bytes: Some(1_000_000_000_000),
                    used_bytes: Some(cache_used),
                    free_bytes: Some(1_000_000_000_000 - cache_used),
                },
            );
        }
        ArrayObservation {
            mount_table_known: true,
            probes,
            union_mounted: Some(true),
            last_sync_at: Some("2026-09-06T14:06:00Z".to_string()),
            parity_errors: Some(0),
            moved_unsynced_bytes: None,
            parity_stale: false,
            last_mover: None,
            cache_age: None,
            conflicts: Vec::new(),
            kernel_names: BTreeMap::new(),
        }
    }

    fn installed_all(_: &str) -> bool {
        true
    }

    /// The verdict table, one row at a time — and every assertion is on the
    /// VERDICT as well as the state, because the apply path filters on the
    /// verdict and a state string alone would let a wrong action through
    /// while the chip looked right.
    ///
    /// THE COLD-BOOT ROW IS THE POINT OF THIS TEST. §3.4 forbids fstab, so
    /// after every reboot every branch of every array is unmounted; if that
    /// state does not judge `Apply`, `plan_mount` can never run and the array
    /// never comes back. An earlier version of this test asserted the
    /// opposite — it pinned `error`/`Freeze` on a cold boot as though it were
    /// intended — which is why the deadlock survived a review: the test
    /// measured correctly and checked the wrong property.
    #[test]
    fn a_cold_boot_judges_apply_so_the_array_can_come_back() {
        let a = array();
        let healthy = all_mounted(&a, 0);
        assert_eq!(
            array_state(&a, &healthy, &installed_all),
            ("active", CodedText::default(), Disposition::Apply)
        );

        // Every branch present, none mounted, union down: the state of this
        // node one second after a reboot.
        let mut cold = healthy.clone();
        for probe in cold.probes.values_mut() {
            *probe = BranchProbe::cold();
        }
        cold.union_mounted = Some(false);
        let (state, detail, verdict) = array_state(&a, &cold, &installed_all);
        assert_eq!(
            (state, verdict),
            ("pending", Disposition::Apply),
            "a cold boot must be appliable, or nothing ever mounts the array again"
        );
        assert!(
            detail.contains("not mounted yet"),
            "and it must read as work to do, not as a fault: {detail}"
        );
        // The disks are named, so an admin watching a slow boot can see which
        // ones the node is waiting on.
        for name in ["sdg", "sdh", "nvme2n1", "sdj"] {
            assert!(detail.contains(name), "{name} missing from: {detail}");
        }

        // Branches up, union still down: also `Apply`, with a different
        // sentence — there is one thing left to do.
        let mut branches_up = cold.clone();
        for probe in branches_up.probes.values_mut() {
            probe.mounted = Some(true);
        }
        let (state, detail, verdict) = array_state(&a, &branches_up, &installed_all);
        assert_eq!((state, verdict), ("pending", Disposition::Apply));
        assert!(detail.contains("union"), "{detail}");

        // One branch unmounted under a LIVE union — somebody unmounted a disk
        // by hand. Still `Apply`: mounting it back is a repair, not a risk.
        let mut one_off = healthy.clone();
        one_off
            .probes
            .insert(data_branch_path("media", "sdh"), BranchProbe::cold());
        let (state, detail, verdict) = array_state(&a, &one_off, &installed_all);
        assert_eq!((state, verdict), ("pending", Disposition::Apply));
        assert!(detail.contains("sdh") && !detail.contains("sdg"), "{detail}");
    }

    /// The most important asymmetry in the file: a missing branch never
    /// unmounts a live union, and never lets a dead one come up.
    ///
    /// What would break if this were wrong: `Remove` here would take every
    /// share on a working array offline because one disk of it dropped, and
    /// `Apply` here would mount the union over an empty directory and send
    /// client writes to the root filesystem.
    #[test]
    fn a_missing_branch_freezes_whether_or_not_the_union_is_up() {
        let a = array();
        let mut down = all_mounted(&a, 0);
        down.probes
            .insert(data_branch_path("media", "sdh"), BranchProbe::device_gone());

        let (state, detail, verdict) = array_state(&a, &down, &installed_all);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("sdh"), "the disk has to be named: {detail}");
        assert!(
            detail.contains("still serving"),
            "with the union up the sentence has to say what a client sees: {detail}"
        );

        let mut down_and_dark = down.clone();
        down_and_dark.union_mounted = Some(false);
        let (state, detail, verdict) = array_state(&a, &down_and_dark, &installed_all);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(
            detail.contains("root filesystem"),
            "with the union down the sentence has to say why it stays down: {detail}"
        );

        // THE CONTRAST that makes the two assertions above mean something:
        // the SAME mountpoint, unmounted, with the disk still present, is
        // `Apply` and not a fault at all. A verdict that could not tell those
        // two apart froze the array on every boot.
        let mut cold = all_mounted(&a, 0);
        cold.probes
            .insert(data_branch_path("media", "sdh"), BranchProbe::cold());
        let (cold_state, _, cold_verdict) = array_state(&a, &cold, &installed_all);
        assert_eq!((cold_state, cold_verdict), ("pending", Disposition::Apply));
        assert_ne!(cold_state, state);
    }

    /// Wave 6: every state sentence travels as a code the screen words, with
    /// the members it names as parameters per role — never a slot name. The
    /// English stays the node's (log, tooltip).
    #[test]
    fn the_state_detail_is_coded_with_its_members_per_role() {
        let codes = |detail: &CodedText| {
            detail
                .reasons
                .iter()
                .map(|r| (r.code.clone(), r.params.clone().into_iter().collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        };
        let p = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let a = array();
        let healthy = all_mounted(&a, 0);

        let mut cold = healthy.clone();
        for probe in cold.probes.values_mut() {
            *probe = BranchProbe::cold();
        }
        cold.union_mounted = Some(false);
        let (_, detail, _) = array_state(&a, &cold, &installed_all);
        assert_eq!(
            codes(&detail),
            vec![("branches_mountable".to_string(), p(&[("cache", "nvme2n1"), ("data", "sdg,sdh"), ("parity", "sdj")]))]
        );

        let mut gone = healthy.clone();
        gone.probes.insert(data_branch_path("media", "sdh"), BranchProbe::device_gone());
        let (_, detail, _) = array_state(&a, &gone, &installed_all);
        assert_eq!(codes(&detail), vec![("branches_gone".to_string(), p(&[("data", "sdh"), ("union", "serving")]))]);
        gone.union_mounted = Some(false);
        let (_, detail, _) = array_state(&a, &gone, &installed_all);
        assert_eq!(codes(&detail), vec![("branches_gone".to_string(), p(&[("data", "sdh"), ("union", "down")]))]);

        let mut unknown = healthy.clone();
        unknown.probes.insert(cache_branch_path("media", "nvme2n1"), BranchProbe { mounted: None, ..Default::default() });
        let (_, detail, _) = array_state(&a, &unknown, &installed_all);
        assert_eq!(codes(&detail), vec![("branches_unknown".to_string(), p(&[("cache", "nvme2n1")]))]);

        let mut up_union_down = healthy.clone();
        up_union_down.union_mounted = Some(false);
        let (_, detail, _) = array_state(&a, &up_union_down, &installed_all);
        assert_eq!(codes(&detail), vec![("union_not_mounted".to_string(), vec![])]);

        let blind = ArrayObservation { mount_table_known: false, ..healthy.clone() };
        assert_eq!(codes(&array_state(&a, &blind, &installed_all).1), vec![("mount_table_unknown".to_string(), vec![])]);
        let no_mergerfs = |id: &str| id != MERGERFS_FEATURE_ID;
        assert_eq!(codes(&array_state(&a, &healthy, &no_mergerfs).1), vec![("mergerfs_missing".to_string(), vec![])]);
        let no_snapraid = |id: &str| id != SNAPRAID_FEATURE_ID;
        assert_eq!(codes(&array_state(&a, &healthy, &no_snapraid).1), vec![("snapraid_unusable".to_string(), vec![])]);
        let mut unprotected = a.clone();
        unprotected.parity.clear();
        let observed = all_mounted(&unprotected, 0);
        assert_eq!(codes(&array_state(&unprotected, &observed, &installed_all).1), vec![("no_parity".to_string(), vec![])]);
        assert!(array_state(&a, &healthy, &installed_all).1.reasons.is_empty(), "a healthy array says nothing");

        // A stored array keys its members by SLOT ('d1', 'c1', 'parity1'),
        // which no admin has seen. A member neither mounted nor in the live
        // inventory is its number; a mounted one is its kernel name.
        let mut stored = a.clone();
        for (i, branch) in stored.branches.iter_mut().enumerate() {
            branch.disk_id = format!("id-slot-{i}");
            branch.name = if branch.role == "cache" { "c1".to_string() } else { format!("d{}", i + 1) };
        }
        stored.parity[0].name = "parity1".to_string();
        stored.parity[0].disk_id = "id-slot-parity".to_string();
        let mut stored_cold = all_mounted(&stored, 0);
        for probe in stored_cold.probes.values_mut() {
            *probe = BranchProbe::cold();
        }
        stored_cold.kernel_names.insert(data_branch_path("media", "d2"), "sdh".to_string());
        let (_, detail, _) = array_state(&stored, &stored_cold, &installed_all);
        assert_eq!(
            codes(&detail),
            vec![("branches_mountable".to_string(), p(&[("cache", "#1"), ("data", "#1,sdh"), ("parity", "#1")]))]
        );
        for slot in ["d1", "d2", "c1", "parity1"] {
            assert!(!detail.text.split(|c: char| !c.is_alphanumeric()).any(|w| w == slot), "{slot} in: {detail}");
        }
    }

    /// Wave-6 critic MINOR 9: a disk in use is refused on ONE rule. For
    /// every inventory role, the refusal, the code it carries and the
    /// sentence agree — a role that one of them forgot would let the create
    /// format a disk something else owns.
    #[test]
    fn a_disk_in_use_is_refused_on_one_rule_and_worded_from_it() {
        let base = NasDisk { disk_id: "id-sdf".into(), name: "sdf".into(), size_bytes: 8 * TB, health: "ok".into(), ..Default::default() };
        let cases = [
            ("free", "", None, None),
            ("pool_member", "", Some("tank"), Some("pool")),
            ("pool_member", "spare", Some("tank"), Some("spare")),
            ("array_member", "", Some("md0"), Some("md")),
            ("system", "", None, Some("system")),
            ("mounted", "", None, Some("mounted")),
            ("used", "", None, Some("used")),
        ];
        for (role, vdev_role, member_of, owner) in cases {
            let disk = NasDisk {
                role: role.into(),
                vdev_role: vdev_role.into(),
                member_of: member_of.map(str::to_string),
                mountpoints: if role == "mounted" { vec!["/srv".into()] } else { Vec::new() },
                ..base.clone()
            };
            let code = conflicting_owner_code(&disk);
            assert_eq!(code.as_ref().map(|c| c.0), owner, "{role}/{vdev_role}");
            assert_eq!(conflicting_owner(&disk).is_some(), code.is_some(), "{role}: the sentence and the rule disagree");
            let refusals = layout_refusals("media", &[disk.clone()], &[], &[], &BTreeSet::new(), &BTreeSet::new());
            let in_use = refusals.iter().find(|r| r.code == "disk_in_use");
            assert_eq!(in_use.is_some(), code.is_some(), "{role}: refused iff the rule says in use");
            if let Some(r) = in_use {
                assert_eq!(r.params.get("owner").map(String::as_str), owner);
                assert!(r.detail.contains(&conflicting_owner(&disk).unwrap()), "{}", r.detail);
            }
        }
    }

    /// The wizard's layout warnings and refusals as codes (wave 6), one code
    /// per English sentence, with what each names as parameters.
    #[test]
    fn the_layout_answers_in_codes_beside_its_sentences() {
        let disk = |id: &str, size: u64, health: &str| NasDisk {
            disk_id: format!("id-{id}"),
            name: id.to_string(),
            size_bytes: size,
            health: health.to_string(),
            role: "free".to_string(),
            ..Default::default()
        };
        let data = [disk("sdb", 8 * TB, "ok"), disk("sdc", 4 * TB, "warning")];
        let parity = [disk("sdd", 8 * TB, "ok")];
        let codes = layout_warning_codes(&data, &parity, &[]);
        let words: Vec<&str> = codes.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(words, vec!["parity_tight", "no_cache", "unhealthy_disks", "mixed_sizes"]);
        assert_eq!(codes[0].params.get("disk").map(String::as_str), Some("sdd"));
        assert_eq!(codes[2].params.get("disks").map(String::as_str), Some("sdc"));
        let sentences = layout_warnings(&data, &parity, &[]);
        assert_eq!(sentences.len(), codes.len(), "one sentence per code");
        assert!(sentences[2].contains("SMART warnings on sdc"), "{sentences:?}");
        assert_eq!(layout_warning_codes(&data, &[], &[])[0].code, "no_parity");

        let small = [disk("sde", 4 * TB, "ok")];
        let refusals = layout_refusals("media", &data, &small, &[], &BTreeSet::new(), &BTreeSet::new());
        let too_small = refusals.iter().find(|r| r.code == "parity_too_small").expect("refused");
        assert_eq!(too_small.disk_name, "sde");
        assert_eq!(too_small.params.get("size").map(String::as_str), Some(&*(4 * TB).to_string()));
        assert_eq!(too_small.params.get("largest").map(String::as_str), Some(&*(8 * TB).to_string()));

        let mut member = disk("sdf", 8 * TB, "ok");
        member.role = "pool_member".to_string();
        member.member_of = Some("tank".to_string());
        let refusals = layout_refusals("media", &[member], &[], &[], &BTreeSet::new(), &BTreeSet::new());
        let in_use = refusals.iter().find(|r| r.code == "disk_in_use").expect("refused");
        let param = |k: &str| in_use.params.get(k).map(String::as_str);
        assert_eq!((param("owner"), param("owner_name"), param("role")), (Some("pool"), Some("tank"), Some("data")));
    }

    /// Unknown is not "not mounted", and the two produce different sentences.
    ///
    /// This is the defect class the whole model is shaped against, so it is
    /// asserted on the SENTENCE and not only on the verdict: both freeze, and
    /// an admin who cannot tell them apart looks at the wrong thing.
    #[test]
    fn an_unmeasured_branch_is_not_an_unmounted_branch() {
        let a = array();
        let mut unknown = all_mounted(&a, 0);
        unknown.probes.insert(
            data_branch_path("media", "sdh"),
            BranchProbe {
                mounted: None,
                ..Default::default()
            },
        );
        let (state, detail, verdict) = array_state(&a, &unknown, &installed_all);
        assert_eq!((state, verdict), ("unknown", Disposition::Freeze));
        assert!(detail.contains("could not tell"), "{detail}");
        assert!(detail.contains("sdh"), "{detail}");

        // And the same branch reported as unmounted says something else.
        let mut down = all_mounted(&a, 0);
        down.probes
            .insert(data_branch_path("media", "sdh"), BranchProbe::device_gone());
        let (down_state, down_detail, _) = array_state(&a, &down, &installed_all);
        assert_ne!(down_state, state);
        assert_ne!(down_detail, detail);

        // A branch that is not mounted and whose DISK nobody looked at is
        // also unknown — the third reading, and the one that keeps
        // `device_present: None` from being read as "the disk is there".
        let mut unlooked = all_mounted(&a, 0);
        unlooked.probes.insert(
            data_branch_path("media", "sdh"),
            BranchProbe {
                mounted: Some(false),
                device_present: None,
                ..Default::default()
            },
        );
        let (state, detail, verdict) = array_state(&a, &unlooked, &installed_all);
        assert_eq!(
            (state, verdict),
            ("unknown", Disposition::Freeze),
            "an unexamined disk must not be assumed present: {detail}"
        );

        // An unreadable mount table freezes the whole array, whatever the
        // individual probes say.
        let mut blind = all_mounted(&a, 0);
        blind.mount_table_known = false;
        let (state, _, verdict) = array_state(&a, &blind, &installed_all);
        assert_eq!((state, verdict), ("unknown", Disposition::Freeze));
    }

    /// Only the admin unmounts an array. A missing tool reports and waits.
    #[test]
    fn only_a_disabled_array_is_ever_taken_down() {
        let mut a = array();
        let healthy = all_mounted(&a, 0);

        a.enabled = false;
        a.state = "active".to_string();
        a.state_detail = "some stale sentence about a running array".to_string();
        let (state, detail, verdict) = array_state(&a, &healthy, &installed_all);
        assert_eq!((state, verdict), ("disabled", Disposition::Remove));
        assert!(detail.is_empty(), "a stopped array drops its old detail: {detail}");

        // An imported row keeps the reason it arrived with.
        a.state = "disabled".to_string();
        a.state_detail = "switched off by the import: the cache disk is not on this node".to_string();
        let (_, detail, _) = array_state(&a, &healthy, &installed_all);
        assert!(detail.contains("import"), "{detail}");

        // Missing tools: reported, never unmounted.
        let a = array();
        let no_mergerfs = |id: &str| id != MERGERFS_FEATURE_ID;
        let (state, detail, verdict) = array_state(&a, &healthy, &no_mergerfs);
        assert_eq!((state, verdict), ("error", Disposition::Freeze));
        assert!(detail.contains("mergerfs"), "{detail}");
        let no_snapraid = |id: &str| id != SNAPRAID_FEATURE_ID;
        let (_, detail, verdict) = array_state(&a, &healthy, &no_snapraid);
        assert_eq!(verdict, Disposition::Freeze);
        assert!(detail.contains("snapraid"), "{detail}");

        // …and an array with no parity does not care about snapraid at all.
        let mut no_parity = array();
        no_parity.parity.clear();
        let observed = all_mounted(&no_parity, 0);
        let (state, _, verdict) = array_state(&no_parity, &observed, &no_snapraid);
        assert_eq!((state, verdict), ("active", Disposition::Apply));
    }

    /// The protection window is a SIZE, it comes from the cache, and it is
    /// `None` when a cache disk could not be read.
    #[test]
    fn the_unprotected_window_is_measured_in_bytes_and_never_guessed() {
        let a = array();
        // 18 GiB on the cache — the canonical figure of the mockups.
        let eighteen_gib = 18 * 1024 * 1024 * 1024;
        let p = protection(&a, &all_mounted(&a, eighteen_gib));
        assert_eq!(p.cache_unprotected_bytes, Some(eighteen_gib));
        assert_eq!(p.status, "window_open");
        assert_eq!(p.fault_tolerance, Some(1));
        assert_eq!(p.protected_as_of.as_deref(), Some("2026-09-06T14:06:00Z"));
        assert!(
            p.detail.contains("na cache bez parity (czeka na mover)"),
            "the sentence the UI builds has to come from here: {}",
            p.detail
        );
        assert!(
            !p.detail.contains("godz") && !p.detail.contains("niezsynchronizowan"),
            "the banned phrasing must not reappear: {}",
            p.detail
        );

        // Ochronę potwierdza także pomiar zmian na dyskach danych.
        let mut verified = all_mounted(&a, 0);
        verified.moved_unsynced_bytes = Some(0);
        let p = protection(&a, &verified);
        assert_eq!(p.cache_unprotected_bytes, Some(0));
        assert_eq!(p.status, "protected");
        // BLK-1: the sentence may claim only what was actually measured.
        // `moved_unsynced_bytes == Some(0)` says the MOVER left nothing behind;
        // it is not a scan of the array, and an MFS write landing straight on a
        // data branch is invisible to every probe this node has.
        assert!(
            p.detail.contains("mover nie zostawił danych poza ostatnim sync"),
            "{}",
            p.detail
        );
        assert!(
            p.detail.contains("ochrona obejmuje stan z ostatniego sync"),
            "the sentence has to name the vintage of what is protected: {}",
            p.detail
        );
        assert!(
            !p.detail.contains("pomiar nie wykazał"),
            "a measurement nobody took must not be claimed: {}",
            p.detail
        );

        // An unreadable cache disk makes the figure unknown — NOT a partial
        // sum, which would understate the risk.
        let mut blind = all_mounted(&a, eighteen_gib);
        blind.probes.insert(
            cache_branch_path("media", "nvme2n1"),
            BranchProbe {
                mounted: Some(true),
                device_present: Some(true),
                size_bytes: None,
                used_bytes: None,
                free_bytes: None,
            },
        );
        let p = protection(&a, &blind);
        assert_eq!(p.cache_unprotected_bytes, None);
        assert_eq!(p.status, "unknown");

        // No parity at all: `Some(0)` is a measurement here, and the status
        // says the array is not protected rather than that the window is open.
        let mut bare = array();
        bare.parity.clear();
        let p = protection(&bare, &all_mounted(&bare, 0));
        assert_eq!(p.fault_tolerance, Some(0));
        assert_eq!(p.status, "unprotected");

        // Two parity disks, one of them down: the tolerance is what is
        // actually there.
        let mut two = array();
        two.parity.push(ParityRow {
            disk_id: "id-sdk".to_string(),
            name: "sdk".to_string(),
            device: "/dev/sdk".to_string(),
            index: 2,
        });
        let mut observed = all_mounted(&two, 0);
        observed
            .probes
            .insert(parity_mount_path("media", 2), BranchProbe::device_gone());
        assert_eq!(protection(&two, &observed).fault_tolerance, Some(1));
        assert_eq!(protection(&two, &observed).status, "unknown");
        // …and unreadable is not zero.
        observed.probes.insert(
            parity_mount_path("media", 2),
            BranchProbe {
                mounted: None,
                ..Default::default()
            },
        );
        assert_eq!(protection(&two, &observed).fault_tolerance, None);
    }

    /// MAJ-2: a MEASURED open window must not be silenced by an UNMEASURED
    /// fault tolerance. The status stays `unknown` — nothing confirmed parity,
    /// and that is the more serious fact — but the sentence still carries the
    /// 18 GiB the card is already showing beside it.
    #[test]
    fn a_measured_open_window_survives_an_unmeasured_fault_tolerance() {
        let a = array();
        let eighteen_gib = 18 * 1024 * 1024 * 1024;
        let canon = "na cache bez parity (czeka na mover)";
        for case in ["parity_errors_unknown", "mounts_unknown", "parity_unreadable"] {
            let mut observed = all_mounted(&a, eighteen_gib);
            match case {
                "parity_errors_unknown" => observed.parity_errors = None,
                "mounts_unknown" => observed.mount_table_known = false,
                _ => {
                    observed.probes.insert(
                        parity_mount_path("media", 1),
                        BranchProbe {
                            mounted: None,
                            ..Default::default()
                        },
                    );
                }
            }
            let p = protection(&a, &observed);
            assert_eq!(p.fault_tolerance, None, "{case}");
            assert_eq!(p.status, "unknown", "{case}");
            assert_eq!(p.cache_unprotected_bytes, Some(eighteen_gib), "{case}");
            assert!(
                p.detail.contains("brak pełnego pomiaru"),
                "{case}: {}",
                p.detail
            );
            assert!(
                p.detail.contains(canon),
                "a measured figure must not vanish behind an unmeasured one, {case}: {}",
                p.detail
            );
        }
        // The missing-parity arm is reachable with an open window too: two
        // parity disks, one gone, is a MEASURED tolerance of 1 against 2
        // configured — and the cache figure beside it must survive that.
        let mut two = array();
        two.parity.push(ParityRow {
            disk_id: "id-sdk".to_string(),
            name: "sdk".to_string(),
            device: "/dev/sdk".to_string(),
            index: 2,
        });
        let mut gone = all_mounted(&two, eighteen_gib);
        gone.probes
            .insert(parity_mount_path("media", 2), BranchProbe::device_gone());
        let p = protection(&two, &gone);
        assert_eq!(p.fault_tolerance, Some(1));
        assert_eq!(p.status, "unknown");
        assert!(p.detail.contains("brakuje dysku parity"), "{}", p.detail);
        assert!(
            p.detail.contains(canon),
            "a measured window must survive a missing parity disk: {}",
            p.detail
        );

        // A stale sync reaches those arms the same way, and says so.
        let mut stale = all_mounted(&a, 0);
        stale.parity_errors = None;
        stale.parity_stale = true;
        stale.moved_unsynced_bytes = Some(4096);
        let p = protection(&a, &stale);
        assert_eq!(p.status, "unknown");
        assert!(p.detail.contains("brak pełnego pomiaru"), "{}", p.detail);
        assert!(p.detail.contains("przeniesione przez mover"), "{}", p.detail);
        // And with nothing open, the arm keeps exactly the sentence it had.
        let mut closed = all_mounted(&a, 0);
        closed.parity_errors = None;
        let p = protection(&a, &closed);
        assert_eq!(p.detail, "brak pełnego pomiaru dostępności i poprawności parity");
    }

    /// OWNER DECISION (2026-09-12), pinned: a cache this node could NOT measure,
    /// beside parity the helper confirms is stale, is a `window_open` — not an
    /// `unknown`. The confirmed fact outranks the missing measurement, and the
    /// sentence carries both. This differs from the last committed behaviour,
    /// where the unmeasured-cache arm won; it is accepted, and this test is what
    /// stops it drifting back.
    #[test]
    fn a_confirmed_stale_parity_outranks_an_unmeasured_cache() {
        let a = array();
        let unreadable = BranchProbe {
            mounted: Some(true),
            device_present: Some(true),
            size_bytes: None,
            used_bytes: None,
            free_bytes: None,
        };
        for (case, moved) in [
            ("nic nie zmierzono jako przeniesione", None),
            ("zmierzone zero przeniesionych bajtów", Some(0)),
            ("zmierzone bajty poza parity", Some(4096)),
        ] {
            let mut blind = all_mounted(&a, 0);
            blind.parity_stale = true;
            blind.moved_unsynced_bytes = moved;
            blind
                .probes
                .insert(cache_branch_path("media", "nvme2n1"), unreadable.clone());
            let p = protection(&a, &blind);
            assert_eq!(p.cache_unprotected_bytes, None, "{case}");
            assert_eq!(
                p.status, "window_open",
                "{case}: potwierdzony fakt bije niezmierzony, {}",
                p.detail
            );
            // Both halves are said: the cache is unmeasured AND parity is stale.
            assert!(p.detail.contains("nie zmierzył"), "{case}: {}", p.detail);
            assert!(
                p.detail.contains("nie potwierdził ochrony")
                    || p.detail.contains("przeniesione przez mover"),
                "{case}: {}",
                p.detail
            );
            // The drift guard: falling through to the unmeasured-cache arm
            // below would drop the confirmed half entirely.
            assert_ne!(
                p.detail, "ten node nie zmierzył, ile czeka na cache",
                "{case}: potwierdzona nieaktualna parity nie może zniknąć"
            );
        }
    }

    /// Brak cache oznacza zero danych na cache, ale nie potwierdza stanu dysków danych.
    #[test]
    fn an_array_with_no_cache_reports_zero_unprotected_bytes() {
        let mut a = array();
        a.branches.retain(|b| b.role != "cache");
        assert!(!a.parity.is_empty(), "parity is kept, or the test proves nothing");

        let p = protection(&a, &all_mounted(&a, 0));
        assert_eq!(
            p.cache_unprotected_bytes,
            Some(0),
            "no cache disk means no unprotected cache bytes, and that is measured"
        );
        assert_eq!(p.status, "unknown");
        assert_eq!(p.fault_tolerance, Some(1));

        // Karta dostaje potwierdzoną ochronę dopiero po pomiarze zmian.
        let disks: BTreeMap<String, NasDisk> = ["sdg", "sdh", "sdj"]
            .into_iter()
            .map(|n| (format!("id-{n}"), disk(n, 4 * TB)))
            .collect();
        let mut observed = all_mounted(&a, 0);
        let wire = to_protocol(&a, &disks, &observed, true, "12.3", ("active", ""));
        assert_eq!(wire.health, "unknown", "{}", wire.health_reason);
        observed.moved_unsynced_bytes = Some(0);
        let wire = to_protocol(&a, &disks, &observed, true, "12.3", ("active", ""));
        assert_eq!(wire.protection.status, "protected");
        assert_eq!(wire.health, "ok", "{}", wire.health_reason);
        assert_eq!(wire.cache_size_bytes, Some(0));
    }

    #[test]
    fn protection_requires_measured_data_and_healthy_parity_with_or_without_cache() {
        for with_cache in [false, true] {
            let mut array = array();
            if !with_cache {
                array.branches.retain(|branch| branch.role != "cache");
            }
            for (case, expected_status) in [
                ("verified", "protected"),
                ("moved", "window_open"),
                ("diff_unknown", "unknown"),
                ("no_sync", "window_open"),
                ("parity_errors", "unknown"),
                ("parity_errors_unknown", "unknown"),
                ("parity_missing", "unknown"),
                ("parity_unknown", "unknown"),
                ("mounts_unknown", "unknown"),
                ("no_parity", "unprotected"),
            ] {
                let mut row = array.clone();
                let mut observed = all_mounted(&row, 0);
                observed.moved_unsynced_bytes = Some(0);
                match case {
                    "moved" => observed.moved_unsynced_bytes = Some(1024),
                    "diff_unknown" => observed.moved_unsynced_bytes = None,
                    "no_sync" => observed.last_sync_at = None,
                    "parity_errors" => observed.parity_errors = Some(1),
                    "parity_errors_unknown" => observed.parity_errors = None,
                    "parity_missing" => {
                        observed
                            .probes
                            .insert(parity_mount_path(&row.name, 1), BranchProbe::device_gone());
                    }
                    "parity_unknown" => {
                        observed
                            .probes
                            .insert(parity_mount_path(&row.name, 1), BranchProbe::default());
                    }
                    "mounts_unknown" => observed.mount_table_known = false,
                    "no_parity" => row.parity.clear(),
                    _ => {}
                }

                let protection = protection(&row, &observed);
                let wire = to_protocol(
                    &row,
                    &BTreeMap::new(),
                    &observed,
                    true,
                    "14.7",
                    ("active", ""),
                );

                assert_eq!(
                    protection.status, expected_status,
                    "{case}, cache={with_cache}: {}",
                    protection.detail
                );
                assert_eq!(wire.protection, protection);
                assert_eq!(protection.cache_unprotected_bytes, Some(0));
                if case == "moved" {
                    assert_eq!(protection.fault_tolerance, Some(1));
                    assert_eq!(protection.moved_unsynced_bytes, Some(1024));
                    assert_eq!(
                        protection.protected_as_of.as_deref(),
                        Some("2026-09-06T14:06:00Z")
                    );
                    assert_eq!(protection.status, "window_open");
                }
                if expected_status == "unknown" {
                    assert_ne!(wire.health, "ok", "{case}: {}", wire.health_reason);
                }
                if matches!(
                    case,
                    "parity_errors"
                        | "parity_errors_unknown"
                        | "parity_unknown"
                        | "mounts_unknown"
                        | "no_sync"
                ) {
                    assert_eq!(protection.fault_tolerance, None, "{case}");
                }
            }
        }
    }

    /// The mover fires on cache pressure, once, and then waits out its
    /// cooldown.
    ///
    /// Against THIS test's own clock, never the process-wide one: three
    /// targets tests once shared a global clock and could pass or fail on
    /// thread order, in both directions.
    #[test]
    fn cache_pressure_starts_one_mover_run_and_then_waits() {
        let a = array();
        let clock = MoverClock::new();
        // 5% free against a 20% floor.
        let pressed = {
            let mut o = all_mounted(&a, 0);
            o.probes.insert(
                cache_branch_path("media", "nvme2n1"),
                BranchProbe {
                    mounted: Some(true),
                    device_present: Some(true),
                    size_bytes: Some(1000),
                    used_bytes: Some(950),
                    free_bytes: Some(50),
                },
            );
            o
        };
        assert_eq!(mover_trigger(&a, &pressed, &clock), MoverTrigger::CacheLow);

        clock.started(&a.name);
        assert_eq!(
            mover_trigger(&a, &pressed, &clock),
            MoverTrigger::None,
            "a run that just started is the answer to this threshold"
        );
        clock.rewind_for_test(&a.name, MOVER_RETRIGGER_COOLDOWN - Duration::from_secs(1));
        assert_eq!(mover_trigger(&a, &pressed, &clock), MoverTrigger::None);
        clock.rewind_for_test(&a.name, MOVER_RETRIGGER_COOLDOWN + Duration::from_secs(1));
        assert_eq!(
            mover_trigger(&a, &pressed, &clock),
            MoverTrigger::CacheLow,
            "past the cooldown the pressure is still there and the mover runs again"
        );

        // Plenty of room: nothing fires.
        assert_eq!(mover_trigger(&a, &all_mounted(&a, 0), &clock), MoverTrigger::None);

        // An unmeasured cache does NOT fire. Starting a privileged job that
        // moves files because a free-space figure could not be read is acting
        // on a measurement that does not exist.
        let mut blind = pressed.clone();
        blind.probes.insert(
            cache_branch_path("media", "nvme2n1"),
            BranchProbe {
                mounted: Some(true),
                device_present: Some(true),
                size_bytes: None,
                used_bytes: None,
                free_bytes: None,
            },
        );
        let fresh = MoverClock::new();
        assert_eq!(mover_trigger(&a, &blind, &fresh), MoverTrigger::None);

        // A schedule that restricts moving does not hold back cache pressure:
        // below the threshold the mover runs whatever the schedule says.
        let mut restricted = array();
        restricted.mover.schedule_enabled = true;
        restricted.mover.schedule = Some(NasSchedule { every: "daily".into(), hour: 3, ..Default::default() });
        assert_eq!(mover_trigger(&restricted, &pressed, &fresh), MoverTrigger::CacheLow);
        // A switched-off array never fires, however full the cache is.
        let mut off = array();
        off.enabled = false;
        assert_eq!(mover_trigger(&off, &pressed, &fresh), MoverTrigger::None);
        // Nor does one with no cache to drain.
        let mut cacheless = array();
        cacheless.branches.retain(|b| b.role != "cache");
        assert_eq!(mover_trigger(&cacheless, &pressed, &fresh), MoverTrigger::None);
    }

    fn aged(due_files: u64, oldest_secs: Option<u64>) -> ElasticCacheAge {
        ElasticCacheAge { due_files, due_bytes: due_files * 1024, held_files: 0, oldest_secs }
    }

    fn with_age(observed: ArrayObservation, age: ElasticCacheAge) -> ArrayObservation {
        ArrayObservation { cache_age: Some(age), ..observed }
    }

    /// The owner's rule (2026-09-17): an array with a cache moves its files to
    /// the data disks by itself. An array with NO schedule, a roomy cache and
    /// aged files starts a run; the same array with only fresh files, with an
    /// unmeasured cache age, cooling down, or without a cache does not.
    #[test]
    fn aged_files_start_a_run_with_no_schedule_and_a_roomy_cache() {
        let a = array();
        assert_eq!(a.mover.schedule, None, "the fixture has no schedule at all");
        assert!(!a.mover.schedule_enabled);
        let roomy = all_mounted(&a, 0);
        let clock = MoverClock::new();
        assert_eq!(
            mover_trigger(&a, &with_age(roomy.clone(), aged(3, Some(9_000))), &clock),
            MoverTrigger::FilesAged
        );
        // Only fresh files: waiting on the cache is what they are there for.
        assert_eq!(mover_trigger(&a, &with_age(roomy.clone(), aged(0, Some(60))), &clock), MoverTrigger::None);
        // Not measured is not "aged".
        assert_eq!(mover_trigger(&a, &roomy, &clock), MoverTrigger::None);

        clock.started(&a.name);
        assert_eq!(
            mover_trigger(&a, &with_age(roomy.clone(), aged(3, Some(9_000))), &clock),
            MoverTrigger::None,
            "a run that just started is the answer to these files"
        );
        clock.rewind_for_test(&a.name, MOVER_RETRIGGER_COOLDOWN + Duration::from_secs(1));
        assert_eq!(
            mover_trigger(&a, &with_age(roomy.clone(), aged(3, Some(9_000))), &clock),
            MoverTrigger::FilesAged
        );

        let mut cacheless = array();
        cacheless.branches.retain(|b| b.role != "cache");
        assert_eq!(
            mover_trigger(&cacheless, &with_age(all_mounted(&cacheless, 0), aged(3, Some(9_000))), &MoverClock::new()),
            MoverTrigger::None
        );
    }

    /// A switched-on schedule RESTRICTS automatic moving to its slots: aged
    /// files wait for the scheduled run. Switched off, it restricts nothing.
    #[test]
    fn a_switched_on_schedule_restricts_the_age_trigger_but_not_a_switched_off_one() {
        let mut a = array();
        a.mover.schedule = Some(NasSchedule { every: "daily".into(), hour: 3, ..Default::default() });
        let observed = with_age(all_mounted(&a, 0), aged(3, Some(9_000)));
        assert_eq!(mover_trigger(&a, &observed, &MoverClock::new()), MoverTrigger::FilesAged, "saved but off");
        a.mover.schedule_enabled = true;
        assert!(restricted_to_schedule(&a));
        assert_eq!(mover_trigger(&a, &observed, &MoverClock::new()), MoverTrigger::None);
    }

    /// A run that moved nothing is evidence the due files cannot move now (no
    /// room, a colliding path). The age trigger then waits out a whole budget
    /// instead of holding the union read-only every cooldown for nothing.
    #[test]
    fn after_a_run_that_moved_nothing_the_age_trigger_backs_off() {
        let mut a = array();
        a.mover_history = vec![NasMoverRun { outcome: "ok".into(), moved_files: 0, ..Default::default() }];
        let observed = with_age(all_mounted(&a, 0), aged(3, Some(9_000)));
        let clock = MoverClock::new();
        // Nothing started in this process yet: the one retry is allowed.
        assert_eq!(mover_trigger(&a, &observed, &clock), MoverTrigger::FilesAged);
        clock.started(&a.name);
        clock.rewind_for_test(&a.name, MOVER_RETRIGGER_COOLDOWN + Duration::from_secs(1));
        assert_eq!(mover_trigger(&a, &observed, &clock), MoverTrigger::None, "cooled down, still backing off");
        clock.rewind_for_test(&a.name, Duration::from_secs(4 * 7_200 + 1));
        assert_eq!(mover_trigger(&a, &observed, &clock), MoverTrigger::FilesAged);
        // A run that moved something is no reason to wait.
        a.mover_history[0].moved_files = 5;
        let clock = MoverClock::new();
        clock.started(&a.name);
        clock.rewind_for_test(&a.name, MOVER_RETRIGGER_COOLDOWN + Duration::from_secs(1));
        assert_eq!(mover_trigger(&a, &observed, &clock), MoverTrigger::FilesAged);
    }

    /// The stuck alert fires on AGE past its threshold and nothing else: a
    /// cache full of fresh files is healthy. It clears on the first probe that
    /// finds nothing that old, and stays put while a run for the files starts.
    #[test]
    fn the_stuck_alert_fires_only_past_its_threshold_and_clears() {
        let mut a = array();
        // Defaults: 2 h age rule, four of them.
        assert_eq!(cache_stuck_after_secs(&a), 8 * 3_600);
        let limit = cache_stuck_after_secs(&a);
        // Lots of pending data, all of it younger than the threshold.
        assert_eq!(cache_stuck_verdict(&a, &aged(500, Some(limit - 1)), false), CacheStuckVerdict::Clear);
        assert_eq!(cache_stuck_verdict(&a, &ElasticCacheAge::default(), false), CacheStuckVerdict::Clear);
        // Past it, with a run starting for the files: neither opened nor closed.
        assert_eq!(cache_stuck_verdict(&a, &aged(2, Some(limit)), true), CacheStuckVerdict::Keep);
        // Past it with nothing started: raised, and the reason is named.
        let held = ElasticCacheAge { due_files: 0, due_bytes: 0, held_files: 1, oldest_secs: Some(limit + 3_600) };
        match cache_stuck_verdict(&a, &held, false) {
            CacheStuckVerdict::Raise(text) => {
                assert!(text.title.contains("media"), "{}", text.title);
                assert!(text.detail.contains("9 h"), "{}", text.detail);
                assert!(text.detail.contains("alarm po 8 h"), "{}", text.detail);
                assert!(text.detail.contains("otwarte"), "{}", text.detail);
                // The same, coded: the waits cut to the unit the sentence
                // shows, and the cause as a word the screen translates.
                assert_eq!(text.code, "elastic_cache_stuck");
                assert_eq!(
                    text.params.into_iter().collect::<Vec<_>>(),
                    vec![
                        ("array".to_string(), "media".to_string()),
                        ("cause".to_string(), "files_busy".to_string()),
                        ("limit_secs".to_string(), (8 * 3_600).to_string()),
                        ("oldest_secs".to_string(), (9 * 3_600).to_string()),
                    ]
                );
            }
            other => panic!("expected a raise, got {other:?}"),
        }
        a.unresolved_operation = true;
        let CacheStuckVerdict::Raise(text) = cache_stuck_verdict(&a, &aged(2, Some(limit)), false) else {
            panic!("an unresolved operation past the threshold is stuck");
        };
        assert!(text.detail.contains("nierozwiązana operacja"), "{}", text.detail);
        assert_eq!(text.params.get("cause").map(String::as_str), Some("unresolved_operation"));
        // A minute later the sentence reads the same, so the coded row must
        // too — or every probe would rewrite the open alert.
        let CacheStuckVerdict::Raise(later) = cache_stuck_verdict(&a, &aged(2, Some(limit + 59)), false) else {
            panic!("still stuck");
        };
        assert_eq!(later, text, "a probe a minute later leaves the alert as it was");

        // No age rule still leaves four cooldowns; a daily window adds a day.
        a.mover.min_age_secs = 0;
        assert_eq!(cache_stuck_after_secs(&a), 4 * MOVER_RETRIGGER_COOLDOWN.as_secs());
        a.mover.min_age_secs = 7_200;
        a.mover.schedule = Some(NasSchedule { every: "daily".into(), hour: 3, ..Default::default() });
        a.mover.schedule_enabled = true;
        assert_eq!(cache_stuck_after_secs(&a), 8 * 3_600 + 86_400);
    }

    /// The parity rule of §5.3, as a REFUSAL with the disk named — the exact
    /// case n08b's step 3 shows.
    #[test]
    fn a_parity_disk_smaller_than_the_largest_data_disk_is_refused_by_name() {
        let data = vec![disk("sdl", 8 * TB), disk("sdn", 4 * TB)];
        let small = vec![disk("sdo", 4 * TB)];
        let refusals = layout_refusals("archiwum", &data, &small, &[], &BTreeSet::new(), &BTreeSet::new());
        let parity = refusals
            .iter()
            .find(|r| r.code == "parity_too_small")
            .expect("the rule has to fire");
        assert_eq!(parity.disk_name, "sdo");
        assert_eq!(parity.disk_id, "id-sdo");
        assert!(parity.detail.contains("8.0 TB"), "{}", parity.detail);
        assert!(parity.detail.contains("4.0 TB"), "{}", parity.detail);

        // Equal is enough — the rule is "at least as large", not "larger".
        let equal = vec![disk("sdm", 8 * TB)];
        let refusals = layout_refusals("archiwum", &data, &equal, &[], &BTreeSet::new(), &BTreeSet::new());
        assert!(
            !refusals.iter().any(|r| r.code == "parity_too_small"),
            "{refusals:?}"
        );
        // …but it earns a warning, because a parity FILE has to fit on a
        // filesystem and the filesystem takes room the file cannot have.
        let warnings = layout_warnings(&data, &equal, &[]);
        assert!(
            warnings.iter().any(|w| w.contains("only just as large")),
            "{warnings:?}"
        );

        // Both parity disks are judged, not just the first.
        let mixed = vec![disk("sdm", 8 * TB), disk("sdo", 4 * TB)];
        let refusals = layout_refusals("archiwum", &data, &mixed, &[], &BTreeSet::new(), &BTreeSet::new());
        let named: Vec<&str> = refusals
            .iter()
            .filter(|r| r.code == "parity_too_small")
            .map(|r| r.disk_name.as_str())
            .collect();
        assert_eq!(named, vec!["sdo"]);
    }

    /// A disk that already belongs to something else is refused, and the
    /// conflicting owner is NAMED.
    #[test]
    fn a_disk_owned_by_a_zfs_pool_or_a_spare_is_refused_with_its_owner() {
        let mut in_pool = disk("sda", 8 * TB);
        in_pool.role = "pool_member".to_string();
        in_pool.member_of = Some("tank".to_string());

        let mut spare = disk("sdb", 8 * TB);
        spare.role = "pool_member".to_string();
        spare.member_of = Some("tank".to_string());
        spare.vdev_role = "spare".to_string();

        let mut system = disk("sdc", 500_000_000_000);
        system.role = "system".to_string();

        let refusals = layout_refusals(
            "archiwum",
            &[in_pool, spare, system],
            &[],
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
        );
        let by = |name: &str| {
            refusals
                .iter()
                .find(|r| r.disk_name == name)
                .unwrap_or_else(|| panic!("{name} was not refused: {refusals:?}"))
        };
        assert_eq!(by("sda").code, "disk_in_use");
        assert!(by("sda").detail.contains("ZFS pool tank"), "{}", by("sda").detail);
        // The spare is reported AS a spare, not as an ordinary pool member:
        // an admin told "member of tank" would go looking in the data vdevs.
        assert!(
            by("sdb").detail.contains("hot spare"),
            "{}",
            by("sdb").detail
        );
        assert!(by("sdc").detail.contains("running system"), "{}", by("sdc").detail);

        // An array already holding the disk refuses it too, with its own code
        // path — the inventory cannot know about other arrays.
        let free = disk("sdd", 8 * TB);
        let taken: BTreeSet<String> = ["id-sdd".to_string()].into_iter().collect();
        let refusals = layout_refusals("archiwum", &[free.clone()], &[], &[], &taken, &BTreeSet::new());
        assert!(refusals.iter().any(|r| r.detail.contains("another Elastic Array")));
        // …and with nothing taken, the same disk passes.
        assert!(layout_refusals("archiwum", &[free], &[], &[], &BTreeSet::new(), &BTreeSet::new()).is_empty());
    }

    /// An array may not take the mountpoint of a ZFS pool.
    ///
    /// The two share one namespace under `/mnt/`, and the failure this stops
    /// is not cosmetic: a mergerfs union mounted at `/mnt/media` over a
    /// mounted pool called `media` hides the pool's files and takes every
    /// subsequent write.
    #[test]
    fn an_array_may_not_take_a_mounted_pools_name() {
        let data = vec![disk("sdg", 8 * TB)];
        let pools: BTreeSet<String> = ["tank".to_string(), "media".to_string()]
            .into_iter()
            .collect();
        let refusals =
            layout_refusals("media", &data, &[], &[], &BTreeSet::new(), &pools);
        let taken = refusals
            .iter()
            .find(|r| r.code == "name_taken")
            .expect("the collision has to be refused");
        assert!(taken.detail.contains("/mnt/media"), "{}", taken.detail);
        // A free name passes, so the assertion above is reading the set and
        // not simply always firing.
        assert!(layout_refusals("archiwum", &data, &[], &[], &BTreeSet::new(), &pools).is_empty());
        // An empty name is the wizard's step 2, before the name is typed: it
        // is not checked against anything.
        assert!(layout_refusals("", &data, &[], &[], &BTreeSet::new(), &pools).is_empty());
    }

    #[test]
    fn layout_refuses_internal_mount_roots_but_accepts_distinct_name() {
        let data = vec![disk("sdg", 8 * TB)];
        for name in ["tentanas", "tentanas-branches"] {
            let refusals = layout_refusals(
                name, &data, &[], &[], &BTreeSet::new(), &BTreeSet::new(),
            );
            assert!(refusals.iter().any(|refusal| refusal.code == "name_invalid"));
        }
        assert!(layout_refusals(
            "tentanas-data", &data, &[], &[], &BTreeSet::new(), &BTreeSet::new(),
        ).is_empty());
    }

    #[test]
    fn layout_plan_refuses_physical_disk_aliases_in_every_pair_of_roles() {
        for (first_role, second_role) in [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)] {
            for identity in ["wwn", "serial", "path"] {
                let mut first = disk("sdy", 8 * TB);
                let mut second = disk("sdz", 8 * TB);
                match identity {
                    "wwn" => {
                        first.wwn = Some("0x5000c500a1b2c3d4".to_string());
                        second.wwn = first.wwn.clone();
                    }
                    "serial" => {
                        first.serial = "ZR18AB3F".to_string();
                        second.serial = first.serial.clone();
                    }
                    _ => second.path = first.path.clone(),
                }
                let mut roles: [Vec<NasDisk>; 3] = Default::default();
                roles[first_role].push(first);
                roles[second_role].push(second.clone());
                if roles[0].is_empty() {
                    roles[0].push(disk("sdx", 8 * TB));
                }

                let plan = plan_layout(
                    "media",
                    "xfs",
                    &roles[0],
                    &roles[1],
                    &roles[2],
                    &BTreeSet::new(),
                    &BTreeSet::new(),
                    &[],
                    &Tools::for_preview(),
                );

                if first_role == 2 && second_role == 2 {
                    assert!(plan.refusals.iter().any(|refusal| refusal.code == "too_many_cache_disks"), "{:?}", plan.refusals);
                } else {
                    assert_eq!(plan.refusals.len(), 1, "{first_role}/{second_role}/{identity}: {:?}", plan.refusals);
                }
                let refusal = plan.refusals.iter().find(|refusal| refusal.code == if first_role == 0 && second_role == 0 { "data_disks_same_device" } else { "disk_repeated" }).unwrap();
                assert_eq!(
                    refusal.code,
                    if first_role == 0 && second_role == 0 {
                        "data_disks_same_device"
                    } else {
                        "disk_repeated"
                    }
                );
                assert_eq!(refusal.disk_id, second.disk_id);
                assert_eq!(refusal.disk_name, second.name);
                for role in [first_role, second_role] {
                    assert!(
                        refusal.detail.contains(["data", "parity", "cache"][role]),
                        "{}",
                        refusal.detail
                    );
                }
                assert!(
                    refusal.detail.contains("sdy") && refusal.detail.contains("sdz"),
                    "{}",
                    refusal.detail
                );
                assert!(plan.steps_preview.is_empty(), "{}", plan.steps_preview);
                assert!(plan.wiped_devices.is_empty(), "{:?}", plan.wiped_devices);
            }
        }
    }

    #[test]
    fn layout_plan_accepts_distinct_disks_with_or_without_hardware_identifiers() {
        for known_identity in [false, true] {
            for parity_count in 0..=2 {
                for cache_count in 0..=1 {
                    let mut selected: Vec<NasDisk> = ["sdx", "sdy", "sdz", "sdu", "sdv", "sdw"]
                        .into_iter()
                        .map(|name| disk(name, 8 * TB))
                        .collect();
                    for disk in &mut selected {
                        disk.wwn = Some(if known_identity {
                            format!("wwn-{}", disk.name)
                        } else {
                            String::new()
                        });
                        if known_identity {
                            disk.serial = format!("serial-{}", disk.name);
                        }
                    }

                    let plan = plan_layout(
                        "media",
                        "xfs",
                        &selected[..2],
                        &selected[2..2 + parity_count],
                        &selected[4..4 + cache_count],
                        &BTreeSet::new(),
                        &BTreeSet::new(),
                        &[],
                        &Tools::for_preview(),
                    );

                    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
                    assert!(!plan.steps_preview.is_empty());
                    assert_eq!(plan.wiped_devices.len(), 2 + parity_count + cache_count);
                }
            }
        }
    }

    #[test]
    fn layout_plan_rejects_more_than_one_cache_disk() {
        let disks: Vec<NasDisk> = ["sdx", "sdy", "sdz"].into_iter().map(|name| disk(name, 8 * TB)).collect();
        let plan = plan_layout(
            "media",
            "xfs",
            &disks[..1],
            &[],
            &disks[1..],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            &Tools::for_preview(),
        );
        assert!(plan.refusals.iter().any(|r| r.code == "too_many_cache_disks"));
    }

    #[test]
    fn layout_plan_refuses_partitions_and_shared_wwn_before_planning_writes() {
        let mut first = disk("sdz1", 8 * TB);
        let mut second = disk("sdz2", 8 * TB);
        let unknown = plan_layout(
            "media",
            "xfs",
            &[first.clone()],
            &[second.clone()],
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            &Tools::for_preview(),
        );
        assert_eq!(unknown.refusals.len(), 1, "{:?}", unknown.refusals);
        assert_eq!(unknown.refusals[0].code, "plan_failed");
        assert_eq!(
            unknown.refusals[0].detail,
            "this layout cannot be turned into a plan: invalid argument: device '/dev/sdz1' is not a whole-disk block device"
        );
        assert!(unknown.steps_preview.is_empty());
        assert!(unknown.wiped_devices.is_empty());

        first.wwn = Some("0x5000c500a1b2c3d4".to_string());
        second.wwn = first.wwn.clone();
        let identified = plan_layout(
            "media",
            "xfs",
            &[first],
            &[second],
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            &Tools::for_preview(),
        );
        assert_eq!(identified.refusals.len(), 1, "{:?}", identified.refusals);
        assert_eq!(identified.refusals[0].code, "disk_repeated");
        assert!(identified.steps_preview.is_empty());
        assert!(identified.wiped_devices.is_empty());
    }

    /// Two inventory rows that are one piece of hardware are refused as data
    /// disks.
    ///
    /// MEASURED (2026-09-06, snapraid 14.7): snapraid refuses this itself —
    /// `Disks 'X' and 'Y' are on the same device.` — but only on the first
    /// sync, which is after every disk has been erased. `disk_repeated` does
    /// not catch it, because the two rows have different ids; what they share
    /// is a WWN, a serial or a path.
    #[test]
    fn two_views_of_one_disk_cannot_both_be_data_disks() {
        let mut a = disk("sdg", 8 * TB);
        let mut b = disk("sdh", 8 * TB);
        a.wwn = Some("0x5000c500a1b2c3d4".to_string());
        b.wwn = Some("0x5000c500a1b2c3d4".to_string());
        let refusals = layout_refusals(
            "media",
            &[a.clone(), b.clone()],
            &[],
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
        );
        let same = refusals
            .iter()
            .find(|r| r.code == "data_disks_same_device")
            .expect("one device behind two rows has to be refused");
        assert!(same.detail.contains("sdg") && same.detail.contains("sdh"), "{}", same.detail);
        assert!(same.detail.contains("WWN"), "the shared identity is named: {}", same.detail);
        // The ids differ, so `disk_repeated` genuinely does not see this —
        // which is the reason the new rule exists.
        assert!(!refusals.iter().any(|r| r.code == "disk_repeated"), "{refusals:?}");

        // A serial and a bare device path catch it too.
        let mut a2 = disk("sdg", 8 * TB);
        let mut b2 = disk("sdh", 8 * TB);
        a2.serial = "ZR18AB3F".to_string();
        b2.serial = "ZR18AB3F".to_string();
        assert!(same_device(&a2, &b2).is_some());
        let mut c = disk("sdg", 8 * TB);
        c.name = "sdz".to_string();
        assert!(same_device(&disk("sdg", 8 * TB), &c).is_some(), "same /dev path");

        // …and two ordinary disks that simply report nothing are NOT the same
        // device. This is the assertion that keeps the rule from refusing
        // every array on hardware with no WWN or serial.
        let plain_a = disk("sdg", 8 * TB);
        let mut plain_b = disk("sdh", 8 * TB);
        plain_b.path = "/dev/sdh".to_string();
        assert_eq!(plain_a.wwn, None);
        assert!(plain_a.serial.is_empty() && plain_b.serial.is_empty());
        assert_eq!(same_device(&plain_a, &plain_b), None);
        assert!(layout_refusals(
            "media",
            &[plain_a, plain_b],
            &[],
            &[],
            &BTreeSet::new(),
            &BTreeSet::new()
        )
        .is_empty());
    }

    /// The health probe asks the binary to do work, and reads the ONE thing
    /// that separates a working snapraid from a crashing one.
    ///
    /// MEASURED (2026-09-06) against both builds with this configuration: the
    /// healthy one exits 1 (`You must have at least 2 'content' files in
    /// different disks.`), the segfaulting one is killed with 139. So exit 1
    /// is the NORMAL, HEALTHY answer here and must never read as broken.
    #[test]
    fn the_snapraid_health_probe_separates_crashing_from_merely_complaining() {
        // The healthy build's measured answer. This assertion is the whole
        // point: an implementation that called non-zero "broken" would mark
        // every healthy node broken, and it is the only assertion here that
        // fails against that implementation.
        assert_eq!(
            probe_verdict(1, "Self-test...\nYou must have at least 2 'content' files in different disks.", ""),
            ToolHealth::Working,
            "exit 1 is what a WORKING snapraid returns on this probe"
        );
        assert_eq!(probe_verdict(0, "Self-test...", ""), ToolHealth::Working);

        for (code, stdout, stderr) in [
            (1, "Self-test...", ""),
            (1, "", "cannot read configuration"),
            (2, "", ""),
            (127, "", ""),
        ] {
            assert!(matches!(
                probe_verdict(code, stdout, stderr),
                ToolHealth::Unknown(_)
            ));
        }

        // The broken build's measured answer, in both spellings a signal
        // reaches us by.
        for code in [139, -1] {
            let ToolHealth::Broken(why) = probe_verdict(code, "Self-test...", "") else {
                panic!("a snapraid killed by a signal is not working (code {code})");
            };
            assert!(why.contains("-O2"), "the fix has to be in the sentence: {why}");
            assert!(
                why.contains("look protected"),
                "and so has the consequence: {why}"
            );
        }
        // 139 is named as a signal, -1 is not (we do not know which one).
        let ToolHealth::Broken(why) = probe_verdict(139, "", "") else {
            unreachable!()
        };
        assert!(why.contains("signal 11"), "SIGSEGV should be named: {why}");

        // `Self-test...` is NOT the discriminator: both builds print it, so a
        // verdict that keyed on it would call the crashing build healthy.
        assert_eq!(probe_verdict(139, "Self-test...", ""), probe_verdict(139, "", ""));
    }

    /// The probe's configuration is one snapraid gets far enough into to
    /// crash — which is the only thing that makes the probe worth running.
    #[test]
    fn the_probe_config_reaches_snapraids_own_diagnostics() {
        let dir = std::path::Path::new("/tmp/probe");
        let text = probe_config(dir);
        // Read with `starts_with`, never `contains`: the version that used
        // `contains` could not see nine leading spaces on every line, and did
        // not.
        let directive = |name: &str| text.lines().filter(|l| l.starts_with(name)).count();
        assert_eq!(directive("parity "), 1, "no parity line, no probe:\n{text}");
        assert_eq!(directive("content "), 1, "{text}");
        assert_eq!(directive("data "), 1, "{text}");
        assert_eq!(directive("blocksize "), 1, "{text}");
        assert!(
            text.lines().all(|l| l.is_empty() || !l.starts_with(char::is_whitespace)),
            "no directive may be indented:\n{text}"
        );
        assert!(text.contains("/tmp/probe/"), "{text}");

        // And the argv asks for `status`, NOT `--version`: MEASURED
        // (2026-09-06) the segfaulting build answers `--version` happily.
        let args = probe_args(&dir.join("probe.conf"));
        assert_eq!(args.last().map(String::as_str), Some("status"));
        assert!(args.contains(&"-c".to_string()));
        assert!(!args.iter().any(|a| a == "--version"));
    }

    /// One disk picked twice is refused before the plan can format it twice.
    #[test]
    fn one_disk_in_two_roles_is_refused() {
        let d = disk("sdg", 8 * TB);
        let refusals = layout_refusals(
            "media",
            std::slice::from_ref(&d),
            std::slice::from_ref(&d),
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
        );
        assert!(
            refusals.iter().any(|r| r.code == "disk_repeated"),
            "{refusals:?}"
        );
    }

    /// The wizard's summary numbers, and the rule that a refused layout gets
    /// no plan.
    #[test]
    fn the_plan_sizes_the_array_and_withholds_the_steps_when_it_refuses() {
        let data = vec![disk("sdl", 8 * TB), disk("sdn", 4 * TB)];
        let parity = vec![disk("sdm", 8 * TB)];
        let tools = Tools::for_preview();

        let plan = plan_layout(
            "archiwum",
            "xfs",
            &data,
            &parity,
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            &tools,
        );
        assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
        // n08b's summary: 8 TB + 4 TB = 12 TB usable, parity adds none.
        assert_eq!(plan.usable_bytes, 12 * TB);
        assert_eq!(plan.parity_bytes, 8 * TB);
        assert_eq!(plan.raw_bytes, 20 * TB);
        assert_eq!(plan.fault_tolerance, 1);
        assert_eq!(plan.union_path, "/mnt/archiwum");
        // The red button's count comes from the plan, so it names exactly the
        // three disks the plan formats. The device STRINGS are not asserted
        // literally: `branch_device` prefers a `/dev/disk/by-id/…` link and
        // falls back to the kernel name, so what they look like depends on
        // the machine running the test. What must hold on every machine is
        // that there are three of them and each names its disk.
        let wiped = plan.wiped_devices.clone();
        assert_eq!(wiped.len(), 3, "{wiped:?}");
        // Compared against what the plan's OWN resolver returns for the same
        // three disks. The previous guard was `d.contains("sdl")`, which reads
        // as machine-independent and is not: `stable_device_path` prefers a
        // `/dev/disk/by-id/…` link, and such a link carries the disk's WWN and
        // never its kernel name. It passed only on a host where sdl/sdm/sdn do
        // not exist and the fallback returned `/dev/sdl`. MEASURED on the
        // owner's node (2026-09-15), where those three names are real array
        // members: `sdl is not among the disks the plan erases:
        // ["/dev/disk/by-id/wwn-0x5000cca0bef1bcce", …]`.
        let mut expected: Vec<String> =
            [disk("sdl", 8 * TB), disk("sdn", 4 * TB), disk("sdm", 8 * TB)]
                .iter()
                .map(branch_device)
                .collect();
        expected.sort();
        let mut got = wiped.clone();
        got.sort();
        assert_eq!(got, expected, "the plan erases exactly the three disks it was given");
        assert!(plan.steps_preview.contains("WIPE"), "{}", plan.steps_preview);
        assert!(
            plan.steps_preview.contains("/mnt/archiwum"),
            "the union has to be in the plan: {}",
            plan.steps_preview
        );

        // A refused layout: no steps, no wipe list, and the refusal survives.
        let small = vec![disk("sdo", 4 * TB)];
        let plan = plan_layout(
            "archiwum",
            "xfs",
            &data,
            &small,
            &[],
            &BTreeSet::new(),
            &BTreeSet::new(),
            &[],
            &tools,
        );
        assert!(!plan.refusals.is_empty());
        assert!(
            plan.steps_preview.is_empty() && plan.wiped_devices.is_empty(),
            "a refused layout must not be shown as one click away"
        );
        // The sizing is still answered, because the wizard shows it next to
        // the refusal.
        assert_eq!(plan.usable_bytes, 12 * TB);
    }

    /// A filesystem this node will not make is a REFUSAL, not a blank preview.
    ///
    /// The protocol says an empty `refusals` means the create button is live.
    /// The version without these checks let `btrfs` through every rule, fail
    /// inside the helper, and come back with no refusals at all — an enabled
    /// button over a plan the node had already declined to build. Each
    /// assertion below fails against that version.
    #[test]
    fn a_filesystem_this_node_cannot_make_stops_the_create_button() {
        let data = vec![disk("sdl", 8 * TB)];
        let tools = Tools::for_preview();
        let plan = |fs: &str, available: &[String]| {
            plan_layout(
                "archiwum",
                fs,
                &data,
                &[],
                &[],
                &BTreeSet::new(),
                &BTreeSet::new(),
                available,
                &tools,
            )
        };

        let refused = plan("btrfs", &[]);
        assert!(
            refused.refusals.iter().any(|r| r.code == "filesystem_invalid"),
            "{:?}",
            refused.refusals
        );
        assert!(
            refused.steps_preview.is_empty() && refused.wiped_devices.is_empty(),
            "a refused layout is never one click away"
        );

        // Known filesystem, absent mkfs: a different code, because it is a
        // different thing to fix — install a package, not pick another
        // filesystem.
        let unavailable = plan("ext4", &["xfs".to_string()]);
        assert!(
            unavailable
                .refusals
                .iter()
                .any(|r| r.code == "filesystem_unavailable"),
            "{:?}",
            unavailable.refusals
        );
        assert!(unavailable.refusals.iter().all(|r| r.code != "filesystem_invalid"));

        // Available: no refusal at all, and the plan is built.
        let ok = plan("ext4", &["xfs".to_string(), "ext4".to_string()]);
        assert!(ok.refusals.is_empty(), "{:?}", ok.refusals);
        assert!(!ok.steps_preview.is_empty());

        // An empty availability list means nobody probed, and that must not
        // refuse everything — otherwise a node whose Environment tab has not
        // run yet could never create an array.
        let unprobed = plan("ext4", &[]);
        assert!(unprobed.refusals.is_empty(), "{:?}", unprobed.refusals);

        // An empty filesystem is the wizard's default, not an error.
        let defaulted = plan("", &[]);
        assert!(defaulted.refusals.is_empty(), "{:?}", defaulted.refusals);
        assert!(defaulted.steps_preview.contains("mkfs.xfs"), "{}", defaulted.steps_preview);
    }

    /// The union's REAL top level, joined with the policies this node stored.
    ///
    /// A tempdir stands in for `/mnt/<array>`: `union_path` is a constant
    /// `/mnt/` away from the array name, so the discovery is given the path
    /// rather than the name and this is the same call the store makes.
    #[test]
    fn the_folder_list_is_the_union_joined_with_the_stored_policies() {
        let union = tempfile::TempDir::new().expect("tempdir");
        for name in ["filmy", "foto", "backup", "lost+found"] {
            std::fs::create_dir(union.path().join(name)).expect("mkdir");
        }
        // Files at the union root are not folders — snapraid's own content
        // file sits there on every array with parity.
        std::fs::write(union.path().join("snapraid.content"), b"x").expect("write");
        let stored = BTreeMap::from([
            ("foto".to_string(), "only".to_string()),
            ("backup".to_string(), "no".to_string()),
        ]);
        let shares = BTreeMap::from([(
            "filmy".to_string(),
            ("share-1".to_string(), "Filmy".to_string()),
        )]);
        let (folders, known) = folders_of(union.path(), &stored, &shares);
        assert!(known, "a readable union is a known folder list");
        assert_eq!(
            folders.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["backup", "filmy", "foto"],
            "lost+found belongs to the filesystem, not to the admin"
        );
        // The clause: a folder nobody decided for is `yes`, and that is a
        // DEFAULT and not a stored decision.
        let filmy = folders.iter().find(|f| f.name == "filmy").expect("filmy");
        assert_eq!(filmy.cache_policy, "yes");
        assert_eq!(filmy.share_id, "share-1");
        assert_eq!(filmy.share_label, "Filmy");
        assert!(folders.iter().all(|f| f.name == "filmy" || f.share_id.is_empty()));

        let mut row = array();
        row.folders = folders;
        row.folders_known = known;
        let rules = row.mover_rules();
        assert_eq!(rules.pinned_folders, vec!["foto".to_string()]);
        assert_eq!(rules.eager_folders, vec!["backup".to_string()]);
        rules.validate().expect("the helper accepts what discovery produced");
    }

    /// The distinction the rest of this file makes about bytes, made about
    /// folders: §3.4 forbids fstab, so before TentaNas mounts the branches
    /// `/mnt/<array>` is an ordinary EMPTY directory of the root filesystem —
    /// and a `read_dir` of it succeeds. Reading that as "this array has no
    /// folders" would publish an empty table for an array full of them.
    #[test]
    fn an_unreadable_union_is_unknown_and_never_zero_folders() {
        let stored = BTreeMap::from([("foto".to_string(), "only".to_string())]);
        let shares = BTreeMap::new();

        // 1. The union directory does not exist at all.
        let missing = std::path::Path::new("/nonexistent-union-tentanas-test");
        let (folders, known) = folders_of(missing, &stored, &shares);
        assert!(!known, "a union that cannot be read is unknown");
        // The stored policy survives it: the pin is an intention, and dropping
        // it because a mount is not up would silently unpin the folder.
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].cache_policy, "only");

        // 2. The union directory exists and is empty — the cold-boot shape,
        //    and the one that reads as a successful measurement of zero.
        let empty = tempfile::TempDir::new().expect("tempdir");
        let (cold, cold_known) = folders_of(empty.path(), &stored, &shares);
        assert!(!cold_known, "an empty read is unknown, never zero folders");
        assert_eq!(cold.len(), 1, "the stored pin is still listed");
        let mut row = array();
        row.folders = cold;
        row.folders_known = cold_known;
        assert_eq!(row.mover_rules().pinned_folders, vec!["foto".to_string()]);

        // 3. And with nothing stored either, the answer is still unknown and
        //    NOT an array whose folder list is empty.
        let (none, none_known) = folders_of(empty.path(), &BTreeMap::new(), &shares);
        assert!(none.is_empty());
        assert!(!none_known, "no rows AND no measurement is unknown");
    }

    /// Past the cap the listing is a truncation, and a truncated list read as
    /// "these are the folders" is what would hide the one an admin came to
    /// pin. `PARITY_ERRORS_MAX_ROWS` answers unknown at its bound for the
    /// same reason.
    #[test]
    fn a_union_past_the_cap_answers_unknown_rather_than_a_truncated_list() {
        let union = tempfile::TempDir::new().expect("tempdir");
        for i in 0..=FOLDERS_MAX_ROWS {
            std::fs::create_dir(union.path().join(format!("f{i}"))).expect("mkdir");
        }
        let (folders, known) = folders_of(union.path(), &BTreeMap::new(), &BTreeMap::new());
        assert!(!known);
        assert!(folders.is_empty());
    }

    /// The two refusals a per-folder policy can meet, and why they may never
    /// be the same answer.
    #[test]
    fn a_folder_policy_tells_an_unknown_folder_from_an_unreadable_list() {
        let mut row = array();
        row.folders = vec![FolderRow {
            name: "foto".to_string(),
            cache_policy: "only".to_string(),
            ..Default::default()
        }];
        row.folders_known = true;
        assert_eq!(folder_policy_refusal(&row, "foto"), None);
        assert_eq!(
            folder_policy_refusal(&row, "nie-ma"),
            Some(FolderPolicyRefusal::NoSuchFolder)
        );

        row.folders_known = false;
        // The stored folder still passes: taking a pin back off must not
        // depend on a mount being up.
        assert_eq!(folder_policy_refusal(&row, "foto"), None);
        assert_eq!(
            folder_policy_refusal(&row, "filmy"),
            Some(FolderPolicyRefusal::FoldersUnknown),
            "an unreadable list must never answer 'no such folder'"
        );
    }

    /// The names a folder may have are the helper's, not this file's: a name
    /// stored here becomes a mover rule the helper validates.
    #[test]
    fn a_folder_name_is_one_segment_under_the_union() {
        for good in ["foto", "Moje Filmy", ".ukryty", "a-b_c.d"] {
            assert!(folder_name_valid(good), "{good}");
        }
        for bad in ["", ".", "..", "a/b", "/foto", "foto\n", "foto\0", &"x".repeat(256)] {
            assert!(!folder_name_valid(bad), "{bad:?}");
        }
    }

    /// The row is the single source of the spec, and the spec is what the
    /// plan is built from — so a folder's cache policy reaches the mover
    /// rules and the cache reaches the union.
    #[test]
    fn the_row_produces_a_spec_whose_union_holds_the_cache_and_the_data_disks() {
        let a = array();
        let spec = a.spec();
        let branches = spec.branch_specs();
        assert_eq!(branches.len(), 3);
        assert!(branches[0].contains("/cache/nvme2n1"), "{branches:?}");
        assert!(branches[0].ends_with("=RW"), "{branches:?}");
        assert!(
            branches[1..].iter().all(|b| b.ends_with("=NC")),
            "with a cache present new files go to the cache: {branches:?}"
        );
        assert_eq!(spec.parity.len(), 1);
        assert_eq!(spec.snapraid.scrub_percent, 8);

        // The folders' cache policies become mover rules and nothing else —
        // mergerfs cannot express them.
        let rules = a.mover_rules();
        assert_eq!(rules.pinned_folders, vec!["foto".to_string()]);
        assert_eq!(rules.eager_folders, vec!["backup".to_string()]);
        assert_eq!(rules.min_age_secs, 7_200);
        assert_eq!(rules.min_free_pct, 20);
        assert!(
            rules.skip_open_files,
            "a file its writer holds open is never moved out from under it"
        );

        // And the coupled sync really is one job with the move.
        let steps = tentanas_helper::elastic::plan_mover(
            &spec,
            &rules,
            a.mover.coupled_sync,
            &Tools::for_preview(),
        )
        .expect("plan");
        assert_eq!(steps.len(), 2, "the move and the sync: {steps:?}");
    }

    /// An array owns every alert key it could open, so dissolving it can
    /// close all of them — including the per-disk ones.
    #[test]
    fn an_array_owns_one_alert_key_per_disk_plus_its_two_array_wide_ones() {
        let a = array();
        let keys = alert_keys(&a);
        assert!(keys.len() >= 6, "too few keys: {keys:?}");
        assert!(keys.contains(&protection_alert_key("media")));
        assert!(keys.contains(&parity_alert_key("media")));
        for name in ["sdg", "sdh", "nvme2n1", "sdj"] {
            assert!(
                keys.contains(&branch_alert_key("media", name)),
                "{name} has no key: {keys:?}"
            );
        }
        // Keys are per array as well as per disk: two arrays with a disk of
        // the same name do not share one row.
        assert_ne!(branch_alert_key("media", "sdg"), branch_alert_key("archiwum", "sdg"));
    }

    /// The wire shape carries the measurements it has and admits the ones it
    /// does not.
    #[test]
    fn the_protocol_shape_carries_unknowns_as_unknowns() {
        let a = array();
        let mut disks = BTreeMap::new();
        for name in ["sdg", "sdh", "sdj"] {
            let mut d = disk(name, 4 * TB);
            d.health = "ok".to_string();
            disks.insert(format!("id-{name}"), d);
        }
        // nvme2n1 is deliberately absent from the inventory: a disk the node
        // cannot see is 'unknown', never 'ok'.
        let observed = all_mounted(&a, 18 * 1024 * 1024 * 1024);
        let state = array_state(&a, &observed, &installed_all);
        let wire = to_protocol(&a, &disks, &observed, true, "12.3", (state.0, state.1.text.as_str()));

        assert_eq!(wire.kind, "elastic-array");
        assert_eq!(wire.union_path, "/mnt/media");
        assert_eq!(wire.data_disks.len(), 2);
        assert_eq!(wire.cache_disks.len(), 1);
        assert_eq!(wire.parity_disks.len(), 1);
        assert_eq!(wire.cache_disks[0].health, "unknown");
        assert_eq!(wire.data_disks[0].health, "ok");
        assert_eq!(
            wire.parity_disks[0].parity_file,
            "/mnt/tentanas-branches/media/parity/1/snapraid.parity"
        );
        // Capacity is the DATA disks only: parity adds none and the cache is
        // not capacity. And it is the SUM of the branches: MEASURED
        // (2026-09-06, mergerfs 2.42.0) `df` on the union would have reported
        // one branch, i.e. 4 TB, so a union-derived figure and this one
        // differ by a whole disk — which is what makes this assertion able to
        // see the mistake at all.
        assert_eq!(wire.usable_bytes, Some(8 * TB));
        assert_eq!(
            wire.data_disks.len(),
            2,
            "two branches of 4 TB each: the total is not any single branch"
        );
        assert!(wire.data_disks.iter().all(|b| b.size_bytes == Some(4 * TB)));
        assert_eq!(wire.cache_size_bytes, Some(1_000_000_000_000));
        assert_eq!(wire.protection.cache_unprotected_bytes, Some(18 * 1024 * 1024 * 1024));
        assert_eq!(wire.snapraid.parity_errors, Some(0));
        assert_eq!(wire.snapraid.config_path, "/etc/tentanas/snapraid-media.conf");
        assert_eq!(wire.folders.len(), 3);
        assert_eq!(wire.folders[0].path, "/mnt/media/filmy");

        // One unreadable data branch makes the CAPACITY unknown rather than
        // smaller — a total that silently dropped a disk would be wrong in the
        // direction nobody notices.
        let mut partial = observed.clone();
        partial.probes.insert(
            data_branch_path("media", "sdh"),
            BranchProbe {
                mounted: Some(true),
                device_present: Some(true),
                size_bytes: None,
                used_bytes: None,
                free_bytes: None,
            },
        );
        let wire = to_protocol(&a, &disks, &partial, true, "12.3", ("active", ""));
        assert_eq!(wire.usable_bytes, None);
        assert_eq!(wire.used_bytes, None);
    }

    #[test]
    fn protocol_keeps_data_capacity_when_cache_measurement_is_unknown() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        let db = Arc::new(crate::db::Db::from_connection(conn));
        let mut spec = create_spec("saved-cache-observation");
        let mut cache = spec.data[0].clone();
        cache.disk_id = "saved-cache-observation-cache".into();
        cache.wwn = Some("wwn-saved-cache-observation-cache".into());
        cache.serial = Some("serial-saved-cache-observation-cache".into());
        cache.expected_uuid = uuid::Uuid::new_v4().to_string();
        spec.cache = Some(cache);
        spec.validate().unwrap();
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&db, &job, Some(&jobs::ElasticJobIntent::Create(spec.clone()))).unwrap();
        let saved = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        let mut result = ready_result(&spec);
        let measured_cache = result.disks.iter_mut().find(|disk| disk.role == ElasticRole::Cache).unwrap();
        measured_cache.size_bytes = None;
        measured_cache.used_bytes = None;
        measured_cache.free_bytes = None;
        let wire = observed_protocol(&saved, &BTreeMap::new(), Ok(result), &[]);
        assert_eq!(wire.usable_bytes, Some(spec.data[0].bytes));
        assert_eq!(wire.cache_size_bytes, None);
        assert_eq!(wire.cache_used_bytes, None);
    }

    /// The card's one status, and what it is allowed to say.
    ///
    /// The point of the test is the ORDER: an unmounted data disk outranks
    /// everything else, because a union with a hole in it is the fault an
    /// admin has to see first — and a green card over one would be the
    /// quietest possible way to lose files.
    #[test]
    fn the_card_status_reports_the_worst_thing_it_can_see() {
        let a = array();
        let disks: BTreeMap<String, NasDisk> = ["sdg", "sdh", "sdj", "nvme2n1"]
            .into_iter()
            .map(|n| (format!("id-{n}"), disk(n, 4 * TB)))
            .collect();
        let healthy = all_mounted(&a, 18 * 1024 * 1024 * 1024);
        let wire = to_protocol(&a, &disks, &healthy, true, "12.3", ("active", ""));
        assert_eq!(
            wire.health, "ok",
            "an array between mover runs is healthy, not degraded: {}",
            wire.health_reason
        );

        // A parity disk that is not mounted is a warning: the array serves,
        // but nothing can be synced, so the window never closes.
        let mut parity_down = healthy.clone();
        parity_down
            .probes
            .insert(parity_mount_path("media", 1), BranchProbe::device_gone());
        let wire = to_protocol(&a, &disks, &parity_down, true, "12.3", ("error", ""));
        assert_eq!(wire.health, "warning");
        assert!(wire.health_reason.contains("sdj"), "{}", wire.health_reason);

        // A data disk that is not mounted outranks it.
        let mut data_down = parity_down.clone();
        data_down
            .probes
            .insert(data_branch_path("media", "sdh"), BranchProbe::device_gone());
        let wire = to_protocol(&a, &disks, &data_down, true, "12.3", ("error", ""));
        assert_eq!(wire.health, "critical");
        assert!(wire.health_reason.contains("sdh"), "{}", wire.health_reason);

        // And a node that could not measure says so rather than reporting ok.
        let mut blind = healthy.clone();
        blind.mount_table_known = false;
        let wire = to_protocol(&a, &disks, &blind, true, "12.3", ("unknown", ""));
        assert_eq!(wire.health, "unknown");
        assert!(!wire.health_reason.is_empty());
    }

    /// Capabilities come from the probes, and the reason a capability is
    /// false travels with it.
    #[test]
    fn capabilities_report_what_was_probed_and_why_not() {
        let features = vec![
            FeatureState {
                id: MERGERFS_FEATURE_ID.to_string(),
                status: "ok".to_string(),
                version: Some("2.40.2".to_string()),
                ..Default::default()
            },
            FeatureState {
                id: SNAPRAID_FEATURE_ID.to_string(),
                status: "missing".to_string(),
                detail: "missing: snapraid".to_string(),
                ..Default::default()
            },
        ];
        let caps = capabilities(&features, &|fs| fs == "xfs");
        assert!(caps.mergerfs);
        assert_eq!(caps.mergerfs_version, "2.40.2");
        assert!(!caps.snapraid);
        assert!(caps.snapraid_version.is_empty());
        assert_eq!(caps.filesystems, vec!["xfs".to_string()]);
        assert!(caps.detail.contains("missing: snapraid"), "{}", caps.detail);

        // A node that was never probed is not a node that has the tools.
        let caps = capabilities(&[], &|_| true);
        assert!(!caps.mergerfs && !caps.snapraid);
        assert!(caps.detail.contains("not probed"), "{}", caps.detail);

        // No mkfs at all is its own sentence, because it stops the wizard
        // before either tool matters.
        let caps = capabilities(&features, &|_| false);
        assert!(caps.filesystems.is_empty());
        assert!(caps.detail.contains("mkfs"), "{}", caps.detail);
    }

    /// The free-disk list is the wizard's candidate list, and it excludes
    /// everything `conflicting_owner` names.
    #[test]
    fn the_candidate_list_excludes_every_disk_that_belongs_to_something() {
        let mut pooled = disk("sda", 8 * TB);
        pooled.role = "pool_member".to_string();
        let mut mounted = disk("sdb", 8 * TB);
        mounted.role = "mounted".to_string();
        mounted.mountpoints = vec!["/srv".to_string()];
        let free_one = disk("sdc", 8 * TB);
        let claimed = disk("sdd", 8 * TB);
        let all = vec![pooled, mounted, free_one, claimed];
        let taken: BTreeSet<String> = ["id-sdd".to_string()].into_iter().collect();

        let free = free_disks(&all, &taken);
        assert_eq!(free.len(), 1, "{free:?}");
        assert_eq!(free[0].name, "sdc");
    }
    // ----- import: adopting an array this node holds but has no record of ---

    /// The journal of a two-data + parity array, owned by the identity the
    /// addon had BEFORE it was re-provisioned.
    fn lost_journal(name: &str) -> ElasticJournalEntry {
        let mut spec = create_spec(name);
        spec.data.push(ElasticDiskSpec {
            disk_id: format!("{name}-data2"),
            wwn: Some(format!("wwn-{name}-data2")),
            serial: Some(format!("serial-{name}-data2")),
            bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        });
        spec.owner = ElasticOwner { org_id: "orgtentanas-rig11".into(), addon_id: "addontentanas".into() };
        ElasticJournalEntry { spec, union_mounted: Some(true) }
    }

    /// The live inventory a member of `entry` would produce: same hardware
    /// identities, and the filesystem UUID the journal recorded.
    fn member_disks(entry: &ElasticJournalEntry) -> Vec<NasDisk> {
        member_names(&entry.spec)
            .into_iter()
            .enumerate()
            .map(|(i, (_, member))| {
                let mut live = disk(&format!("sd{}", (b'a' + i as u8) as char), member.bytes);
                live.disk_id = member.disk_id.clone();
                live.wwn = member.wwn.clone();
                live.serial = member.serial.clone().unwrap_or_default();
                live.role = "used".to_string();
                live.fs_type = Some("xfs".to_string());
                live.fs_uuid = Some(member.expected_uuid.clone());
                live
            })
            .collect()
    }

    /// No organisation of this node other than the asking one: every other
    /// org is one the node has never heard of (disks moved in).
    fn no_orgs() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn this_addon() -> ElasticOwner {
        ElasticOwner { org_id: "org-default".into(), addon_id: "tentanas-8dd19dc4".into() }
    }

    #[test]
    fn a_lost_array_whose_every_member_still_carries_its_uuid_is_importable_and_says_it_re_owns() {
        let entry = lost_journal("media");
        let disks = member_disks(&entry);
        let owner = this_addon();

        let candidates = import_candidates(&[entry.clone()], &[], &disks, &[], &owner, &no_orgs());
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(candidate.status, "importable");
        assert_eq!(candidate.disks_matched, 3, "2 data + 1 parity");
        assert_eq!((candidate.data_disks, candidate.parity_disks, candidate.cache_disks), (2, 1, 0));
        assert!(candidate.disks_missing.is_empty() && candidate.disks_reused.is_empty());
        assert!(candidate.union_mounted, "the union is still published");
        // The dialog has to be able to say whose storage this is — in words.
        assert!(candidate.owner_foreign);
        // Another organisation's journal: a code, and nothing of that tenant
        // — no name, and not its ids either, not even for a tooltip.
        assert_eq!(candidate.owner_kind, OWNER_KIND_OTHER_INSTALLATION);
        assert_eq!(candidate.owner_instance_name, "");
        assert_eq!((candidate.owner_org_id.as_str(), candidate.owner_addon_id.as_str()), ("", ""));
        assert!(candidate.detail.contains("re-owns"), "{}", candidate.detail);
        assert!(
            !candidate.detail.contains("addontentanas") && !candidate.detail.contains("orgtentanas"),
            "no owner id in the sentence: {}",
            candidate.detail
        );
        assert!(import_selection(&candidates, &candidate.array_id, "media").is_ok());
    }

    #[test]
    fn a_member_whose_filesystem_uuid_changed_refuses_the_adoption_and_is_named_as_reused() {
        let entry = lost_journal("media");
        let mut disks = member_disks(&entry);
        // The disk came back — same serial, same WWN — carrying somebody
        // else's filesystem. Hardware identity says "present", and that is
        // exactly the evidence an adoption may not act on.
        disks[0].fs_uuid = Some(uuid::Uuid::new_v4().to_string());
        let candidates = import_candidates(&[entry.clone()], &[], &disks, &[], &this_addon(), &no_orgs());
        let candidate = &candidates[0];

        assert_eq!(candidate.status, "incomplete");
        assert_eq!(candidate.disks_matched, 2);
        assert_eq!(candidate.disks_reused.len(), 1, "{:?}", candidate.disks_reused);
        assert!(candidate.disks_missing.is_empty(), "{:?}", candidate.disks_missing);
        // The disk is present, so it carries the name the Disks tab shows for
        // it, and its place in the array as data — never `d1 · <disk_id>`.
        let reused = &candidate.disks_reused[0];
        assert_eq!(
            (reused.name.as_str(), reused.role.as_str(), reused.index, reused.disk_name.as_str()),
            ("d1", "data", None, disks[0].name.as_str()),
            "the refusal names the disk"
        );
        let refusal = import_selection(&candidates, &candidate.array_id, "media")
            .expect_err("a changed UUID must refuse")
            .to_string();
        assert!(refusal.contains("2 z 3") && refusal.contains(&disks[0].name), "{refusal}");
        assert!(!refusal.contains("media-data"), "no disk id in the refusal: {refusal}");

        // And the same disk present with NO readable signature at all is not
        // read as a match either.
        disks[0].fs_uuid = None;
        disks[0].fs_type = None;
        let blank = import_candidates(&[entry], &[], &disks, &[], &this_addon(), &no_orgs());
        assert_eq!(blank[0].status, "incomplete");
        assert_eq!(blank[0].disks_matched, 2);
    }

    /// A7 of the second review: the scan does not hide a journal the node
    /// cannot load. One whose spec could still be read keeps its row and
    /// becomes unreadable with the helper's reason; one nothing could be read
    /// from is listed by its file name. Neither can be adopted.
    #[test]
    fn an_unreadable_journal_is_listed_with_its_reason_and_never_importable() {
        let entry = lost_journal("archiwum");
        let disks = member_disks(&entry);
        let lost = "4b4b4b4b-4b4b-4b4b-8b4b-4b4b4b4b4b4b";
        let unreadable = [
            ElasticUnreadableJournal { array_id: entry.spec.array_id.clone(), reason: "journal: unknown variant `exploded`".into() },
            ElasticUnreadableJournal { array_id: lost.into(), reason: "journal: EOF while parsing".into() },
        ];
        let candidates = import_candidates(&[entry.clone()], &unreadable, &disks, &[], &this_addon(), &no_orgs());
        assert_eq!(candidates.len(), 2);
        assert_eq!((candidates[0].status.as_str(), candidates[0].detail.as_str()), ("unreadable", "journal: unknown variant `exploded`"));
        assert_eq!(candidates[0].name, "archiwum");
        assert_eq!((candidates[1].array_id.as_str(), candidates[1].status.as_str()), (lost, "unreadable"));
        // The file name is the array's UUID, and a UUID is not a name.
        assert_eq!(candidates[1].name, "");
        assert!(candidates[1].detail.contains("EOF"));
        assert!(import_selection(&candidates, &entry.spec.array_id, "archiwum").is_err());
    }

    #[test]
    fn a_member_that_is_not_in_the_inventory_at_all_is_reported_missing_by_name() {
        let entry = lost_journal("archiwum");
        let mut disks = member_disks(&entry);
        let gone = disks.remove(1);
        let candidates = import_candidates(&[entry], &[], &disks, &[], &this_addon(), &no_orgs());
        let candidate = &candidates[0];

        assert_eq!(candidate.status, "incomplete");
        assert_eq!(candidate.disks_matched, 2);
        // Gone means no kernel name: its place in the array and the serial on
        // the drive, as data the dialog words — never a Polish sentence, and
        // never the journal's disk id.
        assert_eq!(
            candidate.disks_missing,
            vec![NasElasticImportMember {
                name: "d2".to_string(),
                role: "data".to_string(),
                index: None,
                disk_name: String::new(),
                serial: gone.serial.clone(),
            }]
        );
        assert!(candidate.disks_reused.is_empty());
        assert!(
            candidate.detail.contains("2 of 3") && candidate.detail.contains("cannot be found"),
            "{}",
            candidate.detail
        );
    }

    #[test]
    fn an_array_this_node_already_records_is_not_offered_and_neither_is_a_taken_name() {
        let entry = lost_journal("produkt");
        let disks = member_disks(&entry);
        let owner = this_addon();

        let by_id = import_candidates(
            &[entry.clone()],
            &[], &disks,
            &[(entry.spec.array_id.clone(), "produkt".to_string())],
            &owner,
            &no_orgs(),
        );
        assert_eq!(by_id[0].status, "already_known");
        assert!(import_selection(&by_id, &entry.spec.array_id, "produkt").is_err());

        // A DIFFERENT array already holds the name. The name column is UNIQUE
        // per node, so the adoption could not be written — and the sentence
        // has to say which of the two reasons it is.
        let by_name = import_candidates(
            &[entry.clone()],
            &[], &disks,
            &[("11111111-1111-4111-8111-111111111111".to_string(), "produkt".to_string())],
            &owner,
            &no_orgs(),
        );
        assert_eq!(by_name[0].status, "already_known");
        assert!(by_name[0].detail.contains("already uses this name"), "{}", by_name[0].detail);
    }

    /// The array that already holds the name may be ANOTHER organisation's
    /// (`known` is every owner's), so the clash sentence names no array: it
    /// used to repeat the name as "a different array named '…'", telling this
    /// tenant what another tenant had called its array.
    #[test]
    fn the_name_clash_sentence_names_no_array() {
        let entry = lost_journal("ksiegowosc");
        let clash = import_candidates(
            &[entry.clone()],
            &[], &member_disks(&entry),
            &[("22222222-2222-4222-8222-222222222222".to_string(), "ksiegowosc".to_string())],
            &this_addon(),
            &no_orgs(),
        );
        assert_eq!(clash[0].status, "already_known");
        assert!(!clash[0].detail.contains("ksiegowosc"), "{}", clash[0].detail);
        assert!(!clash[0].detail.contains("22222222"), "{}", clash[0].detail);
    }

    #[test]
    fn the_apply_gate_refuses_a_mistyped_name_and_an_array_the_fresh_scan_no_longer_sees() {
        let entry = lost_journal("media");
        let candidates = import_candidates(&[entry.clone()], &[], &member_disks(&entry), &[], &this_addon(), &no_orgs());

        let typo = import_selection(&candidates, &entry.spec.array_id, "Media")
            .expect_err("the retyped name must match exactly")
            .to_string();
        assert_eq!(typo, "the typed confirmation does not match the name");

        // Nothing is adopted on a name alone: the array is addressed by id,
        // and an id the fresh scan does not carry is gone.
        let vanished = import_selection(&candidates, "11111111-1111-4111-8111-111111111111", "media")
            .expect_err("an array with no journal cannot be adopted")
            .to_string();
        assert!(vanished.contains("nie widzi już dziennika"), "{vanished}");
    }

    #[test]
    fn a_journal_that_already_names_this_instance_is_importable_without_a_re_own_sentence() {
        // The database was lost without the addon being re-provisioned: the
        // journal owner is already ours, so there is nothing to re-own.
        let mut entry = lost_journal("media");
        entry.spec.owner = this_addon();
        let candidates = import_candidates(&[entry.clone()], &[], &member_disks(&entry), &[], &this_addon(), &no_orgs());

        assert_eq!(candidates[0].status, "importable");
        assert!(!candidates[0].owner_foreign, "this instance's own journal is not foreign");
        assert_eq!(candidates[0].owner_kind, OWNER_KIND_THIS_INSTANCE);
        assert!(!candidates[0].detail.contains("re-owns"), "{}", candidates[0].detail);
        assert!(candidates[0].detail.contains("already names this instance"), "{}", candidates[0].detail);
    }

    /// The owner ids reach the client only for the asking organisation's own
    /// journals; another installation's are blanked on the node.
    #[test]
    fn owner_ids_are_sent_only_for_an_owner_of_the_asking_organisation() {
        let mut entry = lost_journal("media");
        entry.spec.owner = ElasticOwner { org_id: this_addon().org_id, addon_id: "tentanas-old".into() };
        let candidates = import_candidates(&[entry.clone()], &[], &member_disks(&entry), &[], &this_addon(), &no_orgs());
        assert_eq!(candidates[0].owner_kind, OWNER_KIND_THIS_ORG);
        assert_eq!(
            (candidates[0].owner_org_id.as_str(), candidates[0].owner_addon_id.as_str()),
            ("org-default", "tentanas-old")
        );
        let other = ElasticOwner { org_id: "org-b".into(), addon_id: "nas-9".into() };
        assert_eq!(owner_ids_for(OWNER_KIND_OTHER_INSTALLATION, &other), (String::new(), String::new()));
        assert_eq!(owner_ids_for(OWNER_KIND_THIS_INSTANCE, &other), ("org-b".to_string(), "nas-9".to_string()));
    }

    /// A parity member keeps its level and a cache member its role, so the
    /// dialog can say "dysk parity 1" / "dysk cache" without a slot parser.
    #[test]
    fn every_member_place_carries_its_role_and_a_parity_level() {
        let entry = lost_journal("media");
        let places: Vec<_> = member_names(&entry.spec)
            .into_iter()
            .map(|(place, _)| (place.name, place.role, place.index, place.disk_name))
            .collect();
        assert_eq!(
            places,
            vec![
                ("d1".to_string(), "data".to_string(), None, String::new()),
                ("d2".to_string(), "data".to_string(), None, String::new()),
                ("parity1".to_string(), "parity".to_string(), Some(1), String::new()),
            ]
        );
    }

    /// The owner of a journal is a code judged against the asking instance,
    /// and the organisation decides first: another tenant's journal is
    /// "another installation" even under this very addon id.
    #[test]
    fn a_journal_owner_kind_is_decided_by_the_organisation_first() {
        let asking = ElasticOwner { org_id: "org-a".into(), addon_id: "nas-1".into() };
        let owner = |org: &str, addon: &str| ElasticOwner { org_id: org.into(), addon_id: addon.into() };
        assert_eq!(owner_kind(&owner("org-a", "nas-1"), &asking, &no_orgs()), OWNER_KIND_THIS_INSTANCE);
        assert_eq!(owner_kind(&owner("org-a", "nas-0"), &asking, &no_orgs()), OWNER_KIND_THIS_ORG);
        assert_eq!(owner_kind(&owner("org-b", "nas-1"), &asking, &no_orgs()), OWNER_KIND_OTHER_INSTALLATION);
        assert_eq!(owner_kind(&owner("org-b", "nas-9"), &asking, &no_orgs()), OWNER_KIND_OTHER_INSTALLATION);
    }

    /// The split of 2026-09-22: another organisation is judged by whether
    /// this node has a record of it. One it has is a tenant HERE (hidden);
    /// one it has not is somebody else's machine (visible, adoptable).
    #[test]
    fn another_org_is_other_org_on_node_only_when_this_node_has_a_record_of_it() {
        let asking = ElasticOwner { org_id: "org-a".into(), addon_id: "nas-1".into() };
        let owner = |org: &str, addon: &str| ElasticOwner { org_id: org.into(), addon_id: addon.into() };
        let orgs: BTreeSet<String> = ["org-a".to_string(), "org-b".to_string()].into();
        assert_eq!(owner_kind(&owner("org-b", "nas-1"), &asking, &orgs), OWNER_KIND_OTHER_ORG_ON_NODE);
        assert_eq!(owner_kind(&owner("org-c", "nas-1"), &asking, &orgs), OWNER_KIND_OTHER_INSTALLATION);
        // The asking org's own journals are never "another org", whatever
        // the set holds.
        assert_eq!(owner_kind(&owner("org-a", "nas-1"), &asking, &orgs), OWNER_KIND_THIS_INSTANCE);
        assert_eq!(owner_kind(&owner("org-a", "nas-0"), &asking, &orgs), OWNER_KIND_THIS_ORG);
        assert_eq!(owner_ids_for(OWNER_KIND_OTHER_ORG_ON_NODE, &owner("org-b", "nas-1")), (String::new(), String::new()));
    }

    /// Another organisation of this node: its journal is not a candidate at
    /// all — no row, so no name, no members, no status — and an adoption that
    /// names its array id anyway (a guessed or remembered id) is refused with
    /// the same sentence as a journal that does not exist.
    #[test]
    fn a_journal_of_another_org_on_this_node_is_not_listed_and_its_adoption_is_refused() {
        let entry = lost_journal("media");
        let mine = {
            let mut own = lost_journal("archiwum");
            own.spec.owner = this_addon();
            own
        };
        let disks: Vec<NasDisk> = member_disks(&entry).into_iter().chain(member_disks(&mine)).collect();
        let orgs: BTreeSet<String> = [this_addon().org_id, entry.spec.owner.org_id.clone()].into();

        let candidates = import_candidates(&[entry.clone(), mine.clone()], &[], &disks, &[], &this_addon(), &orgs);
        assert_eq!(candidates.len(), 1, "only this org's own journal: {candidates:?}");
        assert_eq!(candidates[0].array_id, mine.spec.array_id);
        let wire = serde_json::to_string(&candidates).unwrap();
        assert!(!wire.contains("\"media\"") && !wire.contains(&entry.spec.array_id), "the name is never sent: {wire}");

        let refusal = import_selection(&candidates, &entry.spec.array_id, "media").unwrap_err().to_string();
        assert!(refusal.contains("nie widzi już dziennika"), "refused like a journal that is not there: {refusal}");
        assert!(!refusal.contains("media"), "{refusal}");
    }

    /// An organisation this node has NO record of (the disks came from
    /// another machine): listed, adoptable, with the re-own sentence — and
    /// still without its ids.
    #[test]
    fn a_journal_of_an_org_unknown_to_this_node_is_listed_and_adoptable() {
        let entry = lost_journal("media");
        let orgs: BTreeSet<String> = [this_addon().org_id, "org-other-tenant".to_string()].into();
        let candidates = import_candidates(&[entry.clone()], &[], &member_disks(&entry), &[], &this_addon(), &orgs);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "media");
        assert_eq!(candidates[0].status, "importable");
        assert_eq!(candidates[0].owner_kind, OWNER_KIND_OTHER_INSTALLATION);
        assert_eq!((candidates[0].owner_org_id.as_str(), candidates[0].owner_addon_id.as_str()), ("", ""));
        assert!(import_selection(&candidates, &entry.spec.array_id, "media").is_ok());
    }

    #[test]
    fn a_member_is_named_by_its_kernel_name_or_by_its_part_in_the_array() {
        assert_eq!(member_label("d2", "sdh"), "sdh");
        assert_eq!(member_label("d2", ""), "dysk danych 2");
        assert_eq!(member_label("c1", ""), "dysk cache");
        assert_eq!(member_label("parity1", ""), "dysk parity 1");
    }

    #[test]
    fn an_unreadable_mount_table_never_reads_as_an_unmounted_union() {
        let mut entry = lost_journal("media");
        entry.union_mounted = None;
        let candidates = import_candidates(&[entry.clone()], &[], &member_disks(&entry), &[], &this_addon(), &no_orgs());
        assert!(!candidates[0].union_mounted, "unknown is rendered as 'not claimed to be mounted'");
        assert_eq!(candidates[0].status, "importable", "and it does not block the adoption");
    }

    // ----- recovery out of needs_attention (helper 0.14.0) --------------------

    /// Runs one job body to its end under the real `jobs::spawn` wrapper —
    /// the one that closes the job row with `finish_job` — and returns the
    /// body's outcome.
    async fn run_job<F, Fut>(
        db: &DbPool,
        kind: &str,
        subject: &str,
        intent: jobs::ElasticJobIntent,
        body: F,
    ) -> Result<()>
    where
        F: FnOnce(jobs::JobHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let (completion, finished) = tokio::sync::oneshot::channel();
        jobs::spawn(db, kind, subject, "test", Some(intent), Some(completion), body).unwrap();
        tokio::time::timeout(Duration::from_secs(5), finished).await.unwrap().unwrap()
    }

    /// A manual Sync, Scrub or Fix that FAILED, as helper 0.14.0 answers it
    /// (`perform_maintenance`, Err arm): no intent left pending, the verdict
    /// recorded as the array's cause, and for a Sync parity marked stale.
    /// `counted` is a log that counted errors (a scrub that found damage);
    /// without it the log broke its contract and counted nothing.
    fn helper_failed_run(
        spec: &ElasticCreateSpec,
        operation_id: &str,
        kind: ElasticSnapraidKind,
        counted: bool,
    ) -> ElasticSnapraidResult {
        let mut answer = snapraid_result(spec, operation_id, kind.clone(), ElasticSnapraidOutcome::Failed);
        if !counted {
            answer.run.outcome = ElasticSnapraidOutcome::NeedsAttention;
            answer.run.exit_code = Some(0);
            answer.run.errors_file = None;
            answer.run.errors_io = None;
            answer.run.errors_data = None;
        }
        answer.state.last_run = Some(answer.run.clone());
        answer.state.attention = Some(tentanas_helper::elastic::ElasticAttention::ParityRun {
            operation_id: operation_id.into(),
            kind: kind.clone(),
        });
        if kind == ElasticSnapraidKind::Sync {
            answer.state.stale_parity_bytes = Some(0);
            answer.state.parity_stale = true;
        }
        answer
    }

    /// A run the helper's admission gate refused: nothing written, the
    /// journal's facts as they were, the closed code as the run's detail.
    fn helper_refused_run(
        spec: &ElasticCreateSpec,
        operation_id: &str,
        kind: ElasticSnapraidKind,
        refusal: tentanas_helper::elastic::ElasticRefusal,
        facts: impl FnOnce(&mut ElasticResult),
    ) -> ElasticSnapraidResult {
        let mut answer = snapraid_result(spec, operation_id, kind, ElasticSnapraidOutcome::Refused);
        answer.run.detail = Some(refusal.as_str().into());
        facts(&mut answer.state);
        answer
    }

    /// Feeds one helper answer to a SnapRAID job on `db`, through the job
    /// body and `finish_job`, and returns the job's outcome.
    async fn run_snapraid_answer(
        db: &DbPool,
        spec: &ElasticCreateSpec,
        operation_id: &str,
        kind: &ElasticSnapraidKind,
        answer: &ElasticSnapraidResult,
    ) -> Result<()> {
        let intent = jobs::ElasticJobIntent::Snapraid {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: operation_id.into(),
            kind: kind.clone(),
            acknowledge_parity_fault: None,
        };
        let (work_spec, work_id, work_kind, work_answer) =
            (spec.clone(), operation_id.to_string(), kind.clone(), answer.clone());
        run_job(db, &format!("elastic_{}", snapraid_kind(kind)), &spec.name, intent, move |h| async move {
            execute_snapraid_job(&h, &work_spec, &work_id, &work_kind, async move { reply(&work_answer) }).await
        })
        .await
    }

    /// THE CROSS-LAYER RULE (design §5, test 15): what the core offers on the
    /// wire for every kind of parity run is what the helper's own gate
    /// (`maintenance_admission`) admits over the very same answer.
    fn assert_core_offers_what_the_helper_admits(
        label: &str,
        row: &ElasticArrayRow,
        wire: &NasElasticArray,
        helper: &ElasticResult,
    ) {
        use tentanas_helper::elastic::maintenance_admission;
        let facts = helper.admission();
        let admits = |kind: &ElasticSnapraidKind, acknowledged: Option<&str>| {
            maintenance_admission(&facts, kind, acknowledged).is_ok()
        };
        // The acknowledgement the screen's confirm would send is the fault id
        // the core puts on the wire — and THAT is what the helper checks.
        let confirmed = Some(wire.sync_fault_id.as_str());
        // A Scrub, and a Sync with the confirm: offered exactly when admitted.
        assert_eq!(wire.parity_run_available, admits(&ElasticSnapraidKind::Scrub, None), "{label}: scrub");
        assert_eq!(wire.parity_run_available, admits(&ElasticSnapraidKind::Sync, confirmed), "{label}: confirmed sync");
        // A Sync offered without the confirm is one the helper admits without it,
        if wire.parity_run_available && !wire.sync_needs_acknowledgement {
            assert!(admits(&ElasticSnapraidKind::Sync, None), "{label}: plain sync offered but refused");
        }
        // and whenever the helper would refuse the plain Sync, the core asks.
        if !admits(&ElasticSnapraidKind::Sync, None) && facts.attention.is_some_and(|cause| {
            matches!(cause, tentanas_helper::elastic::ElasticAttention::ParityRun { .. })
        }) {
            assert!(wire.sync_needs_acknowledgement, "{label}: the confirm is not asked for");
        }
        // A repair the dispatch would start is one the helper admits.
        let fix = ElasticSnapraidKind::Fix { disk: "d1".into() };
        if matches!(row.state.as_str(), "active" | "needs_attention")
            && repair_blocker(wire).is_none()
            && repair_evidence(wire).is_some()
        {
            assert!(admits(&fix, None), "{label}: repair offered but refused");
        }
    }

    /// C1, the wedge that started this: a manual run that FAILED used to pin
    /// the array for good. Every failure the helper records is fed through
    /// the real job and `finish_job`, and the screen's offer is held to the
    /// helper's gate over the same answer. The Sync over a Scrub or Repair
    /// fault is offered, and only with the confirm.
    #[tokio::test]
    async fn after_every_failed_parity_run_the_core_offers_exactly_what_the_helper_admits() {
        let fix = ElasticSnapraidKind::Fix { disk: "d1".into() };
        for (label, kind, counted, attention, needs_confirm) in [
            ("sync failed", ElasticSnapraidKind::Sync, true, "sync_failed", false),
            ("scrub counted errors", ElasticSnapraidKind::Scrub, true, "scrub_failed", true),
            // The log broke its contract and counted nothing: no repair
            // evidence in the history, and still the helper's recorded cause
            // asks for the confirm.
            ("scrub broke its log", ElasticSnapraidKind::Scrub, false, "scrub_failed", true),
            ("repair failed", fix.clone(), true, "fix_failed", true),
        ] {
            let spec = create_spec("recover");
            let db = settled_database(&spec);
            let operation_id = uuid::Uuid::now_v7().to_string();
            let answer = helper_failed_run(&spec, &operation_id, kind.clone(), counted);
            let outcome = run_snapraid_answer(&db, &spec, &operation_id, &kind, &answer).await;
            assert!(outcome.is_err(), "{label}");
            let row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
            assert_eq!(row.state, "needs_attention", "{label}");
            let wire = observed_protocol(&row, &BTreeMap::new(), Ok(answer.state.clone()), &[]);
            assert_eq!(wire.attention, attention, "{label}");
            assert!(wire.parity_run_available, "{label}: the array has a way out");
            assert_eq!(wire.sync_needs_acknowledgement, needs_confirm, "{label}");
            if needs_confirm {
                // The id the confirm will name is the run that recorded the
                // fault — the helper's own cause.
                assert_eq!(wire.sync_fault_id, operation_id, "{label}");
            }
            assert_core_offers_what_the_helper_admits(label, &row, &wire, &answer.state);
        }
    }

    /// I5 for a parity run: a run the helper's gate REFUSED closes `failed`
    /// with its code as the job's error, the array keeps its state, and no
    /// unresolved row is left — a refused repair used to add one each time.
    #[tokio::test]
    async fn a_gate_refusal_closes_failed_and_leaves_nothing_unresolved() {
        use tentanas_helper::elastic::{ElasticAttention, ElasticRefusal};
        let fix = ElasticSnapraidKind::Fix { disk: "d1".into() };
        let cases: Vec<(&str, ElasticSnapraidKind, ElasticRefusal, Box<dyn FnOnce(&mut ElasticResult)>)> = vec![
            ("repair over a pending intent", fix.clone(), ElasticRefusal::OperationPending, Box::new(|state| {
                state.stage = ElasticStage::NeedsAttention;
                state.operation_pending = true;
            })),
            ("sync over a scrub fault", ElasticSnapraidKind::Sync, ElasticRefusal::FaultUnacknowledged, Box::new(|state| {
                state.stage = ElasticStage::NeedsAttention;
                state.attention = Some(ElasticAttention::ParityRun {
                    operation_id: uuid::Uuid::now_v7().to_string(),
                    kind: ElasticSnapraidKind::Scrub,
                });
            })),
            ("scrub over a failed restore", ElasticSnapraidKind::Scrub, ElasticRefusal::AttentionOther, Box::new(|state| {
                state.stage = ElasticStage::NeedsAttention;
                state.attention = Some(ElasticAttention::Other);
            })),
        ];
        for (label, kind, refusal, facts) in cases {
            let spec = create_spec("refused");
            let db = settled_database(&spec);
            let operation_id = uuid::Uuid::now_v7().to_string();
            let answer = helper_refused_run(&spec, &operation_id, kind.clone(), refusal, facts);
            let error = run_snapraid_answer(&db, &spec, &operation_id, &kind, &answer).await.unwrap_err();
            let code = format!("refusal:elastic_{}", refusal.as_str());
            assert_eq!(error.to_string(), code, "{label}: the job's error is the code the screen words");
            let (state, stored): (String, String) = db
                .read()
                .unwrap()
                .query_row(
                    "SELECT state,error FROM nas_elastic_operations WHERE operation_id=?1",
                    [&operation_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((state.as_str(), stored.as_str()), ("failed", code.as_str()), "{label}");
            let row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
            assert_eq!(row.state, "active", "{label}: the array is untouched");
            assert!(!row.unresolved_operation, "{label}: nothing is left unresolved");
            let refused = row.snapraid_history.first().expect("the refused run is listed");
            assert_eq!((refused.outcome.as_str(), refused.detail.as_str()), ("refused", refusal.as_str()), "{label}");
            let wire = observed_protocol(&row, &BTreeMap::new(), Ok(answer.state.clone()), &[]);
            assert_core_offers_what_the_helper_admits(label, &row, &wire, &answer.state);
        }
    }

    /// The array a stopped add leaves in the helper: the spec grown by the
    /// disk, the add recorded as the cause, and where it stopped.
    fn helper_stopped_add(
        spec_after: &ElasticCreateSpec,
        step: tentanas_helper::elastic::ElasticAddStep,
        formatted: bool,
        joined: bool,
        in_union: Option<bool>,
    ) -> ElasticResult {
        use tentanas_helper::elastic::{add_undo_admission, ElasticAttention, ElasticPendingAdd};
        let disk = spec_after.data.last().unwrap().clone();
        let mut state = ready_result(spec_after);
        state.stage = ElasticStage::NeedsAttention;
        state.detail = Some("krok dodawania dysku nie powiódł się".into());
        state.attention = Some(ElasticAttention::AddDisk { disk_id: disk.disk_id.clone(), joined });
        let slot = ElasticRole::Data(spec_after.data.len() as u16);
        let mounted = in_union == Some(true) || step == tentanas_helper::elastic::ElasticAddStep::Join;
        for observation in state.disks.iter_mut().filter(|d| d.role == slot) {
            observation.mounted = Some(mounted);
            if !formatted {
                // A blank disk, or the start of an mkfs that never finished:
                // nothing on it is the array's yet.
                observation.observed_uuid = None;
                observation.filesystem = None;
                observation.size_bytes = None;
                observation.used_bytes = None;
                observation.free_bytes = None;
            }
        }
        state.pending_add = Some(ElasticPendingAdd {
            disk_id: disk.disk_id.clone(),
            branch: tentanas_helper::elastic::data_branch_name(spec_after.data.len()),
            formatted,
            mounted: Some(mounted),
            in_union,
            joined,
            step,
            undo_possible: add_undo_admission(joined, in_union).is_ok(),
        });
        state
    }

    async fn run_add_answer(
        db: &DbPool,
        spec: &ElasticCreateSpec,
        disk: &ElasticDiskSpec,
        answer: &ElasticResult,
    ) -> (String, Result<()>) {
        let operation_id = uuid::Uuid::now_v7().to_string();
        let intent = jobs::ElasticJobIntent::AddDisk {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: operation_id.clone(),
            disk: disk.clone(),
        };
        let spec_after = spec_with_added_disk(spec, disk).unwrap();
        let (before, id, work_disk, work_answer) = (spec.clone(), operation_id.clone(), disk.clone(), answer.clone());
        let outcome = run_job(db, "elastic_add_disk", &spec.name, intent, move |h| async move {
            execute_add_disk_job(&h, &before, &spec_after, &id, &work_disk, async move { reply(&work_answer) }).await
        })
        .await;
        (operation_id, outcome)
    }

    async fn run_abort_answer(
        db: &DbPool,
        spec: &ElasticCreateSpec,
        disk: &ElasticDiskSpec,
        answer: &ElasticResult,
    ) -> (String, Result<()>) {
        let operation_id = uuid::Uuid::now_v7().to_string();
        let intent = jobs::ElasticJobIntent::AddDiskAbort {
            owner: spec.owner.clone(),
            array_id: spec.array_id.clone(),
            operation_id: operation_id.clone(),
            disk: disk.clone(),
        };
        let (work_spec, id, work_disk, work_answer) = (spec.clone(), operation_id.clone(), disk.clone(), answer.clone());
        let outcome = run_job(db, "elastic_add_disk_abort", &spec.name, intent, move |h| async move {
            execute_add_disk_abort_job(&h, &work_spec, &id, &work_disk, async move { reply(&work_answer) }).await
        })
        .await;
        (operation_id, outcome)
    }

    fn operation_state(db: &DbPool, operation_id: &str) -> String {
        db.read()
            .unwrap()
            .query_row("SELECT state FROM nas_elastic_operations WHERE operation_id=?1", [operation_id], |r| r.get(0))
            .unwrap()
    }

    /// C2's first wedge (K3/K4, I5): an add the helper REFUSED before its
    /// journal grew closes `failed`, keeps the array's state, raises no add
    /// alert and pins nothing — so a different disk is admitted at once, by
    /// the core and by the helper alike.
    #[tokio::test]
    async fn a_refused_add_closes_failed_and_pins_nothing() {
        use tentanas_helper::elastic::{add_disk_admission, ElasticAddAdmission, ElasticRefusal};
        let spec = create_spec("refused-add");
        let db = settled_database(&spec);
        let disk = fresh_disk("refused-add", 16 * 1024 * 1024 * 1024);
        let mut answer = ready_result(&spec);
        answer.refused = Some(ElasticRefusal::DiskClaimed);
        answer.detail = Some("dysk pełni w tej macierzy inną rolę".into());
        let (operation_id, outcome) = run_add_answer(&db, &spec, &disk, &answer).await;
        assert_eq!(outcome.unwrap_err().to_string(), "refusal:elastic_disk_claimed");
        assert_eq!(operation_state(&db, &operation_id), "failed");
        let row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(row.state, "active", "a refusal is not a fault");
        assert!(!row.unresolved_operation);
        assert!(row.pending_add.is_none());
        assert!(store::unfinished_elastic_add_disk(&db, &spec.owner, &spec.array_id).unwrap().is_none());
        assert!(
            store::list_alerts(&db, true).unwrap().iter().all(|alert| alert.code != "elastic_add_disk_unconfirmed"),
            "no add alert for a refusal"
        );
        // The helper admits another disk as a FRESH add over the same facts,
        // and so does the core.
        let mut other = fresh_disk("refused-add-other", 16 * 1024 * 1024 * 1024);
        other.disk_id = "refused-add-other".into();
        assert_eq!(add_disk_admission(&answer.admission(), spec.data.last(), &other), Ok(ElasticAddAdmission::Fresh));
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(answer.clone()), &[]);
        assert!(wire.pending_add_disk.is_none() && wire.attention.is_empty());
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_add_disk".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &db,
            &job,
            Some(&jobs::ElasticJobIntent::AddDisk {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                disk: other,
            }),
        )
        .expect("a different disk is admitted after a refusal");
    }

    /// K5: the ONE rule for a helper answer's member count. The spec, or the
    /// spec with the PINNED add's disk — never an extra disk nothing pinned,
    /// never two. An add that stopped before its filesystem was confirmed is
    /// observable although its disk carries nothing of the array yet.
    #[test]
    fn a_half_done_add_is_observable_only_while_it_is_pinned() {
        use tentanas_helper::elastic::ElasticAddStep;
        let spec = create_spec("half");
        let disk = fresh_disk("half", 16 * 1024 * 1024 * 1024);
        let after = spec_with_added_disk(&spec, &disk).unwrap();
        let grown = helper_stopped_add(&after, ElasticAddStep::Format, false, false, Some(false));
        assert_eq!(answered_spec(&spec, Some(&disk), &grown).unwrap(), after);
        validate_observation(&answered_spec(&spec, Some(&disk), &grown).unwrap(), &grown)
            .expect("an unformatted pending slot is not an identity mismatch");
        assert!(answered_spec(&spec, None, &grown).is_err(), "N+1 without a pinned add");
        let mut other = disk.clone();
        other.expected_uuid = uuid::Uuid::new_v4().to_string();
        assert!(
            validate_observation(&answered_spec(&spec, Some(&other), &grown).unwrap(), &grown).is_ok(),
            "an unconfirmed slot carries no identity to compare"
        );
        let mut twice = grown.clone();
        let mut extra = twice.disks.last().unwrap().clone();
        extra.role = ElasticRole::Data(3);
        twice.disks.push(extra);
        assert!(answered_spec(&spec, Some(&disk), &twice).is_err(), "N+2 is another array");
        assert_eq!(answered_spec(&spec, Some(&disk), &ready_result(&spec)).unwrap(), spec);
        // A MOUNTED slot is a confirmed one: an unconfirmed slot may not claim it.
        let mut mounted = grown.clone();
        for observation in mounted.disks.iter_mut().filter(|d| d.role == ElasticRole::Data(2)) {
            observation.mounted = Some(true);
        }
        assert!(validate_observation(&after, &mounted).is_err());
        // A pending add must name the last slot.
        let mut foreign = grown.clone();
        foreign.pending_add.as_mut().unwrap().disk_id = "someone-else".into();
        assert!(validate_observation(&after, &foreign).is_err());
        // An older record, from before the add succeeded, is judged against
        // the array it described.
        assert_eq!(recorded_spec(&after, None, &ready_result(&spec)).unwrap(), spec);
    }

    /// C2's second wedge (K4-K7, I4, I6): an add that stopped part-way, at
    /// every step, is pinned; the core offers its resume exactly when the
    /// helper admits one, offers its undo exactly when the helper would take
    /// it, offers no parity run over it, and a resume the helper refuses
    /// leaves it pinned. Before the disk joined the share the undo removes
    /// the slot and the array is active again; after, it is refused.
    #[tokio::test]
    async fn an_unfinished_add_is_pinned_and_resumed_or_undone_as_the_helper_admits() {
        use tentanas_helper::elastic::{
            add_disk_admission, add_undo_admission, maintenance_admission, ElasticAddAdmission, ElasticAddStep,
            ElasticRefusal,
        };
        for (label, step, formatted, joined, in_union) in [
            ("mkfs stopped", ElasticAddStep::Format, false, false, Some(false)),
            ("mount failed", ElasticAddStep::Mount, true, false, Some(false)),
            ("join failed", ElasticAddStep::Join, true, true, Some(false)),
            ("final sync failed", ElasticAddStep::Sync, true, true, Some(true)),
        ] {
            let spec = create_spec("grow");
            let db = settled_database(&spec);
            let disk = fresh_disk("grow", 16 * 1024 * 1024 * 1024);
            let after = spec_with_added_disk(&spec, &disk).unwrap();
            let stopped = helper_stopped_add(&after, step, formatted, joined, in_union);
            let (first, outcome) = run_add_answer(&db, &spec, &disk, &stopped).await;
            assert!(outcome.is_err(), "{label}");
            assert_eq!(operation_state(&db, &first), "needs_attention", "{label}");
            let row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
            assert_eq!(row.pending_add.as_ref(), Some(&disk), "{label}: the add is pinned");
            let wire = observed_protocol(&row, &BTreeMap::new(), Ok(stopped.clone()), &[]);
            let pending = wire.pending_add_disk.clone().expect("the screen sees the unfinished add");
            assert_eq!(pending.disk_id, disk.disk_id, "{label}");
            assert_eq!(pending.step, add_step_code(step), "{label}");
            assert_eq!(wire.attention, "add_disk", "{label}");
            // The resume offered is the resume the helper admits; any other
            // disk is refused by both.
            let facts = stopped.admission();
            assert_eq!(add_disk_admission(&facts, after.data.last(), &disk), Ok(ElasticAddAdmission::Resume), "{label}");
            let mut other = fresh_disk("grow-other", 16 * 1024 * 1024 * 1024);
            other.disk_id = "grow-other".into();
            assert_eq!(add_disk_admission(&facts, after.data.last(), &other), Err(ElasticRefusal::AttentionAddDisk));
            let job = |disk: &ElasticDiskSpec| {
                (
                    tentaflow_protocol::tentanas::NasJob {
                        job_id: uuid::Uuid::now_v7().to_string(),
                        kind: "elastic_add_disk".into(),
                        subject: spec.name.clone(),
                        status: "running".into(),
                        started_at: store::now(),
                        ..Default::default()
                    },
                    jobs::ElasticJobIntent::AddDisk {
                        owner: spec.owner.clone(),
                        array_id: spec.array_id.clone(),
                        operation_id: uuid::Uuid::now_v7().to_string(),
                        disk: disk.clone(),
                    },
                )
            };
            let (other_job, other_intent) = job(&other);
            assert!(store::insert_job(&db, &other_job, Some(&other_intent)).is_err(), "{label}: another disk");
            // The undo offered is the undo the helper takes.
            assert_eq!(pending.undo_possible, add_undo_admission(joined, in_union).is_ok(), "{label}");
            // No parity run over an unfinished add, on either side.
            assert!(!wire.parity_run_available, "{label}");
            assert_eq!(
                maintenance_admission(&facts, &ElasticSnapraidKind::Scrub, None),
                Err(ElasticRefusal::AttentionAddDisk)
            );
            assert_core_offers_what_the_helper_admits(label, &row, &wire, &stopped);

            // A RESUME THE HELPER REFUSES leaves the add pinned (K4).
            let mut refused = stopped.clone();
            refused.refused = Some(ElasticRefusal::OperationPending);
            let (retry, outcome) = run_add_answer(&db, &spec, &disk, &refused).await;
            assert_eq!(outcome.unwrap_err().to_string(), "refusal:elastic_operation_pending", "{label}");
            assert_eq!(operation_state(&db, &retry), "failed", "{label}");
            assert_eq!(operation_state(&db, &first), "needs_attention", "{label}: still pinned");
            assert_eq!(
                store::unfinished_elastic_add_disk(&db, &spec.owner, &spec.array_id).unwrap().as_ref(),
                Some(&disk),
                "{label}"
            );

            if pending.undo_possible {
                // THE UNDO removes the slot: the helper's array is the core's
                // again, nothing pins it, and the array is active.
                let (undo, outcome) = run_abort_answer(&db, &spec, &disk, &ready_result(&spec)).await;
                outcome.expect(label);
                assert_eq!(operation_state(&db, &undo), "succeeded", "{label}");
                assert_eq!(operation_state(&db, &first), "failed", "{label}: the add is closed as history");
                let row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
                assert!(row.pending_add.is_none(), "{label}");
                assert_eq!(row.state, "active", "{label}");
                assert!(!row.unresolved_operation, "{label}");
                assert_eq!(row.data().count(), 1, "{label}: the member row was never written");
            } else {
                // Once the disk may have joined, the helper refuses the undo,
                // and the add stays pinned for its resume.
                let mut joined_refusal = stopped.clone();
                joined_refusal.refused = Some(ElasticRefusal::AddJoined);
                let (undo, outcome) = run_abort_answer(&db, &spec, &disk, &joined_refusal).await;
                assert_eq!(outcome.unwrap_err().to_string(), "refusal:elastic_add_joined", "{label}");
                assert_eq!(operation_state(&db, &undo), "failed", "{label}");
                assert_eq!(
                    store::unfinished_elastic_add_disk(&db, &spec.owner, &spec.array_id).unwrap().as_ref(),
                    Some(&disk),
                    "{label}"
                );
            }
        }
    }

    fn history_run(kind: &str, outcome: &str, errors_data: Option<u64>) -> NasSnapraidRun {
        NasSnapraidRun {
            operation_id: Some(uuid::Uuid::now_v7().to_string()),
            kind: kind.into(),
            outcome: outcome.into(),
            errors: errors_data.map(|data| data + 2),
            errors_file: errors_data.map(|_| 2),
            errors_io: errors_data.map(|_| 0),
            errors_data,
            ..Default::default()
        }
    }

    /// M2 of the release review: the acknowledgement requirement, the
    /// repair's evidence and the scheduled Sync's skip end with the CURE — a
    /// clean full scrub, a successful repair, or for file errors alone the
    /// Sync that drops them — and not before. A Sync alone does not end data
    /// errors: their marks stay repairable across it.
    #[test]
    fn a_parity_fault_ends_with_its_cure_and_not_before() {
        let data_fault = history_run("scrub", "failed", Some(3));
        let file_fault = history_run("scrub", "failed", Some(0));
        let (sync_ok, scrub_ok, fix_ok) =
            (history_run("sync", "ok", None), history_run("scrub", "ok", Some(0)), history_run("fix", "ok", None));
        let fix_failed = history_run("fix", "failed", Some(1));
        // (newest first, fault still stands)
        for (label, history, stands) in [
            ("data errors", vec![data_fault.clone()], true),
            ("data errors, then an acknowledged Sync", vec![sync_ok.clone(), data_fault.clone()], true),
            ("data errors, Sync, then a clean scrub", vec![scrub_ok.clone(), sync_ok.clone(), data_fault.clone()], false),
            ("data errors, then a repair", vec![fix_ok.clone(), data_fault.clone()], false),
            ("file errors only", vec![file_fault.clone()], true),
            ("file errors only, then the Sync that drops them", vec![sync_ok.clone(), file_fault.clone()], false),
            ("a repair failed", vec![fix_failed.clone()], true),
            ("a repair failed, then a clean scrub", vec![scrub_ok.clone(), fix_failed.clone()], false),
        ] {
            assert_eq!(sync_acknowledgement_needed(&history), stands, "{label}");
            assert_eq!(scheduled_sync_blocker(&history).is_some(), stands, "{label}: cadence");
        }
        // The id an acknowledgement names is the fault's own run.
        assert_eq!(unacknowledged_fault(&[sync_ok, data_fault.clone()]).and_then(|r| r.operation_id.clone()), data_fault.operation_id);
    }

    /// M4 of the release review: a core restart during an undo that the
    /// helper completed left the add pinned and the undo row orphaned; the
    /// only offer was then the resume, which formats and adds the very disk
    /// the admin took out. The helper's own answer — the array without the
    /// disk, no add recorded — settles it on the next read.
    #[tokio::test]
    async fn an_undo_the_core_lost_is_settled_from_the_helpers_answer() {
        use tentanas_helper::elastic::ElasticAddStep;
        let spec = create_spec("lost-undo");
        let db = settled_database(&spec);
        let disk = fresh_disk("lost-undo", 16 * 1024 * 1024 * 1024);
        let after = spec_with_added_disk(&spec, &disk).unwrap();
        let stopped = helper_stopped_add(&after, ElasticAddStep::Mount, true, false, Some(false));
        let (first, outcome) = run_add_answer(&db, &spec, &disk, &stopped).await;
        assert!(outcome.is_err());
        // The undo starts, and the core loses it: its row is orphaned.
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_add_disk_abort".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &db,
            &job,
            Some(&jobs::ElasticJobIntent::AddDiskAbort {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: uuid::Uuid::now_v7().to_string(),
                disk: disk.clone(),
            }),
        )
        .unwrap();
        store::fail_orphaned_jobs(&db).unwrap();
        let mut row = store::elastic_array(&db, &spec.owner, &spec.name).unwrap().unwrap();
        assert_eq!(row.pending_add.as_ref(), Some(&disk), "still pinned after the restart");
        // The helper, meanwhile, finished the undo.
        let undone = ready_result(&spec);
        assert!(add_undone_by_helper(&row, &undone));
        // Not while it still holds the add, or counts its disk.
        assert!(!add_undone_by_helper(&row, &stopped));
        reconcile_undone_add(&db, &mut row, &undone);
        assert!(row.pending_add.is_none(), "the add is settled");
        assert_eq!(row.state, "active");
        assert!(!row.unresolved_operation);
        assert_eq!(operation_state(&db, &first), "failed");
        let wire = observed_protocol(&row, &BTreeMap::new(), Ok(undone), &[]);
        assert!(wire.pending_add_disk.is_none(), "no resume is offered for a disk the admin took out");
    }

    /// Minor 6 of the release review: a run recorded while an add was pinned
    /// counts the add's disk. Once the add is gone (undone) that record must
    /// not make the whole array unreadable.
    #[test]
    fn a_record_that_counted_an_undone_add_still_reads() {
        let spec = create_spec("old-record");
        let disk = fresh_disk("old-record", 16 * 1024 * 1024 * 1024);
        let after = spec_with_added_disk(&spec, &disk).unwrap();
        let id = uuid::Uuid::now_v7().to_string();
        let mut record = snapraid_result(&after, &id, ElasticSnapraidKind::Scrub, ElasticSnapraidOutcome::Refused);
        record.run.detail = Some("attention_add_disk".into());
        assert!(validate_snapraid_result(&spec, &id, &ElasticSnapraidKind::Scrub, &record).is_err());
        strip_unpinned_slot(&spec, None, &mut record.state);
        validate_snapraid_result(&recorded_spec(&spec, None, &record.state).unwrap(), &id, &ElasticSnapraidKind::Scrub, &record)
            .expect("the record reads without the slot");
        // While the add is pinned nothing is stripped.
        let mut pinned = snapraid_result(&after, &id, ElasticSnapraidKind::Scrub, ElasticSnapraidOutcome::Refused);
        strip_unpinned_slot(&spec, Some(&disk), &mut pinned.state);
        assert_eq!(pinned.state.disks.len(), member_count(&after));
    }
}
