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
    NasElasticParity, NasElasticPlan, NasElasticProtection, NasElasticRefusal, NasMoverSettings,
    NasSchedule, NasSnapraidState,
};
use tentanas_helper::elastic::{
    cache_branch_path, config_path, data_branch_path, parity_file_path, parity_mount_path,
    union_path, Branch as SpecBranch, ElasticSpec, MergerfsOptions, MoverRules,
    ParityDisk as SpecParity, SnapraidOptions, Tools,
};
use anyhow::{anyhow, ensure, Result};
use tentanas_helper::elastic::{ElasticCreateSpec, ElasticOwner, ElasticResult, ElasticRole, ElasticStage, ElasticClaimsResult};
use tentanas_helper::elastic::{ElasticSnapraidKind, ElasticSnapraidOutcome, ElasticSnapraidResult};
use tentanas_helper::HelperCommand;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use super::{db as store, jobs};
use std::sync::{Arc, Mutex, OnceLock};

/// The machine kind of §5.3, next to a ZFS pool's `zfs`. The SPEC fixes the
/// spelling: "Elastic Array" in prose, `elastic-array` on the wire, and never
/// the trademarked name of the product this resembles.
pub const KIND: &str = "elastic-array";

/// Environment feature ids of the two tools an array needs.
pub const MERGERFS_FEATURE_ID: &str = "mergerfs";
pub const SNAPRAID_FEATURE_ID: &str = "snapraid";

/// How long an out-of-schedule mover run waits before it may fire again.
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
pub const MOVER_RETRIGGER_COOLDOWN: Duration = Duration::from_secs(20 * 60);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoverConfig {
    pub enabled: bool,
    pub schedule: Option<NasSchedule>,
    pub min_age_secs: u64,
    pub cache_min_free_pct: u8,
    pub coupled_sync: bool,
}

impl Default for MoverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            schedule: None,
            min_age_secs: 7_200,
            cache_min_free_pct: 20,
            // §5.3 makes this the default and the wizard explains it: without
            // the coupling the mover moves bytes out of one unprotected
            // window and into another.
            coupled_sync: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapraidConfig {
    pub sync_schedule: Option<NasSchedule>,
    pub scrub_schedule: Option<NasSchedule>,
    pub scrub_percent: u8,
    pub scrub_older_than_days: u32,
}

/// One array's desired state, the shape a store row will carry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ElasticArrayRow {
    pub snapraid_history: Vec<tentaflow_protocol::tentanas::NasSnapraidRun>,
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
    pub mover: MoverConfig,
    pub snapraid: SnapraidConfig,
    /// The last state this node persisted, and why — carried the way
    /// `TargetRow::state`/`state_detail` are, so an array switched off by an
    /// import keeps the sentence that explains it.
    pub state: String,
    pub state_detail: String,
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
}

