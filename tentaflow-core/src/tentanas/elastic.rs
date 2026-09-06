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
//   * nowhere yet — the store, the job executor and the reconcile loop. This
//     slice builds the model and the plan; the code that persists a row and
//     runs a job is the next one.
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
    } else if !observed.mount_table_known {
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
    } else if cache_bytes.is_none() {
        (
            "unknown",
            "this node could not measure how much is waiting on the cache".to_string(),
        )
    } else if cache_bytes == Some(0) && observed.last_sync_at.is_some() {
        (
            "protected",
            if array.cache().next().is_none() {
                // No cache at all: nothing is ever staged outside parity, so
                // this array is protected between syncs in a way a cached one
                // never is. Worth saying, because it is the trade the cache
                // buys speed with.
                "everything on the data disks is covered by the last sync, and this array \
                 has no cache staging files outside it"
                    .to_string()
            } else {
                "everything on the data disks is covered by the last sync, and the cache is \
                 empty"
                    .to_string()
            },
        )
    } else if observed.last_sync_at.is_none() {
        (
            "window_open",
            "no sync has run yet, so parity covers nothing".to_string(),
        )
    } else {
        (
            "window_open",
            "on the cache without parity — protected after the next sync, which the mover \
             runs immediately after it moves"
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

    if !name.is_empty() && tentanas_helper::elastic::validate_array_name(name).is_err() {
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

    // Two DATA disks that are really one device.
    //
    // MEASURED (2026-09-06, snapraid 14.7): snapraid refuses such a
    // configuration outright — `Disks 'X' and 'Y' are on the same device.` —
    // and neither §5.3 nor `disk_repeated` covers it. `disk_repeated` compares
    // disk IDS, and this is the case where two DIFFERENT ids resolve to one
    // piece of hardware: a multipath device enumerated twice, or the same disk
    // seen through two links. Parity computed across two views of one disk
    // protects nothing at all — the "second" copy dies with the first — so
    // this is a refusal, and it is better made here than by a snapraid that
    // only speaks up on the first sync, after every disk has been erased.
    for (i, a) in data.iter().enumerate() {
        for b in data.iter().skip(i + 1) {
            let Some(shared) = same_device(a, b) else {
                continue;
            };
            out.push(refuse(
                "data_disks_same_device",
                b,
                format!(
                    "{} and {} are the same device ({shared}): snapraid refuses two data \
                     disks on one device, and parity across two views of one disk protects \
                     nothing",
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

/// What the probe's result means. Pure, so the classification is testable
/// without a snapraid on the build machine — which is the only way it gets
/// tested at all, since the interesting case is a binary that crashes.
///
/// THE ONLY THING THAT SEPARATES THE TWO BUILDS IS THE SIGNAL. MEASURED
/// (2026-09-06), the same probe configuration run against both binaries:
///
///   healthy (-O2)       exit 1,   stdout: `Self-test...` then
///                                 `You must have at least 2 'content' files
///                                 in different disks.`
///   broken (-O3 -flto)  exit 139 (SIGSEGV), stdout: `Self-test...` only
///
/// So a HEALTHY snapraid exits NON-ZERO on this probe and always will:
/// snapraid wants two content files on two different disks, and a throwaway
/// directory cannot offer that. The first version of this function called
/// every non-zero exit `Broken`, which would have marked EVERY HEALTHY NODE
/// broken and shown its admin a sentence about `-march=native -O3 -flto`
/// while snapraid worked perfectly. That is the defect this probe exists to
/// prevent, pointed the other way — and it is the worse direction, because it
/// teaches an admin to ignore the one row that would have told them their
/// parity is fake.
///
/// Therefore: `Broken` if and only if the process was KILLED BY A SIGNAL. Any
/// ordinary exit, 1 included, is proof the binary ran, parsed a config and
/// reached its own diagnostics.
///
/// `Self-test...` is NOT the discriminator: MEASURED, both builds print it,
/// so its presence proves nothing about which one is running.
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
        let _ = (stdout, stderr);
        return ToolHealth::Broken(format!(
            "snapraid is installed but was killed{signal} while reading a trivial configuration. A build made with -march=native -O3 -flto is known to segfault on `status` and `sync`; rebuilding at -O2 fixes it. Until then this node cannot sync or scrub and its parity would never report an error, so the array would look protected and would not be."
        ));
    }
    ToolHealth::Working
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
            config_path: config_path(&array.name),
            last_sync: None,
            last_scrub: None,
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

#[cfg(test)]
mod tests {
    use super::*;

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

        // An empty cache with a sync behind it is protected.
        let p = protection(&a, &all_mounted(&a, 0));
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

    /// An array with NO cache disk is protected, not unknown.
    ///
    /// The cache is optional (§5.3), and "zero cache disks hold zero
    /// unprotected bytes" is a fact, not a gap. The version that started the
    /// sum at `None` left every cacheless array grey with a dash on the
    /// Protection KPI and a sentence blaming a measurement nobody ever had to
    /// take. The existing protection test could not see it, because it
    /// reached the cacheless case by clearing PARITY, which short-circuits
    /// earlier — so this one keeps the parity and removes only the cache.
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
        assert_eq!(p.status, "protected");
        assert_eq!(p.fault_tolerance, Some(1));
        assert!(
            p.detail.contains("no cache"),
            "the sentence should say why there is no window: {}",
            p.detail
        );

        // …and the card is green rather than grey.
        let disks: BTreeMap<String, NasDisk> = ["sdg", "sdh", "sdj"]
            .into_iter()
            .map(|n| (format!("id-{n}"), disk(n, 4 * TB)))
            .collect();
        let observed = all_mounted(&a, 0);
        let wire = to_protocol(&a, &disks, &observed, true, "12.3", ("active", ""));
        assert_eq!(wire.health, "ok", "{}", wire.health_reason);
        assert_eq!(wire.cache_size_bytes, Some(0));
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