impl ArrayObservation {
    fn probe(&self, mountpoint: &str) -> BranchProbe {
        self.probes.get(mountpoint).copied().unwrap_or_default()
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
) -> (&'static str, String, Disposition) {
    if !array.enabled {
        // A row that ARRIVED disabled keeps the sentence it arrived with —
        // the config import writes reasons an admin has to read into exactly
        // this field. A row an admin STOPPED keeps nothing: its old detail
        // described a running array.
        let carried = if array.state == "disabled" {
            array.state_detail.clone()
        } else {
            String::new()
        };
        return ("disabled", carried, Disposition::Remove);
    }

    if !observed.mount_table_known {
        return (
            "unknown",
            "this node could not read its mount table, so it will not mount or unmount \
             anything: a union mounted over branches it cannot see would send every client \
             write to the root filesystem"
                .to_string(),
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
            "mergerfs is not installed on this node, so the union cannot be mounted".to_string(),
            Disposition::Freeze,
        );
    }
    if !array.parity.is_empty() && !installed(SNAPRAID_FEATURE_ID) {
        return (
            "error",
            "snapraid is not usable on this node — it is missing, or installed and not \
             working (see the Environment tab) — so the parity disks of this array protect \
             nothing until it is"
                .to_string(),
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
    for (mountpoint, label) in mountpoints_of(array) {
        let probe = observed.probe(&mountpoint);
        match (probe.mounted, probe.device_present) {
            (Some(true), _) => {}
            (Some(false), Some(true)) => mountable.push(label),
            (Some(false), Some(false)) => gone.push(label),
            // Not mounted and nobody looked at the disk, or the mount table
            // itself was unreadable. Both are "this node does not know".
            (Some(false), None) | (None, _) => unknown.push(label),
        }
    }
    if !unknown.is_empty() {
        return (
            "unknown",
            format!(
                "this node could not tell whether {} {} there, and will not mount the union \
                 until it can",
                unknown.join(", "),
                if unknown.len() == 1 { "is" } else { "are" }
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
        let consequence = if observed.union_mounted == Some(true) {
            "the union is still serving the disks it has, and the files on the missing one \
             are not visible in it"
        } else {
            "the union stays down: mounting it over an unmounted branch would send client \
             writes to the root filesystem"
        };
        return (
            "error",
            format!(
                "{} {} not on this node — {consequence}",
                gone.join(", "),
                if gone.len() == 1 { "is" } else { "are" }
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
            format!(
                "{} {} present and not mounted yet — the next reconcile mounts {} and then \
                 the union",
                mountable.join(", "),
                if mountable.len() == 1 { "is" } else { "are" },
                if mountable.len() == 1 { "it" } else { "them" }
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
            "saved, but this node is not serving the union yet — the next reconcile mounts it"
                .to_string(),
            Disposition::Apply,
        );
    }

    let detail = if array.parity.is_empty() {
        "no parity disk: a disk failure loses that disk's files".to_string()
    } else {
        String::new()
    };
    ("active", detail, Disposition::Apply)
}

/// Every mountpoint the array needs, with the label an error message uses.
fn mountpoints_of(array: &ElasticArrayRow) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for branch in array.data() {
        out.push((
            data_branch_path(&array.name, &branch.name),
            format!("data disk {}", branch.name),
        ));
    }
    for branch in array.cache() {
        out.push((
            cache_branch_path(&array.name, &branch.name),
            format!("cache disk {}", branch.name),
        ));
    }
    for parity in &array.parity {
        out.push((
            parity_mount_path(&array.name, parity.index),
            format!("parity disk {}", parity.name),
        ));
    }
    out
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

/// The protection alert: the cache is holding unprotected bytes and the mover
/// is not draining them.
pub fn protection_alert_key(array: &str) -> String {
    format!("elastic:{array}:protection")
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

    let (status, detail) = if !parity_present {
        (
            "unprotected",
            "this array has no parity disk: a disk failure loses that disk's files".to_string(),
        )
    } else if observed.last_sync_at.is_none() {
        (
            "window_open",
            "Brak potwierdzonego sync: ochrona parity nie została potwierdzona".to_string(),
        )
    } else if observed.parity_errors.is_some_and(|errors| errors > 0) {
        (
            "unknown",
            "Wykryto błędy parity; nie można potwierdzić ochrony danych".to_string(),
        )
    } else if fault_tolerance.is_none() {
        (
            "unknown",
            "Brak pełnego pomiaru dostępności i poprawności parity".to_string(),
        )
    } else if fault_tolerance.map(usize::from) != Some(array.parity.len()) {
        (
            "unknown",
            "Brakuje dysku parity; pełna skonfigurowana ochrona nie jest dostępna".to_string(),
        )
    } else if cache_bytes.is_none() {
        (
            "unknown",
            "this node could not measure how much is waiting on the cache".to_string(),
        )
    } else if cache_bytes.is_some_and(|bytes| bytes > 0) {
        (
            "window_open",
            "on the cache without parity — protected after the next sync, which the mover \
             runs immediately after it moves"
                .to_string(),
        )
    } else if observed.moved_unsynced_bytes.is_some_and(|bytes| bytes > 0) {
        (
            "window_open",
            "Dane na dyskach danych nie są objęte ostatnim sync; opróżnienie cache nie zamyka okna bez ochrony".to_string(),
        )
    } else if observed.moved_unsynced_bytes.is_none() {
        (
            "unknown",
            "Nie zmierzono zmian na dyskach danych od ostatniego sync".to_string(),
        )
    } else {
        (
            "protected",
            "Pomiar nie wykazał danych poza ostatnim sync; cache jest pusty, a parity dostępna bez zgłoszonych błędów".to_string(),
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
    /// Nothing to do — or nothing this node is allowed to conclude.
    None,
}

/// Whether the cache-pressure trigger should fire for this array right now.
///
/// The clock is a PARAMETER, not a wall-clock read, for the reason
/// `targets::removal_is_due_with` takes one: the process-wide clock is shared
/// state and `cargo test` runs tests in parallel, so a test that wound the
/// global clock would decide another test's outcome. It is also what makes the
/// cooldown testable without waiting twenty minutes.
///
/// Returns `MoverTrigger::None` for every uncertainty. A mover run is a
/// privileged job that moves files; starting one because a free-space figure
/// could not be read would be acting on a measurement that does not exist.
pub fn mover_trigger(
    array: &ElasticArrayRow,
    observed: &ArrayObservation,
    clock: &MoverClock,
) -> MoverTrigger {
    if !array.enabled || !array.mover.enabled {
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
    MoverTrigger::None
}

/// When each array last started an out-of-schedule mover run.
///
/// One instance per caller-supplied clock; `global()` is the process-wide one
/// the tick uses, and a test makes its own.
#[derive(Debug, Default)]
pub struct MoverClock(std::sync::Mutex<BTreeMap<String, Instant>>);

impl MoverClock {
    pub fn new() -> Self {
        Self(std::sync::Mutex::new(BTreeMap::new()))
    }

    pub fn global() -> &'static Self {
        static CLOCK: std::sync::OnceLock<MoverClock> = std::sync::OnceLock::new();
        CLOCK.get_or_init(MoverClock::new)
    }

    /// Records that a run has just been started for this array.
    pub fn started(&self, array: &str) {
        if let Ok(mut at) = self.0.lock() {
            at.insert(array.to_string(), Instant::now());
        }
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
        let Ok(at) = self.0.lock() else {
            return false;
        };
        at.get(array)
            .map(|t| t.elapsed() >= MOVER_RETRIGGER_COOLDOWN)
            .unwrap_or(true)
    }

    /// Moves an existing mark back in time so a test can reach the far side of
    /// the cooldown. It REQUIRES the mark to exist and says so if it does not
    /// — a version that silently created one would pass just as happily
    /// against a `started` that recorded nothing.
    #[cfg(test)]
    fn rewind_for_test(&self, array: &str, by: Duration) {
        let mut at = self.0.lock().expect("mover clock");
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
fn refuse(code: &str, disk: &NasDisk, detail: String) -> NasElasticRefusal {
    NasElasticRefusal {
        code: code.to_string(),
        disk_id: disk.disk_id.clone(),
        disk_name: disk.name.clone(),
        detail,
    }
}

/// Whether a disk is already owned by something else, and by what.
///
/// §5.3: a disk in an Elastic Array cannot be in a ZFS pool or be a spare. The
/// conflicting owner is NAMED, because "this disk is in use" sends an admin
/// hunting and "this disk is in the pool tank" does not. The inventory's
/// `role` is the authority — it is derived from what `lsblk` and `zpool`
/// actually report, not from what this app remembers writing.
pub fn conflicting_owner(disk: &NasDisk) -> Option<String> {
    // A spare is a pool member with a `spare` vdev role, so it is checked
    // first: otherwise it would be reported as an ordinary member and the
    // admin would go looking for it in the pool's data vdevs.
    if disk.vdev_role == "spare" {
        return Some(match disk.member_of.as_deref() {
            Some(pool) => format!("a hot spare of pool {pool}"),
            None => "a hot spare".to_string(),
        });
    }
    match disk.role.as_str() {
        "pool_member" => Some(match disk.member_of.as_deref() {
            Some(pool) => format!("a member of ZFS pool {pool}"),
            None => "a member of a ZFS pool".to_string(),
        }),
        "array_member" => Some(match disk.member_of.as_deref() {
            Some(array) => format!("a member of md array {array}"),
            None => "a member of an md array".to_string(),
        }),
        "system" => Some("carrying this node's running system".to_string()),
        "mounted" => Some(match disk.mountpoints.first() {
            Some(mp) => format!("mounted at {mp}"),
            None => "mounted".to_string(),
        }),
        "used" => Some("holding a filesystem or partitions".to_string()),
        _ => None,
    }
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
                    format!("{} already belongs to another Elastic Array", disk.name),
                ));
                continue;
            }
            if let Some(owner) = conflicting_owner(disk) {
                out.push(refuse(
                    "disk_in_use",
                    disk,
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

/// Things an admin should know and may still choose.
fn layout_warnings(data: &[NasDisk], parity: &[NasDisk], cache: &[NasDisk]) -> Vec<String> {
    let mut out = Vec::new();
    if parity.is_empty() {
        out.push(
            "no parity disk: this array has no protection at all, and losing one data disk \
             loses that disk's files"
                .to_string(),
        );
    }
    // A parity disk exactly the size of the largest data disk passes the hard
    // rule and can still come up short: the parity FILE lives on a
    // filesystem, and the filesystem's own metadata takes space the file
    // cannot have. Not a refusal — it may well fit — but it is the failure an
    // admin discovers weeks later, on the first sync that fills the disk.
    if let Some(largest) = data.iter().map(|d| d.size_bytes).max() {
        for disk in parity {
            if disk.size_bytes >= largest && disk.size_bytes < largest + largest / 100 {
                out.push(format!(
                    "{} is only just as large as the largest data disk: the parity file has to \
                     fit on a filesystem, and its metadata may leave too little room",
                    disk.name
                ));
            }
        }
    }
    if cache.is_empty() {
        out.push(
            "no cache disk: there is nothing for the mover to do, and new files are written \
             straight to the data disks"
                .to_string(),
        );
    }
    let unhealthy: Vec<&str> = data
        .iter()
        .chain(parity.iter())
        .chain(cache.iter())
        .filter(|d| matches!(d.health.as_str(), "warning" | "critical"))
        .map(|d| d.name.as_str())
        .collect();
    if !unhealthy.is_empty() {
        out.push(format!(
            "SMART warnings on {}: building an array on a disk that is already failing starts \
             it degraded",
            unhealthy.join(", ")
        ));
    }
    // Mixed sizes are the POINT of this array kind, so they are not warned
    // about the way a ZFS vdev warns about them — the note says the opposite
    // of the ZFS one on purpose.
    if data.len() > 1 {
        let smallest = data.iter().map(|d| d.size_bytes).min().unwrap_or(0);
        let largest = data.iter().map(|d| d.size_bytes).max().unwrap_or(0);
        if largest > smallest {
            out.push(
                "the data disks are different sizes, and every byte of each of them is used: \
                 that is what this array kind is for"
                    .to_string(),
            );
        }
    }
    out
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
            detail: format!(
                "mkfs.{wanted} is not installed on this node, so no disk of this array can \
                 be prepared"
            ),
            ..Default::default()
        });
    }
    let warnings = layout_warnings(data, parity, cache);

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
    let health = health_of(&protection, &data_disks, &cache_disks, &parity_disks, state.0);

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
        mover: NasMoverSettings {
            enabled: array.mover.enabled,
            schedule: array.mover.schedule.clone(),
            min_age_secs: array.mover.min_age_secs,
            cache_min_free_pct: array.mover.cache_min_free_pct,
            coupled_sync: array.mover.coupled_sync,
            last_run: None,
            history: Vec::new(),
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
            scrub_percent: array.snapraid.scrub_percent,
            scrub_older_than_days: array.snapraid.scrub_older_than_days,
            parity_errors: observed.parity_errors,
            parity_errors_window_days: 30,
        },
        protection,
        created_at: array.created_at.clone(),
        updated_at: array.updated_at.clone(),
    }
}

/// The one status of the array card, with its reason.
fn health_of(
    protection: &NasElasticProtection,
    data: &[NasElasticBranch],
    cache: &[NasElasticBranch],
    parity: &[NasElasticParity],
    state: &str,
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
    if protection.status == "unprotected" {
        return ("warning", protection.detail.clone());
    }
    if protection.status == "unknown" {
        return ("unknown", protection.detail.clone());
    }
    ("ok", String::new())
}

fn validate_observation(spec: &ElasticCreateSpec, result: &ElasticResult) -> Result<()> {
    ensure!(
        result.array_id == spec.array_id
            && result.operation_id == spec.operation_id
            && result.owner == spec.owner,
        "Odpowiedź helpera dotyczy innej intencji lub właściciela"
    );
    ensure!(
        result.disks.len() == spec.data.len() + spec.parity.len(),
        "Niepełny zestaw obserwacji dysków"
    );
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
        };
        let expected = expected.ok_or_else(|| anyhow!("Obca rola w odpowiedzi helpera"))?;
        ensure!(
            seen.insert((role, index)),
            "Powtórzona rola w odpowiedzi helpera"
        );
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
    Ok(())
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
    ensure!(out.success() && out.stdout.len() < 64 * 1024, "Nie można odczytać rezerwacji roota");
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

async fn execute_job(h: &jobs::JobHandle, spec: ElasticCreateSpec, operation_id: String,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>) -> Result<()> {
    let result = async {
        let out = run.await?;
        ensure!(out.success(), "Helper Elastic zwrócił błąd {}",out.code);
        ensure!(out.stdout.len() < 64 * 1024, "Odpowiedź Elastic przekracza limit");
        let result: ElasticResult = serde_json::from_str(&out.stdout)?;
        validate_result(&spec, &result)?;
        Ok::<_,anyhow::Error>(result)
    }.await;
    let key = format!("elastic:{}:restore",spec.array_id);
    match result {
        Ok(result) => {
            store::finish_elastic_operation(h.db(), &spec.owner, &operation_id, Ok(&result))?;
            if result.stage != ElasticStage::Ready {
                let detail = result.detail.as_deref().unwrap_or("Macierz wymaga interwencji; rezerwacje zachowane");
                store::raise_alert(h.db(), &key, "warning", "elastic-array", &spec.name,
                    "Macierz wymaga interwencji", detail)?;
                return Err(anyhow!(detail.to_string()));
            }
            store::resolve_alert(h.db(), &key)?;
            h.progress(100);
            Ok(())
        }
        Err(error) => {
            let detail = format!("{error}; utrata odpowiedzi nie dowodzi zatrzymania I/O");
            store::finish_elastic_operation(h.db(), &spec.owner, &operation_id, Err(&detail))?;
            store::raise_alert(h.db(), &key, "warning", "elastic-array", &spec.name,
                "Niepotwierdzony wynik macierzy", &detail)?;
            Err(error)
        }
    }
}

pub fn snapraid_kind(kind: ElasticSnapraidKind) -> &'static str {
    match kind {
        ElasticSnapraidKind::Sync => "sync",
        ElasticSnapraidKind::Scrub => "scrub",
    }
}

pub fn snapraid_command(
    owner: &ElasticOwner,
    array_id: &str,
    operation_id: &str,
    kind: ElasticSnapraidKind,
) -> HelperCommand {
    match kind {
        ElasticSnapraidKind::Sync => HelperCommand::ElasticSync {
            owner: owner.clone(),
            array_id: array_id.into(),
            operation_id: operation_id.into(),
        },
        ElasticSnapraidKind::Scrub => HelperCommand::ElasticScrub {
            owner: owner.clone(),
            array_id: array_id.into(),
            operation_id: operation_id.into(),
        },
    }
}

pub fn validate_snapraid_result(
    spec: &ElasticCreateSpec,
    operation_id: &str,
    kind: ElasticSnapraidKind,
    result: &ElasticSnapraidResult,
) -> Result<()> {
    if result.run.outcome == ElasticSnapraidOutcome::Refused {
        validate_observation(spec, &result.state)?;
    } else {
        validate_result(spec, &result.state)?;
    }
    let run = &result.run;
    ensure!(
        run.operation_id == operation_id && run.kind == kind,
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
        ElasticSnapraidOutcome::Refused => {
            ensure!(
                result.state.stage == ElasticStage::Ready
                    && run.exit_code.is_none()
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
                        )
                    ),
                "Niepotwierdzona odmowa przed scrub"
            );
        }
        ElasticSnapraidOutcome::Succeeded => {
            let empty_sync = kind == ElasticSnapraidKind::Sync
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
                    && (empty_sync
                        || (run.errors_file == Some(0)
                            && run.errors_io == Some(0)
                            && run.errors_data == Some(0))),
                "Niepotwierdzony sukces SnapRAID"
            );
            if kind == ElasticSnapraidKind::Scrub {
                ensure!(
                    run.total_blocks
                        .zip(run.checked_blocks)
                        .is_some_and(|(total, checked)| checked > 0 && checked <= total)
                        && run.accessed_mb.is_some(),
                    "Scrub nie potwierdził pełnego zakresu"
                );
            }
            if kind == ElasticSnapraidKind::Sync {
                ensure!(
                    result.state.sync_completed_at == run.finished_at,
                    "Inny checkpoint sync"
                );
            }
        }
        ElasticSnapraidOutcome::Failed | ElasticSnapraidOutcome::NeedsAttention => {
            ensure!(
                result.state.stage == ElasticStage::NeedsAttention
                    && result.state.last_run.as_ref() == Some(run),
                "Błąd SnapRAID nie zachował nieukończonej operacji"
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
    kind: ElasticSnapraidKind,
    run: impl std::future::Future<Output = Result<super::broker::CommandOutput>>,
) -> Result<()> {
    let output = run.await?;
    ensure!(
        output.success() && output.stdout.len() < 64 * 1024,
        "Brak wiarygodnego wyniku SnapRAID"
    );
    let result: ElasticSnapraidResult = serde_json::from_str(&output.stdout)?;
    validate_snapraid_result(spec, operation_id, kind, &result)?;
    store::record_snapraid_result(h.db(), &spec.owner, operation_id, &result)?;
    ensure!(
        result.run.outcome == ElasticSnapraidOutcome::Succeeded,
        "{}",
        result
            .run
            .detail
            .as_deref()
            .unwrap_or("Operacja SnapRAID nie zakończyła się sukcesem")
    );
    Ok(())
}

pub fn spawn_snapraid(
    db: &DbPool,
    array: &ElasticArrayRow,
    started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    kind: ElasticSnapraidKind,
) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let intent = jobs::ElasticJobIntent::Snapraid {
        owner: spec.owner.clone(),
        array_id: spec.array_id.clone(),
        operation_id: operation_id.clone(),
        kind,
    };
    jobs::spawn(
        db,
        &format!("elastic_{}", snapraid_kind(kind)),
        &array.name,
        started_by,
        Some(intent),
        None,
        move |h| async move {
            let command = snapraid_command(&spec.owner, &spec.array_id, &operation_id, kind);
            let run = jobs::run_step(
                &h,
                &command,
                explicit.as_deref(),
                Duration::from_secs(24 * 60 * 60),
            );
            execute_snapraid_job(&h, &spec, &operation_id, kind, run).await
        },
    )
}

pub async fn create_job(h: jobs::JobHandle, spec: ElasticCreateSpec,
    explicit: Option<Arc<ElevationToken>>) -> Result<()> {
    let command = HelperCommand::ElasticCreate { operation: spec.clone() };
    let run = jobs::run_step(&h,&command,explicit.as_deref(),Duration::from_secs(24 * 60 * 60));
    execute_job(&h, spec.clone(), spec.operation_id.clone(), run).await
}

pub fn spawn_restore(db: &DbPool, array: &ElasticArrayRow, started_by: &str,
    explicit: Option<Arc<ElevationToken>>,
    completion: Option<tokio::sync::oneshot::Sender<Result<()>>>) -> Result<tentaflow_protocol::tentanas::NasJob> {
    let spec = array.persisted_spec()?.clone();
    let operation_id = uuid::Uuid::now_v7().to_string();
    let intent = jobs::ElasticJobIntent::Restore { owner: spec.owner.clone(),
        array_id: spec.array_id.clone(), operation_id: operation_id.clone() };
    jobs::spawn(db,"elastic_restore",&array.name,started_by,Some(intent),completion,move |h| async move {
        let command = HelperCommand::ElasticRestore { array_id: spec.array_id.clone(),owner:spec.owner.clone() };
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
    validate_observation(spec,&result)?;
    Ok(result)
}

fn observed_protocol(array: &ElasticArrayRow, disks: &BTreeMap<String,NasDisk>,
    result: Result<ElasticResult>, features: &[FeatureState]) -> NasElasticArray {
    let mut observed = ArrayObservation::default();
    let failure;
    let mut root_stage = None;
    match result {
        Ok(result) => {
            observed.mount_table_known = result.union_mounted.is_some()
                && result.disks.iter().all(|d| d.mounted.is_some());
            observed.union_mounted = result.union_mounted;
            observed.last_sync_at = result.sync_completed_at;
            root_stage = Some(result.stage);
            failure = result.detail;
            for disk in result.disks {
                let path = match disk.role {
                    ElasticRole::Data(i) => data_branch_path(&array.name,&format!("d{i}")),
                    ElasticRole::Parity(i) => parity_mount_path(&array.name,i),
                };
                observed.probes.insert(path,BranchProbe { mounted:disk.mounted,
                    device_present:disk.device_present,size_bytes:disk.size_bytes,
                    used_bytes:disk.used_bytes,free_bytes:disk.free_bytes });
            }
        }
        Err(error) => failure = Some(error.to_string()),
    }
    let installed = |id: &str| features.iter().any(|f| f.id == id && f.status == "ok");
    let snapraid = features.iter().find(|f| f.id == SNAPRAID_FEATURE_ID);
    let (mut state, mut detail, _) = array_state(array,&observed,&installed);
    if let Some(stage) = root_stage {
        if stage != ElasticStage::Ready {
            state = "needs_attention";
            detail = failure.unwrap_or_else(|| "Nieukończony checkpoint helpera".to_string());
        } else if array.state != "active" {
            state = if array.state == "creating" { "creating" } else { "needs_attention" };
            detail = if array.state_detail.is_empty() {
                "Oczekuje na trwałe potwierdzenie operacji w bazie instancji".to_string()
            } else { array.state_detail.clone() };
        }
    } else {
        state = "unknown";
        detail = failure.unwrap_or_default();
    }
    to_protocol(array,disks,&observed,installed(SNAPRAID_FEATURE_ID),
        snapraid.and_then(|f| f.version.as_deref()).unwrap_or(""),(state,&detail))
}

pub async fn list(db: &DbPool, owner: &ElasticOwner) -> Result<Vec<NasElasticArray>> {
    let rows = store::elastic_arrays(db,owner)?;
    let environment = super::environment::cached_or_probe(db).await;
    let features = environment.as_ref().map(|e| e.features.as_slice()).unwrap_or(&[]);
    let disks = super::disks::snapshot().0.into_iter().map(|d| (d.disk_id.clone(),d)).collect();
    let mut arrays = Vec::with_capacity(rows.len());
    for array in rows {
        let observed = match &environment {
            Ok(_) => observe_array(db,&array).await,
            Err(error) => Err(anyhow!("Brak pomiaru środowiska: {error}")),
        };
        arrays.push(observed_protocol(&array,&disks,observed,features));
    }
    Ok(arrays)
}

pub async fn get(db: &DbPool, owner: &ElasticOwner, name: &str) -> Result<Option<NasElasticArray>> {
    Ok(list(db,owner).await?.into_iter().find(|a| a.name == name))
}

async fn restore_startup_rows<F>(rows: &[ElasticArrayRow], mut start: F) -> Result<()>
where
    F: FnMut(
        &ElasticArrayRow,
        tokio::sync::oneshot::Sender<Result<()>>,
    ) -> Result<tentaflow_protocol::tentanas::NasJob>,
{
    static STARTUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _startup = STARTUP.lock().await;
    for row in rows {
        let (completion, finished) = tokio::sync::oneshot::channel();
        start(row, completion)?;
        finished
            .await
            .map_err(|_| anyhow::anyhow!("Brak potwierdzenia zakończenia przywracania"))??;
    }
    Ok(())
}

pub fn start_restore(main_db: DbPool, db: DbPool, owner: ElasticOwner) {
    static STARTING: OnceLock<Mutex<BTreeSet<(String,String)>>> = OnceLock::new();
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return; };
    let starting = STARTING.get_or_init(|| Mutex::new(BTreeSet::new()));
    let key = (owner.org_id.clone(),owner.addon_id.clone());
    if !starting.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone()) { return; }
    runtime.spawn(async move {
        let outcome = async {
            if !super::instance_should_run(&main_db,&db) { return Ok(()); }
            let rows = store::elastic_arrays(&db,&owner)?;
            let status = super::elevation::status(&db).await;
            let allowed = status.mode == "helper" && status.helper_state == "ok" && status.core_compatible;
            if allowed {
                return restore_startup_rows(&rows, |row, completion| {
                    if !super::instance_should_run(&main_db,&db) {
                        anyhow::bail!("Instancja nie jest aktywna; przywracanie zatrzymane");
                    }
                    spawn_restore(&db,row,"startup",None,Some(completion))
                }).await;
            }
            for row in rows {
                let spec = row.persisted_spec()?;
                store::raise_alert(&db,&format!("elastic:{}:restore",spec.array_id),"warning",
                    "elastic-array",&row.name,"Macierz oczekuje na przywrócenie",
                    "Brak bezobsługowego kanału roota; wymagane jawne Przywróć")?;
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

    pub(crate) fn create_spec(name: &str) -> ElasticCreateSpec {
        let disk = |role: &str| tentanas_helper::elastic::ElasticDiskSpec {
            disk_id:format!("{name}-{role}"),wwn:Some(format!("wwn-{name}-{role}")),
            serial:Some(format!("serial-{name}-{role}")),bytes:32 * 1024 * 1024 * 1024,
            expected_uuid:uuid::Uuid::new_v4().to_string(),
        };
        ElasticCreateSpec { array_id:uuid::Uuid::new_v4().to_string(),
            operation_id:uuid::Uuid::new_v4().to_string(),owner:ElasticOwner {org_id:"org".into(),addon_id:"nas".into()},
            name:name.into(),filesystem:tentanas_helper::elastic::ElasticFilesystem::Ext4,
            data:vec![disk("data")],parity:vec![disk("parity")] }
    }

    pub(crate) fn ready_result(spec: &ElasticCreateSpec) -> ElasticResult {
        ElasticResult { array_id:spec.array_id.clone(),operation_id:spec.operation_id.clone(),owner:spec.owner.clone(),
            stage:ElasticStage::Ready,union_mounted:Some(true),sync_completed_at:Some(store::now()),detail:None,last_run:None,
            disks:spec.data.iter().enumerate().map(|(i,d)| (ElasticRole::Data((i+1) as u16),d))
                .chain(spec.parity.iter().enumerate().map(|(i,d)| (ElasticRole::Parity((i+1) as u8),d)))
                .map(|(role,d)| tentanas_helper::elastic::ElasticDiskObservation { role,
                    kernel_name:Some("sdz".into()),device:Some("/dev/sdz".into()),observed_uuid:Some(d.expected_uuid.clone()),
                    filesystem:Some(spec.filesystem.as_str().into()),device_present:Some(true),mounted:Some(true),
                    size_bytes:Some(d.bytes),used_bytes:Some(4096),free_bytes:Some(d.bytes-4096),detail:None }).collect() }
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
        let run = tentanas_helper::elastic::ElasticSnapraidRun {
            operation_id: operation_id.into(),
            kind,
            started_at: "2026-09-08T01:00:00Z".into(),
            finished_at: Some("2026-09-08T01:01:00Z".into()),
            outcome,
            exit_code: (!refused).then_some(if success { 0 } else { 1 }),
            total_blocks: success.then_some(80),
            checked_blocks: (success && kind == ElasticSnapraidKind::Scrub).then_some(68),
            accessed_mb: (success && kind == ElasticSnapraidKind::Scrub).then_some(17),
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
        if success && kind == ElasticSnapraidKind::Sync {
            state.sync_completed_at = run.finished_at.clone();
        }
        ElasticSnapraidResult { state, run }
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
        validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &good).unwrap();
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
                validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &bad).is_err(),
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
        validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &refused).unwrap();
        refused.run.checked_blocks = Some(0);
        assert!(
            validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &refused).is_err()
        );
        let failed = snapraid_result(
            &spec,
            &id,
            ElasticSnapraidKind::Scrub,
            ElasticSnapraidOutcome::Failed,
        );
        validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &failed).unwrap();
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
        validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Sync, &empty).unwrap();
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
        validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Sync, &unchanged).unwrap();
        unchanged.run.errors_data = None;
        unchanged.state.last_run = Some(unchanged.run.clone());
        assert!(
            validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Sync, &unchanged).is_err()
        );
        empty.run.total_blocks = Some(1);
        empty.state.last_run = Some(empty.run.clone());
        assert!(validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Sync, &empty).is_err());
        let mut zero_scrub = good.clone();
        zero_scrub.run.checked_blocks = Some(0);
        zero_scrub.state.last_run = Some(zero_scrub.run.clone());
        assert!(
            validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &zero_scrub).is_err()
        );
        let mut sub_mb = good;
        sub_mb.run.accessed_mb = Some(0);
        sub_mb.state.last_run = Some(sub_mb.run.clone());
        validate_snapraid_result(&spec, &id, ElasticSnapraidKind::Scrub, &sub_mb).unwrap();
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

    #[tokio::test]
    async fn startup_rows_stop_after_body_panic_or_persistence_failure() {
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
                    name: "forbidden-second".into(),
                    ..Default::default()
                },
            ];
            let mut calls = 0;
            let result = restore_startup_rows(&rows, |row, completion| {
                    calls += 1;
                    jobs::spawn(&db, "startup-test", &row.name, "test", None, Some(completion), move |h| async move {
                        assert_ne!(failure, 1, "kontrolowana panika");
                        if failure == 2 {
                            h.db().write().unwrap().execute_batch(
                                "CREATE TRIGGER refuse_finish BEFORE UPDATE ON nas_jobs BEGIN SELECT RAISE(ABORT,'persist denied'); END;")?;
                            return Ok(());
                        }
                        Err(anyhow!("kontrolowany błąd wykonawcy"))
                    })
                }).await;
            assert!(result.is_err());
            assert_eq!(calls, 1);
            let jobs = store::list_jobs(&db, 10).unwrap();
            assert_eq!(jobs.len(), 1);
            assert!(!jobs::running()
                .lock()
                .unwrap()
                .contains_key(&jobs[0].job_id));
            assert_eq!(
                jobs[0].status,
                if failure == 2 { "running" } else { "failed" }
            );
        }
    }

    #[tokio::test]
    async fn startup_rows_stop_on_spawn_error_or_missing_completion() {
        let rows = vec![ElasticArrayRow::default(), ElasticArrayRow::default()];
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
            assert!(result.is_err());
            assert_eq!(calls, 1);
        }
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
                        ElasticSnapraidKind::Sync,
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
                if matches!(case, "success" | "refused" | "persist") {
                    "active"
                } else {
                    "needs_attention"
                },
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
                    _ => "needs_attention",
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
                assert_eq!(wire.protection.status, "unknown");
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
                assert_eq!(reopened.snapraid_history[0].outcome, "needs_attention");
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
        for (mountpoint, _) in mountpoints_of(array) {
            probes.insert(
                mountpoint,
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
            ("active", String::new(), Disposition::Apply)
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
            p.detail.contains("on the cache without parity"),
            "the sentence the UI builds has to come from here: {}",
            p.detail
        );
        assert!(
            !p.detail.contains("hour") && !p.detail.contains("unsynced for"),
            "the banned phrasing must not reappear: {}",
            p.detail
        );

        // Ochronę potwierdza także pomiar zmian na dyskach danych.
        let mut verified = all_mounted(&a, 0);
        verified.moved_unsynced_bytes = Some(0);
        let p = protection(&a, &verified);
        assert_eq!(p.cache_unprotected_bytes, Some(0));
        assert_eq!(p.status, "protected");

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

        // A switched-off mover never fires, however full the cache is.
        let mut off = array();
        off.mover.enabled = false;
        assert_eq!(mover_trigger(&off, &pressed, &fresh), MoverTrigger::None);
        // Nor does one with no cache to drain.
        let mut cacheless = array();
        cacheless.branches.retain(|b| b.role != "cache");
        assert_eq!(mover_trigger(&cacheless, &pressed, &fresh), MoverTrigger::None);
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

                assert_eq!(
                    plan.refusals.len(),
                    1,
                    "{first_role}/{second_role}/{identity}: {:?}",
                    plan.refusals
                );
                let refusal = &plan.refusals[0];
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
                for cache_count in 0..=2 {
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
        for name in ["sdl", "sdm", "sdn"] {
            assert!(
                wiped.iter().any(|d| d.contains(name)),
                "{name} is not among the disks the plan erases: {wiped:?}"
            );
        }
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
        let wire = to_protocol(&a, &disks, &observed, true, "12.3", (state.0, state.1.as_str()));

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
}
