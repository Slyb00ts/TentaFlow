// =============================================================================
// File: tentanas-helper/src/elastic.rs — Elastic Array (plan-02 §5.3): the
// desired state of ONE mergerfs + SnapRAID array and the ordered list of
// privileged operations that makes the node serve it.
//
// The same deal `block.rs` makes, for the same reasons: the plan is DATA, it
// is rendered by one function, and both sides render it — core to show it
// before anything happens and to put it in the job log, the wrapper to carry
// it out. One renderer, so the preview and the action cannot disagree.
//
// THE TOPOLOGY IS LOAD-BEARING, and getting it wrong is the whole slice:
//
//   /mnt/<array>                      ONE mergerfs union, the only path a
//                                     share, a folder or a client ever names
//   /mnt/tentanas-branches/<array>/
//       cache/<disk>                  branch — new files are created HERE
//       data/<disk>                   branch — where the mover puts them
//       parity/<n>                    NOT a branch: it holds the parity FILE
//
// The cache is a BRANCH OF THE UNION, never a mount of its own. That is what
// makes the mover a move BETWEEN BRANCHES UNDER the union, invisible from
// outside: `/mnt/media/filmy/a.mkv` is the same path before and after,
// whichever disk holds the bytes. A design with the cache as a separate
// filesystem would change the client-visible path every time the mover ran,
// and every SMB/NFS share on it would break.
//
// WHY NEW FILES LAND ON THE CACHE, mechanically: mergerfs create policies are
// MOUNT-WIDE — there is no per-directory create policy, so "use cache: yes/no"
// per folder cannot be expressed to mergerfs at all. The per-folder policy is
// therefore a MOVER rule, not a mergerfs setting — see `MoverRules`.
//
// There are TWO mechanisms that put new files on the cache, and this file
// picks one of them:
//   * branch ORDER with a first-found policy. MEASURED (2026-09-06, mergerfs
//     2.42.0): `category.create=ff` put consecutive new files on the first
//     branch, consistently. So order alone is sufficient.
//   * branch MODE: mounting the data branches `=NC` ("no create") takes them
//     out of every create policy while leaving existing files writable, so
//     the cache is the only candidate whatever the policy is. UNVERIFIED —
//     the mode syntax and its meaning have not been measured.
// This file uses `=NC` because it keeps the admin's chosen create policy
// meaningful for the mover's own target choice instead of forcing `ff` on the
// whole union. That is a judgement made on an unmeasured mechanism, and it
// carries the ENOSPC question named at the end of this header.
//
// WHAT IS MEASURED AND WHAT IS NOT. A first measurement pass ran on
// 2026-09-06 against mergerfs 2.42.0 and snapraid 14.7 on a live node; every
// fact it established is marked MEASURED (2026-09-06) at the place it decides
// something. Everything else is still read out of the projects' documentation
// and is marked UNVERIFIED. `block.rs` earned its "MEASURED (obs. NN)" notes
// the same way; a claim in this file without one of the two markers is a
// claim nobody has checked, so do not add one.
//
// THE MOST DANGEROUS OPEN QUESTION, named here so it is not buried: whether
// `moveonenospc` can move a file onto a branch mounted `=NC`. The whole
// cache design below rests on `=NC`, and if a full cache cannot spill onto
// the data disks, a client's write FAILS with ENOSPC on an array that has
// terabytes free. Measure that before this ships.
// =============================================================================

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{component_ok, invalid, CatalogError, MOUNT_ROOT};

/// Where the branches of every array live. Deliberately NOT under
/// `FLEET_MOUNT_ROOT` (`/mnt/tentanas/`), which holds this node's mounts of
/// OTHER nodes' shares: an array called `media` and a remote share called
/// `media` would otherwise fight over one directory.
pub const BRANCH_ROOT: &str = "/mnt/tentanas-branches/";

/// The app-owned configuration directory. snapraid has no drop-in directory,
/// so the app takes a directory of its own rather than a share of
/// `/etc/snapraid.conf`: one file per array, all of it ours, nothing of
/// anybody else's in it.
pub const CONFIG_DIR: &str = "/etc/tentanas/";

/// SnapRAID supports six parity disks; the wizard offers two (§5.3). The limit
/// is here because it is the one the PLAN can honour — `parity_directive`
/// knows the spelling of exactly these two.
pub const MAX_PARITY: usize = 2;

/// snapraid's own name for the file that holds the block checksums. It is
/// written on every data branch, so a data disk carries the map of what it
/// should contain even if the node is gone.
pub const CONTENT_FILE: &str = "snapraid.content";

const SNAPRAID: &[&str] = &["/usr/bin/snapraid", "/usr/local/bin/snapraid", "/bin/snapraid"];
const MERGERFS: &[&str] = &["/usr/bin/mergerfs", "/usr/local/bin/mergerfs", "/bin/mergerfs"];
const MKFS_XFS: &[&str] = &["/usr/sbin/mkfs.xfs", "/sbin/mkfs.xfs", "/usr/bin/mkfs.xfs"];
const MKFS_EXT4: &[&str] = &["/usr/sbin/mkfs.ext4", "/sbin/mkfs.ext4", "/usr/bin/mkfs.ext4"];

/// The filesystems a data, cache or parity disk may carry. Both keep every
/// disk readable on its own — the property the danger zone promises when an
/// array is dissolved ("dane na dyskach XFS pozostają czytelne osobno").
pub const FILESYSTEMS: &[&str] = &["xfs", "ext4"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticOwner {
    pub org_id: String,
    pub addon_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticFilesystem {
    Xfs,
    Ext4,
}

impl ElasticFilesystem {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xfs => "xfs",
            Self::Ext4 => "ext4",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticDiskSpec {
    pub disk_id: String,
    pub wwn: Option<String>,
    pub serial: Option<String>,
    pub bytes: u64,
    pub expected_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticCreateSpec {
    pub array_id: String,
    pub operation_id: String,
    pub owner: ElasticOwner,
    pub name: String,
    pub filesystem: ElasticFilesystem,
    pub data: Vec<ElasticDiskSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<ElasticDiskSpec>,
    pub parity: Vec<ElasticDiskSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticRole {
    Data(u16),
    Cache,
    Parity(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticStage {
    Prepared,
    Formatting,
    Mounting,
    SyncPending,
    Ready,
    NeedsAttention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticDiskObservation {
    pub role: ElasticRole,
    pub kernel_name: Option<String>,
    pub device: Option<String>,
    pub observed_uuid: Option<String>,
    pub filesystem: Option<String>,
    pub device_present: Option<bool>,
    pub mounted: Option<bool>,
    pub size_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticServiceMode {
    Hold,
    Online,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticServiceState {
    pub mode: ElasticServiceMode,
    pub operation_id: String,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticResult {
    pub array_id: String,
    pub operation_id: String,
    pub owner: ElasticOwner,
    pub stage: ElasticStage,
    pub disks: Vec<ElasticDiskObservation>,
    pub union_mounted: Option<bool>,
    pub service: Option<ElasticServiceState>,
    pub union_readonly: Option<bool>,
    pub sync_completed_at: Option<String>,
    pub detail: Option<String>,
    pub last_run: Option<ElasticSnapraidRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_mover: Option<ElasticMoverRun>,
    /// Bytes the mover put on data branches after the last confirmed Sync;
    /// `Some` means parity is out of date. The core reads it as moved but
    /// unsynced bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_parity_bytes: Option<u64>,
    /// Explicit: parity does not cover everything the mover moved. The byte
    /// count above is informational; this flag is the verdict.
    #[serde(default)]
    pub parity_stale: bool,
    /// File records the mover could neither finish nor withdraw, oldest
    /// first: the closed history plus the running operation's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stuck_records: Vec<ElasticStuckRecord>,
    /// Stuck paths the mover still skips whose full record is no longer shown.
    #[serde(default)]
    pub stuck_hidden: u64,
    /// Stuck paths dropped from the skip set to make room for a newer one.
    /// Such a path is NO LONGER skipped: the mover may meet it again.
    ///
    /// DELIBERATELY not rendered anywhere yet (owner, 2026-09-12): this is the
    /// array's lifetime figure and it never decays, so showing it would be an
    /// alarm nobody can clear. The per-run count on `ElasticMoverRun` plus the
    /// system log is the contract until the admin-acknowledge view exists.
    #[serde(default)]
    pub stuck_evicted: u64,
    /// The array cannot be operated again in this boot: a recovery after a
    /// boot stopped part-way, and only the next boot retries it.
    #[serde(default)]
    pub restart_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticSnapraidKind {
    Sync,
    Scrub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticSnapraidOutcome {
    Running,
    Succeeded,
    Failed,
    NeedsAttention,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticSnapraidRun {
    pub operation_id: String,
    pub kind: ElasticSnapraidKind,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: ElasticSnapraidOutcome,
    pub exit_code: Option<i32>,
    pub total_blocks: Option<u64>,
    pub checked_blocks: Option<u64>,
    pub accessed_mb: Option<u64>,
    pub errors_file: Option<u64>,
    pub errors_io: Option<u64>,
    pub errors_data: Option<u64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticSnapraidResult {
    pub state: ElasticResult,
    pub run: ElasticSnapraidRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticMoverPhase {
    Holding,
    Moving,
    Syncing,
    NeedsAttention,
    Complete,
}

/// Why one file stayed on the cache: `Skipped` by rule (another process held
/// it open), `Refused` because moving it is unsupported or unsafe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticMoverIssueKind {
    Skipped,
    Refused,
    /// Needs the admin: a source removed by identity after a read error, or
    /// a record that could be neither finished nor withdrawn.
    Attention,
}

/// One file the mover left behind, by path. The list is bounded; the counters
/// of `ElasticMoverRun` stay exact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticMoverIssue {
    pub path: String,
    pub kind: ElasticMoverIssueKind,
    pub reason: String,
}

/// An inode pin as a stuck record keeps it. `size` and `sha256` are absent
/// for a temporary whose copy was never confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticFilePin {
    pub device: u64,
    pub inode: u64,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

/// A file record the mover could neither finish nor withdraw. Both copies
/// stay on disk: the helper never deletes anything for it, and every later
/// run leaves its path on the cache. Nothing clears one yet; an admin
/// acknowledgement is a later command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticStuckRecord {
    pub operation_id: String,
    /// Relative path on the cache and on `target`, bounded for display.
    pub path: String,
    /// SHA-256 of the exact path: what later runs match on.
    pub path_sha256: String,
    /// The data branch (`d1`...) the record was moving to.
    pub target: Option<String>,
    /// The temporary name on `target`, when the record had one.
    pub temporary: Option<String>,
    pub source: ElasticFilePin,
    pub temporary_copy: Option<ElasticFilePin>,
    pub destination: Option<ElasticFilePin>,
    pub reason: String,
    /// Whether the mover still skips this path. A record whose digest was
    /// evicted from the skip set is still shown, but nothing keeps a later
    /// run off its path any more. Derived on every read, so it costs the
    /// journal nothing while it is true.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticMoverRun {
    pub operation_id: String,
    pub resume_operation_id: String,
    pub phase: ElasticMoverPhase,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub moved_files: u64,
    pub moved_bytes: u64,
    pub skipped_files: u64,
    pub skipped_bytes: u64,
    pub refused_files: u64,
    pub issues: Vec<ElasticMoverIssue>,
    pub counts_known: bool,
    pub detail: Option<String>,
    pub coupled_sync: Option<ElasticSnapraidRun>,
    /// Every stuck record of the array, this run's included, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stuck_records: Vec<ElasticStuckRecord>,
    /// Stuck paths the mover still skips whose full record is no longer shown.
    #[serde(default)]
    pub stuck_hidden: u64,
    /// Stuck paths dropped from the skip set to make room for a newer one.
    /// Such a path is NO LONGER skipped: the mover may meet it again.
    #[serde(default)]
    pub stuck_evicted: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticMoverResult {
    pub state: ElasticResult,
    pub run: ElasticMoverRun,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticClaim {
    pub disk_id: String,
    pub wwn: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticClaimsResult {
    pub name_claimed: Option<bool>,
    pub namespace_clear: Option<bool>,
    pub disks: Vec<ElasticClaim>,
}

pub fn validate_elastic_uuid(value: &str) -> Result<(), CatalogError> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        || value == "00000000-0000-0000-0000-000000000000"
    {
        return Err(invalid("nieprawidłowy UUID Elastic"));
    }
    Ok(())
}

fn identity_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.trim() == value
        && !value.chars().any(char::is_control)
}

impl ElasticOwner {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if !identity_text(&self.org_id) || !identity_text(&self.addon_id) {
            return Err(invalid("nieprawidłowy właściciel Elastic"));
        }
        Ok(())
    }
}

impl ElasticCreateSpec {
    pub fn validate(&self) -> Result<(), CatalogError> {
        validate_elastic_uuid(&self.array_id)?;
        validate_elastic_uuid(&self.operation_id)?;
        self.owner.validate()?;
        validate_array_name(&self.name)?;
        if matches!(self.name.as_str(), "tentanas" | "tentanas-branches") {
            return Err(invalid("nazwa zajmuje stałą przestrzeń mountów aplikacji"));
        }
        if self.data.is_empty() || self.data.len() + usize::from(self.cache.is_some()) + self.parity.len() > 32 || self.parity.len() > 2 {
            return Err(invalid("Elastic wymaga danych, najwyżej 2 parity i 32 urządzeń łącznie"));
        }
        let disks: Vec<_> = self.data.iter().chain(self.cache.iter()).chain(&self.parity).collect();
        for (i, disk) in disks.iter().enumerate() {
            validate_elastic_uuid(&disk.expected_uuid)?;
            if !identity_text(&disk.disk_id) || disk.bytes == 0
                || (disk.wwn.is_none() && disk.serial.is_none())
                || disk.wwn.as_deref().is_some_and(|s| !identity_text(s))
                || disk.serial.as_deref().is_some_and(|s| !identity_text(s))
            {
                return Err(invalid("brak pełnej tożsamości nośnika Elastic"));
            }
            for previous in &disks[..i] {
                if disk.disk_id == previous.disk_id || disk.expected_uuid == previous.expected_uuid
                    || disk.wwn.as_ref().is_some_and(|v| previous.wwn.as_ref() == Some(v))
                    || disk.serial.as_ref().is_some_and(|v| previous.serial.as_ref() == Some(v))
                {
                    return Err(invalid("powtórzona tożsamość nośnika Elastic"));
                }
            }
        }
        let largest = self.data.iter().map(|disk| disk.bytes).max().unwrap_or(0);
        if self.parity.iter().any(|disk| disk.bytes < largest) {
            return Err(invalid("parity mniejsze niż największy dysk danych"));
        }
        let encoded = serde_json::to_vec(self).map_err(|e| invalid(e.to_string()))?;
        if encoded.len() > 15 * 1024 {
            return Err(invalid("zbyt duża specyfikacja Elastic"));
        }
        Ok(())
    }
}

/// mergerfs create policies the wizard offers. `mfs` (most free space) is the
/// default §5.3 names.
///
/// MEASURED (2026-09-06, mergerfs 2.42.0): these seven names are exactly the
/// ones the running binary ACCEPTS on `category.create`. The list used to
/// carry `lus`, which this build does not know, and lacked `lfs` and
/// `msplfs`, which it does — so the wizard offered one policy that would have
/// failed the mount and hid two that work.
///
/// Two behaviours of the policies themselves were measured and both matter to
/// the mover:
///   * `ff` puts consecutive new files on the FIRST branch, consistently. So
///     "new files land on the cache" is obtainable from branch ORDER alone,
///     without branch modes — see `ElasticSpec::branch_specs`.
///   * `mfs` with equal free space on every branch is DETERMINISTIC, not
///     random: three files in a row all landed on the LAST branch. A model
///     that expected it to spread writes across equal disks would be wrong.
pub const CREATE_POLICIES: &[&str] =
    &["mfs", "epmfs", "ff", "lfs", "epff", "rand", "msplfs"];

// =============================================================================
// Paths
// =============================================================================

/// The union mountpoint: the same namespace ZFS pools live in, because n05
/// lists an Elastic Array as one row next to them and a share must not have to
/// know which kind of pool it sits on.
pub fn union_path(array: &str) -> String {
    format!("{MOUNT_ROOT}{array}")
}

pub fn branch_root(array: &str) -> String {
    format!("{BRANCH_ROOT}{array}")
}

pub fn data_branch_path(array: &str, disk: &str) -> String {
    format!("{BRANCH_ROOT}{array}/data/{disk}")
}

pub fn cache_branch_path(array: &str, disk: &str) -> String {
    format!("{BRANCH_ROOT}{array}/cache/{disk}")
}

/// A parity disk is mounted like the others but is NOT a branch of the union:
/// snapraid writes one big file on it, and a union that could hand that file
/// to a client would let a share delete the array's own protection.
pub fn parity_mount_path(array: &str, index: u8) -> String {
    format!("{BRANCH_ROOT}{array}/parity/{index}")
}

/// snapraid names the first parity file `snapraid.parity` and the second
/// `snapraid.2-parity`; the directive that points at them differs the same way
/// (`parity` / `2-parity`), which is why both are derived from the index here
/// rather than spelled out twice.
pub fn parity_file_path(array: &str, index: u8) -> String {
    let name = if index <= 1 {
        "snapraid.parity".to_string()
    } else {
        format!("snapraid.{index}-parity")
    };
    format!("{}/{name}", parity_mount_path(array, index))
}

pub fn config_path(array: &str) -> String {
    format!("{CONFIG_DIR}snapraid-{array}.conf")
}

/// Whether a path is INSIDE the branch tree rather than on the union.
///
/// It exists for the share layer to call, and the reason is the §3.4 trap in
/// its sharpest form: a share exported from `/mnt/tentanas-branches/media/data/sdg`
/// would show a client one disk of the array instead of the array, would write
/// past the mover's back, and — once the mover moved a file off that disk —
/// would make the file vanish from the client's view while the union still had
/// it. A share on an Elastic Array must name the union path.
pub fn is_branch_path(path: &str) -> bool {
    path.starts_with(BRANCH_ROOT)
}

// =============================================================================
// Desired state
// =============================================================================

/// One data or cache disk of the array.
///
/// `device` is what mkfs and mount are pointed at. `disk` is the kernel name
/// (`sdg`) and is what names the branch directory and the snapraid data entry,
/// so the config file stays readable by a human standing in front of the node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Branch {
    pub disk: String,
    pub device: String,
}

/// One parity disk. `index` is 1-based and decides both the directive and the
/// file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ParityDisk {
    pub index: u8,
    pub disk: String,
    pub device: String,
}

/// Every mergerfs option this app sets on the union.
///
/// Taken apart with NO `..` rest pattern in `mergerfs_options`, for the reason
/// `block::host_object_attrs` is: a field added here must be classified — into
/// the mount options or explicitly out of them — before the crate compiles
/// again. A setting that lands in the struct, reaches the UI and never reaches
/// the mount is the exact defect that guard exists for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergerfsOptions {
    /// One of `CREATE_POLICIES`. With a cache branch present it only ever
    /// chooses among the data branches (they are `=NC`), which is where it
    /// decides which disk the mover fills next.
    pub create_policy: String,
    /// mergerfs `minfreespace`: a branch with less than this is skipped by the
    /// create policy. A size with a unit suffix, as mergerfs spells it.
    /// UNVERIFIED, including whether the percentage form this validator
    /// accepts is understood by the binary.
    pub min_free_space: String,
    /// mergerfs `moveonenospc`: when a write hits ENOSPC, move the open file
    /// to another branch and carry on instead of failing the client's write.
    pub move_on_enospc: bool,
    /// mergerfs `cache.files`. UNVERIFIED which value this array wants and
    /// UNVERIFIED that `off` is accepted by this build; `off` is mergerfs'
    /// own documented default and the one that cannot show a client stale
    /// contents after the mover moved a file underneath it.
    pub cache_files: String,
    /// FUSE `allow_other`. Required: smbd and nfsd run as other users and
    /// would otherwise get EACCES on the whole union.
    pub allow_other: bool,
    /// mergerfs `func.getattr=newest`. UNVERIFIED, and it matters here: during
    /// a mover run one file exists on two branches for a moment, and `newest`
    /// is what decides which one a `stat` reports.
    ///
    /// What IS measured about that moment (2026-09-06, mergerfs 2.42.0) is
    /// the outcome an admin cares about: moving a file from one branch to
    /// another underneath a LIVE union does not disturb the union path — the
    /// same path went on reading the same contents across the move. That is
    /// the §5.3 foundation, confirmed.
    pub getattr_newest: bool,
}

impl Default for MergerfsOptions {
    fn default() -> Self {
        Self {
            create_policy: "mfs".to_string(),
            min_free_space: "20G".to_string(),
            move_on_enospc: true,
            cache_files: "off".to_string(),
            allow_other: true,
            getattr_newest: true,
        }
    }
}

/// Everything this app puts in — or deliberately keeps out of — the array's
/// snapraid config.
///
/// Same no-`..` destructuring rule as `MergerfsOptions`, and here it has
/// already earned its keep twice over: `scrub_percent` and
/// `scrub_older_than_days` are NOT config directives, they are arguments of
/// `snapraid scrub`. A field added to this struct has to be classified into
/// one of those two places, and `snapraid_directives` is where that decision
/// is written down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapraidOptions {
    /// `blocksize` in KiB. snapraid's own default is 256.
    pub block_size_kib: u32,
    /// `autosave` in GiB: how much a long sync may process before it writes
    /// the content file, so an interrupted sync does not start from zero.
    pub autosave_gib: u32,
    /// `exclude` patterns. The defaults are the ones snapraid's own manual
    /// recommends; an admin may add to them.
    pub excludes: Vec<String>,
    /// `nohidden`.
    pub nohidden: bool,
    /// `snapraid scrub -p <percent>` — an ARGUMENT, never a directive.
    pub scrub_percent: u8,
    /// `snapraid scrub -o <days>` — an ARGUMENT, never a directive.
    pub scrub_older_than_days: u32,
}

impl Default for SnapraidOptions {
    fn default() -> Self {
        Self {
            block_size_kib: 256,
            autosave_gib: 500,
            excludes: default_excludes(),
            nohidden: false,
            scrub_percent: 8,
            scrub_older_than_days: 10,
        }
    }
}

/// What snapraid must not try to protect: directories no filesystem owns and
/// files that are recreated rather than restored. Leaving `lost+found` in
/// makes every fsck a parity change.
pub fn default_excludes() -> Vec<String> {
    vec![
        "/lost+found/".to_string(),
        "/tmp/".to_string(),
        "*.unrecoverable".to_string(),
        ".AppleDouble".to_string(),
        "._AppleDouble".to_string(),
        ".DS_Store".to_string(),
    ]
}

/// The whole desired state of one array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ElasticSpec {
    pub name: String,
    /// One of `FILESYSTEMS`, for the data and cache disks.
    pub filesystem: String,
    pub data: Vec<Branch>,
    pub cache: Vec<Branch>,
    pub parity: Vec<ParityDisk>,
    pub mergerfs: MergerfsOptions,
    pub snapraid: SnapraidOptions,
}

impl ElasticSpec {
    pub fn union_path(&self) -> String {
        union_path(&self.name)
    }

    pub fn config_path(&self) -> String {
        config_path(&self.name)
    }

    /// Whether this array has parity at all. An array with none is legal
    /// (§5.3 allows 0 parity disks) and gets NO snapraid config and NO sync
    /// step — writing a config with no `parity` line would produce a file
    /// snapraid refuses on every run.
    pub fn has_parity(&self) -> bool {
        !self.parity.is_empty()
    }

    /// The branch list in mount order, cache FIRST.
    ///
    /// Order is not cosmetic: `ff`-family create policies take the first
    /// branch that fits, and the mover reads the same order to know which way
    /// "down" is.
    pub fn branch_paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .cache
            .iter()
            .map(|b| cache_branch_path(&self.name, &b.disk))
            .collect();
        out.extend(self.data.iter().map(|b| data_branch_path(&self.name, &b.disk)));
        out
    }

    /// The mode every DATA branch of this array is mounted with.
    ///
    /// ONE function, called by `branch_specs` (the mount) and by
    /// `plan_add_data_disk` (the online add), because the two used to decide
    /// it separately and the second one did not decide it at all. A disk
    /// added to a live union went in with no mode — i.e. `RW` — while every
    /// other data branch was `NC`, so the empty new disk won `mfs` on
    /// practically every write and NEW FILES SILENTLY STOPPED GOING TO THE
    /// CACHE. After the next reboot `plan_mount` mounted the same disk `NC`
    /// again, so the array behaved one way before a restart and another after
    /// it. The knock-on was worse than the cause: those writes landed on data
    /// disks outside the last sync, while `protection()` only counts the
    /// cache — so the array reported itself protected over unsynced files.
    pub fn data_branch_mode(&self) -> &'static str {
        if self.cache.is_empty() {
            "RW"
        } else {
            "NC"
        }
    }

    /// The mergerfs branch entry for one data disk of this array, mode
    /// included. The online add sends exactly this string.
    pub fn data_branch_spec(&self, disk: &str) -> String {
        format!(
            "{}={}",
            data_branch_path(&self.name, disk),
            self.data_branch_mode()
        )
    }

    /// The same list with the mergerfs branch MODE appended to each.
    ///
    /// `=NC` on the data branches when a cache exists is what makes new files
    /// land on the cache — see the file header for why this mechanism was
    /// picked over branch order, and for the ENOSPC question it leaves open.
    /// Without a cache every data branch is `=RW` and the create policy
    /// chooses among them.
    ///
    /// The ORDER is measured to matter even so. MEASURED (2026-09-06,
    /// mergerfs 2.42.0): `ff` takes the first branch and `mfs` takes the last
    /// one when free space is equal, both deterministically — so the cache
    /// coming first is what makes an `ff` array behave like a cached array
    /// even if `=NC` turns out to mean something else than assumed.
    pub fn branch_specs(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .cache
            .iter()
            .map(|b| format!("{}=RW", cache_branch_path(&self.name, &b.disk)))
            .collect();
        out.extend(self.data.iter().map(|b| self.data_branch_spec(&b.disk)));
        out
    }
}

// =============================================================================
// The plan
// =============================================================================

/// One privileged operation. Everything an Elastic Array needs fits in these
/// verbs, which is what lets a whole create — three disks wiped, three
/// filesystems made, a union mounted and a first sync started — be shown to an
/// admin before a single byte moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElasticStep {
    /// A line for the log and nothing else. It is how a plan says why it did
    /// NOT do something — a coupled sync that is switched off, an array with
    /// no parity — instead of leaving a silence that reads as "nothing to do".
    Note(String),
    /// Create a directory (parents included). Idempotent.
    Mkdir { path: String },
    /// DESTRUCTIVE: makes a filesystem, wiping whatever the disk held. The one
    /// step in this plan that loses data, which is why `wiped_devices` exists
    /// and why the wizard's red button counts its output rather than the
    /// disks the admin ticked.
    Mkfs {
        /// Absolute path of the mkfs binary. Resolved by `Tools`, never left
        /// to `PATH` — the wrapper runs as root with a sanitized environment
        /// and a relative program name there is a different program.
        program: String,
        device: String,
        filesystem: String,
        label: String,
    },
    /// Mount one branch, parity disk included.
    Mount {
        source: String,
        mountpoint: String,
        filesystem: String,
        options: Vec<String>,
    },
    /// Mount the union over the branches. Separate from `Mount` because its
    /// source is a branch LIST and its options are mergerfs', not a
    /// filesystem's.
    MergerfsMount {
        program: String,
        branches: Vec<String>,
        mountpoint: String,
        options: Vec<String>,
    },
    Unmount { mountpoint: String },
    /// Add one branch to a union that is ALREADY MOUNTED, without taking it
    /// down.
    ///
    /// MEASURED (2026-09-06, mergerfs 2.42.0):
    /// `setfattr -n user.mergerfs.srcmounts -v "+<path>" <union>` succeeds on
    /// a live mount. That turns §5.3's headline — "add one disk at any
    /// moment" — from a remount into a genuinely online operation, and it is
    /// why `plan_add_data_disk` contains no `Unmount`: clients keep their
    /// handles and the shares never blink.
    ///
    /// The value is the leading `+` plus the branch path; `render` shows the
    /// whole xattr write, because "a branch was added to a live union" is the
    /// line an admin looks for when the new disk does not appear.
    AddBranch {
        union: String,
        branch: String,
    },
    /// Replace a file the app owns. `secret` is `false` for everything this
    /// module writes today — a snapraid config holds no credential — and the
    /// flag exists so that if one ever does, the redaction is in the ONLY
    /// renderer there is rather than in whichever caller remembered.
    WriteFile {
        path: String,
        content: String,
        secret: bool,
    },
    /// Move files from the cache branches down onto the data branches.
    ///
    /// Its own verb rather than a shell line, because it is not one: it is a
    /// walk that has to skip open and locked files, honour an age rule and a
    /// free-space rule, and report what it SKIPPED. Having it in the plan is
    /// what makes the coupled job — mover, then `snapraid sync`, one sequence
    /// — inspectable as one thing.
    MoveFiles {
        from: Vec<String>,
        to: Vec<String>,
        rules: MoverRules,
    },
    /// Run one allowlisted program with a fully built argv. No shell.
    Run { program: String, args: Vec<String> },
}

/// When the mover may move a file, and what it must leave alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MoverRules {
    /// Move nothing younger than this. 0 = no age limit. Fresh files staying
    /// on the cache is the point of having one.
    pub min_age_secs: u64,
    /// A trigger only: the core's scheduler starts a run outside the schedule
    /// when the cache has less free space than this. A run itself moves every
    /// aged file; the helper validates this value and never gates on it.
    pub min_free_pct: u8,
    /// Folders whose files are never moved down ("use cache: only"). Names are
    /// relative to the union root.
    pub pinned_folders: Vec<String>,
    /// Folders whose files are moved down on the first run whatever their age
    /// ("use cache: no"). mergerfs cannot keep them off the cache in the first
    /// place — see the file header — so this is where that policy lives.
    pub eager_folders: Vec<String>,
    /// A file another process holds open is SKIPPED and reported, never moved
    /// underneath it. §5.3, and it is why a run reports `skipped` instead of
    /// claiming it moved everything.
    ///
    /// MEASURED (2026-09-06, mergerfs 2.42.0), and it CORRECTS the reason
    /// this rule was written for. An already-open file descriptor SURVIVES
    /// the move — it holds the branch's inode, and reads through it keep
    /// working — so the danger is not that a reader breaks. It is WRITE
    /// SEMANTICS: a process that still holds the old inode goes on appending
    /// to a file the mover has already copied away, and everything it writes
    /// after the copy is lost when the old copy is unlinked. Silent data loss
    /// on exactly the file somebody was busy with, which is why this is not
    /// negotiable rather than merely polite.
    pub skip_open_files: bool,
}

/// The JSON cost of a text, which is what the journal actually pays for it.
pub(crate) fn json_size(value: &str) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |encoded| encoded.len())
}

/// The folder rules travel in the helper journal with every mover run;
/// together they stay far below its read limit.
const MOVER_FOLDER_LIMIT: usize = 128;
const MOVER_FOLDER_BYTES: usize = 16 * 1024;

fn mover_folder_valid(folder: &str) -> bool {
    !folder.is_empty()
        && folder.len() <= 4096
        && !folder.chars().any(char::is_control)
        && !folder
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

impl MoverRules {
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.min_free_pct > 100 {
            return Err(invalid(format!("minimum free space {}% is outside 0..=100", self.min_free_pct)));
        }
        if self.pinned_folders.len() + self.eager_folders.len() > MOVER_FOLDER_LIMIT
            // Bounded as the journal stores them: escaped, a byte can cost two.
            || self
                .pinned_folders
                .iter()
                .chain(&self.eager_folders)
                .map(|folder| json_size(folder))
                .sum::<usize>()
                > MOVER_FOLDER_BYTES
        {
            return Err(invalid("mover folder rules exceed their bound"));
        }
        if !self.pinned_folders.iter().all(|folder| mover_folder_valid(folder)) {
            return Err(invalid("pinned folder must be a bounded relative path"));
        }
        if !self.eager_folders.iter().all(|folder| mover_folder_valid(folder)) {
            return Err(invalid("eager folder must be a bounded relative path"));
        }
        if self.pinned_folders.iter().any(|folder| self.eager_folders.iter().any(|other| folder == other)) {
            return Err(invalid("pinned and eager folders overlap"));
        }
        Ok(())
    }
}

impl ElasticStep {
    /// Whether this step destroys data. Exactly `Mkfs` today.
    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::Mkfs { .. })
    }

    /// The path or device the step acts on — what an error message names.
    /// `Note` acts on nothing and says so rather than returning its prose.
    pub fn subject(&self) -> &str {
        match self {
            Self::Note(_) => "",
            Self::Mkdir { path } => path,
            Self::Mkfs { device, .. } => device,
            Self::Mount { mountpoint, .. } => mountpoint,
            Self::MergerfsMount { mountpoint, .. } => mountpoint,
            Self::Unmount { mountpoint } => mountpoint,
            Self::AddBranch { union, .. } => union,
            Self::WriteFile { path, .. } => path,
            Self::MoveFiles { .. } => "",
            Self::Run { program, .. } => program,
        }
    }
}

/// Every device a plan would WIPE.
///
/// The wizard's red button says "Utwórz pulę (wymaż 3 dyski)" and the number
/// has to come from the plan that will run, not from the checkboxes: a plan
/// that reformats a disk the admin thought was being kept, or that says 3 and
/// wipes 4, is the one mistake in this whole file that cannot be undone.
pub fn wiped_devices(steps: &[ElasticStep]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|s| match s {
            ElasticStep::Mkfs { device, .. } => Some(device.clone()),
            _ => None,
        })
        .collect()
}

/// The plan as text: one line per step, secret file contents replaced by
/// `***`. The only rendering there is, so nothing can reach a job log through
/// a caller that forgot to redact.
pub fn render(steps: &[ElasticStep]) -> String {
    let mut out = String::new();
    for step in steps {
        match step {
            ElasticStep::Note(text) => out.push_str(&format!("note: {text}\n")),
            ElasticStep::Mkdir { path } => out.push_str(&format!("mkdir {path}\n")),
            // Loud, and first in the sentence: this is the line an admin reads
            // to find out which disks are about to be emptied.
            ElasticStep::Mkfs {
                program,
                device,
                filesystem,
                label,
            } => out.push_str(&format!(
                "WIPE {program} {device} filesystem={filesystem} label={label}\n"
            )),
            ElasticStep::Mount {
                source,
                mountpoint,
                filesystem,
                options,
            } => out.push_str(&format!(
                "mount -t {filesystem} -o {} {source} {mountpoint}\n",
                options.join(",")
            )),
            ElasticStep::MergerfsMount {
                program,
                branches,
                mountpoint,
                options,
            } => out.push_str(&format!(
                "{program} -o {} {} {mountpoint}\n",
                options.join(","),
                branches.join(":")
            )),
            ElasticStep::Unmount { mountpoint } => out.push_str(&format!("umount {mountpoint}\n")),
            // The mode is part of `branch` and is printed with it: "did the
            // new disk join as NC or as RW" is the question this line has to
            // be able to answer months later.
            ElasticStep::AddBranch { union, branch } => out.push_str(&format!(
                "setfattr -n user.mergerfs.srcmounts -v +{branch} {union}\n"
            )),
            ElasticStep::WriteFile {
                path,
                content,
                secret,
            } => {
                if *secret {
                    out.push_str(&format!("write {path} = ***\n"));
                } else {
                    out.push_str(&format!("write {path}:\n"));
                    for line in content.lines() {
                        out.push_str(&format!("    {line}\n"));
                    }
                }
            }
            ElasticStep::MoveFiles { from, to, rules } => {
                out.push_str(&format!(
                    "move {} -> {} (older than {}s, extra run below {}% cache free, {})\n",
                    from.join(":"),
                    to.join(":"),
                    rules.min_age_secs,
                    rules.min_free_pct,
                    if rules.skip_open_files {
                        "skipping open files"
                    } else {
                        "NOT skipping open files"
                    }
                ));
                for folder in &rules.pinned_folders {
                    out.push_str(&format!("    keep on cache: {folder}\n"));
                }
                for folder in &rules.eager_folders {
                    out.push_str(&format!("    move at once: {folder}\n"));
                }
            }
            ElasticStep::Run { program, args } => {
                out.push_str(&format!("run {program} {}\n", args.join(" ")));
            }
        }
    }
    out
}

// =============================================================================
// Options and directives — the two enumerations
// =============================================================================

/// The mergerfs `-o` list this app mounts a union with.
///
/// `MergerfsOptions` is destructured with NO `..`: a new field does not
/// compile until somebody decides here whether it becomes a mount option.
///
/// PARTLY MEASURED. `category.create` and the seven policy names it accepts
/// are MEASURED (2026-09-06, mergerfs 2.42.0) — see `CREATE_POLICIES`.
/// `minfreespace`, `cache.files`, `moveonenospc`, `allow_other` and
/// `func.getattr` are still UNVERIFIED: their spellings come from mergerfs'
/// documentation and no mount has been made with them on a live node. A
/// single unaccepted option fails the WHOLE mount, so this is the list to
/// measure first.
pub fn mergerfs_options(options: &MergerfsOptions) -> Result<Vec<String>, CatalogError> {
    let MergerfsOptions {
        create_policy,
        min_free_space,
        move_on_enospc,
        cache_files,
        allow_other,
        getattr_newest,
    } = options;

    if !CREATE_POLICIES.contains(&create_policy.as_str()) {
        return Err(invalid(format!("unknown create policy '{create_policy}'")));
    }
    if !matches!(cache_files.as_str(), "off" | "partial" | "full" | "auto-full") {
        return Err(invalid(format!("unknown cache.files value '{cache_files}'")));
    }
    validate_size(min_free_space)?;

    let mut out = vec![
        format!("category.create={create_policy}"),
        format!("minfreespace={min_free_space}"),
        format!("cache.files={cache_files}"),
    ];
    if *move_on_enospc {
        out.push("moveonenospc=true".to_string());
    }
    if *allow_other {
        out.push("allow_other".to_string());
    }
    if *getattr_newest {
        out.push("func.getattr=newest".to_string());
    }
    Ok(out)
}

/// Every `(directive, value)` pair of the array's snapraid config, in file
/// order.
///
/// `SnapraidOptions` is destructured with NO `..` for the same reason
/// `mergerfs_options` destructures its own struct, and the two `scrub_*`
/// fields show why it is worth the noise: they are bound and dropped HERE,
/// with a comment saying where they really go, instead of quietly failing to
/// appear in a file they never belonged in.
///
/// MEASURED (2026-09-06, snapraid 14.7): a configuration in exactly this
/// shape — `parity`, `content`, `data <name> <path>` — was accepted by the
/// running binary with no warnings. `data` replaced `disk` in snapraid 11, so
/// a node running something older would still need `disk`; that older shape
/// is UNVERIFIED and unsupported here.
///
/// MEASURED (2026-09-06, snapraid 14.7), and it is a REFUSAL this file cannot
/// make: snapraid rejects a configuration whose data directories sit on the
/// same device — `Disks 'X' and 'Y' are on the same device.` The helper only
/// sees device STRINGS and cannot tell two paths on one disk apart from two
/// disks, so the check lives where the inventory does
/// (`tentanas::elastic::layout_refusals`, code `data_disks_same_device`).
pub fn snapraid_directives(spec: &ElasticSpec) -> Result<Vec<(String, String)>, CatalogError> {
    let SnapraidOptions {
        block_size_kib,
        autosave_gib,
        excludes,
        nohidden,
        // Arguments of `snapraid scrub`, not directives of the config file.
        // `snapraid_args` is where they are used; they are bound here so that
        // this enumeration covers the whole struct and a future field cannot
        // slip past both places.
        scrub_percent: _,
        scrub_older_than_days: _,
    } = &spec.snapraid;

    if !spec.has_parity() {
        return Err(invalid(
            "an array with no parity disk has no snapraid configuration".to_string(),
        ));
    }
    if *block_size_kib == 0 || *block_size_kib > 65_536 {
        return Err(invalid(format!("blocksize {block_size_kib} KiB")));
    }

    let mut out = Vec::new();
    for parity in &spec.parity {
        out.push((
            parity_directive(parity.index)?.to_string(),
            parity_file_path(&spec.name, parity.index),
        ));
    }
    // Kopie na systemowym FS i każdym parity dają parity_count + 1 nośników;
    // poza branchami unii klient nie może usunąć mapy ochrony przez share.
    out.push(("content".to_string(), format!("{CONFIG_DIR}{}-{CONTENT_FILE}", spec.name)));
    for parity in &spec.parity {
        out.push((
            "content".to_string(),
            format!(
                "{}/{CONTENT_FILE}",
                parity_mount_path(&spec.name, parity.index)
            ),
        ));
    }
    // The DATA disks only. The cache is deliberately absent: files on it are
    // not covered by parity, which is the whole unprotected window §5.3 makes
    // the admin look at. A cache listed here would make snapraid claim to
    // protect bytes that move out from under it on every mover run.
    for branch in &spec.data {
        out.push((
            "data".to_string(),
            format!(
                "{} {}",
                branch.disk,
                data_branch_path(&spec.name, &branch.disk)
            ),
        ));
    }
    for pattern in excludes {
        out.push(("exclude".to_string(), pattern.clone()));
    }
    out.push(("blocksize".to_string(), block_size_kib.to_string()));
    if *autosave_gib > 0 {
        out.push(("autosave".to_string(), autosave_gib.to_string()));
    }
    if *nohidden {
        out.push(("nohidden".to_string(), String::new()));
    }
    Ok(out)
}

fn parity_directive(index: u8) -> Result<&'static str, CatalogError> {
    match index {
        1 => Ok("parity"),
        2 => Ok("2-parity"),
        other => Err(invalid(format!(
            "parity index {other} is outside 1..={MAX_PARITY}"
        ))),
    }
}

/// The array's snapraid config file, rendered.
pub fn snapraid_config(spec: &ElasticSpec) -> Result<String, CatalogError> {
    let mut out = format!(
        "# Managed by TentaNas — Elastic Array '{}'. Do not edit: the app\n\
         # rewrites this file from tentanas.db on every change (plan-02 §3.4).\n",
        spec.name
    );
    for (directive, value) in snapraid_directives(spec)? {
        if value.is_empty() {
            out.push_str(&format!("{directive}\n"));
        } else {
            out.push_str(&format!("{directive} {value}\n"));
        }
    }
    Ok(out)
}

// =============================================================================
// snapraid commands
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapraidAction {
    /// Bring parity up to date with what is on the data disks.
    Sync,
    /// Re-read a percentage of the oldest blocks and compare them with parity.
    Scrub,
    /// Report what parity knows, without touching anything.
    ///
    /// MEASURED (2026-09-06, snapraid 14.7) — and the measurement is a
    /// WARNING, not a feature. `status` does NOT report unsynced data: 12 MiB
    /// were written to a data disk without a sync and the output came back
    /// byte for byte identical to the run before the write. So nothing in
    /// this command can answer "how much is unprotected", and a model that
    /// parsed it for that would report a protected array over unprotected
    /// bytes. `diff` is what sees a change; the cache figure is ours.
    ///
    /// The output shape, recorded here so the parser that eventually reads it
    /// has a fixture from a real binary rather than an invented one — columns
    /// `Files | Fragmented Files | Excess Fragments | Wasted GB | Used GB |
    /// Free GB | Use% | Name`, a summary row after a rule, a scrub-age
    /// histogram, and the sentences `The oldest block was scrubbed N days
    /// ago, the median M, the newest K.`, `No sync is in progress.`, `100% of
    /// the array is not scrubbed.`, `No file has a zero sub-second
    /// timestamp.` Before the first sync it also prints `WARNING! Free space
    /// info will be valid after the first sync.` and `The array is empty.` —
    /// so a parser must not read the free-space columns of a fresh array.
    Status,
    /// What has changed since the last sync, without touching anything.
    ///
    /// MEASURED (2026-09-06, snapraid 14.7): this — not `status` — is where a
    /// difference shows up. It prints `add <path>` per changed file, then a
    /// counted summary (`5 equal / 1 added / 0 removed / 0 updated / 0 moved
    /// / 0 copied / 0 relocated / 0 restored`) and `There are differences!`,
    /// and it EXITS 2. See `snapraid_outcome`.
    Diff,
    /// Rebuild one named data disk from parity — the recovery wizard's engine.
    Fix { disk: String },
}

/// What a snapraid exit code MEANS for the action that produced it.
///
/// This exists because of one measured code. MEASURED (2026-09-06, snapraid
/// 14.7): `status` = 0, `sync` = 0, `scrub` = 0, **`diff` = 2**, `check` = 1.
/// A job runner that treats non-zero as failure would report every successful
/// `diff` as a failed job — and `diff` is the only command that can answer
/// "what is not yet in parity", so the one thing this app needs most would be
/// permanently red.
///
/// The `check` = 1 reading is recorded but NOT interpreted: it was taken on an
/// array that had a difference, so it does not establish what `check` returns
/// on a clean array, and there is no `Check` action here to hang it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapraidOutcome {
    /// The run did what it was asked to and found nothing to report.
    Ok,
    /// The run SUCCEEDED and found differences. Not a failure.
    Differences,
    /// The run failed, or was killed.
    Failed,
}

pub fn snapraid_outcome(action: &SnapraidAction, code: i32) -> SnapraidOutcome {
    if code == 0 {
        return SnapraidOutcome::Ok;
    }
    // A negative code is `broker::run_unprivileged`'s stand-in for "no exit
    // status", i.e. killed by a signal. That is never "differences" — see the
    // segfaulting build in `tentanas::elastic::snapraid_health`.
    if code < 0 {
        return SnapraidOutcome::Failed;
    }
    match (action, code) {
        (SnapraidAction::Diff, 2) => SnapraidOutcome::Differences,
        _ => SnapraidOutcome::Failed,
    }
}

/// The argv snapraid is run with. Options come BEFORE the command word, which
/// is snapraid's own order.
///
/// MEASURED (2026-09-06, snapraid 14.7) for `-c <conf>` with `status`, `sync`,
/// `scrub` and `diff`: all four ran and returned the codes `snapraid_outcome`
/// classifies. UNVERIFIED: `-p` / `-o` on `scrub`, and `-d <disk> fix`.
pub fn snapraid_args(spec: &ElasticSpec, action: &SnapraidAction) -> Result<Vec<String>, CatalogError> {
    let mut args = vec!["-c".to_string(), spec.config_path()];
    match action {
        SnapraidAction::Sync => args.push("sync".to_string()),
        SnapraidAction::Status => args.push("status".to_string()),
        SnapraidAction::Diff => args.push("diff".to_string()),
        SnapraidAction::Scrub => {
            args.push("-p".to_string());
            args.push("full".to_string());
            args.push("scrub".to_string());
        }
        SnapraidAction::Fix { disk } => {
            // The disk must be one THIS array carries. A `fix -d` naming
            // anything else would either do nothing or, with a name snapraid
            // does know from another config, rebuild the wrong disk from the
            // wrong parity.
            if !spec.data.iter().any(|b| b.disk == *disk) {
                return Err(invalid(format!(
                    "'{disk}' is not a data disk of array '{}'",
                    spec.name
                )));
            }
            args.push("-d".to_string());
            args.push(disk.clone());
            args.push("fix".to_string());
        }
    }
    Ok(args)
}

// =============================================================================
// Validation
// =============================================================================

/// An array name. It becomes a directory under `/mnt/`, a directory under the
/// branch root and a file name in `/etc/tentanas/`, so it has the same shape a
/// pool name has.
pub fn validate_array_name(name: &str) -> Result<(), CatalogError> {
    if !component_ok(name) || name.contains('/') || name.len() > 64 {
        return Err(invalid(format!("array name '{name}'")));
    }
    if !name.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric()) {
        return Err(invalid(format!(
            "array name '{name}' must start with a letter or a digit"
        )));
    }
    Ok(())
}

/// A device an array may format or mount.
///
/// Wider than `validate_device` on purpose, and narrow in a different
/// direction: `/dev/disk/by-id/<link>` is accepted because a branch mount has
/// to survive a reboot that renames `sdg` to `sdh`, and a mount table keyed on
/// kernel names would then mount a data disk in another disk's place — with
/// mergerfs happily serving the result. `/dev/sdg` stays accepted for the
/// nodes and the tests that have no by-id links.
pub fn validate_branch_device(device: &str) -> Result<(), CatalogError> {
    if let Some(link) = device.strip_prefix("/dev/disk/by-id/") {
        return if component_ok(link) && link.len() <= 200 {
            Ok(())
        } else {
            Err(invalid(format!("device link '{device}'")))
        };
    }
    crate::validate_device(device)
}

/// A mergerfs/mount size such as `20G`. Digits and one unit letter — anything
/// else would travel into an option string this app builds.
fn validate_size(value: &str) -> Result<(), CatalogError> {
    let ok = (1..=16).contains(&value.len())
        && value.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'K' | b'M' | b'G' | b'T' | b'%'));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("size '{value}'")))
    }
}

/// Everything about a spec that can be judged without looking at the node.
///
/// Sizes are NOT judged here — the helper does not know how big a disk is, and
/// the parity rule of §5.3 ("parity ≥ the largest data disk") is a HARD
/// REFUSAL computed in the core, where the inventory lives
/// (`tentanas::elastic::layout_refusals`).
///
/// MEASURED (2026-09-06, snapraid 14.7), and it is why that refusal has to be
/// ours: snapraid does NOT validate parity capacity up front. A 64 MiB parity
/// file was configured against a 200 MiB data disk holding 40 MiB and the
/// sync succeeded, printing `Resizing...`. What that establishes is narrow —
/// no capacity check at configuration time, and none at sync time while the
/// data still fits — not that snapraid would accept a genuinely short parity.
/// The consequence for the admin is the same either way: nothing warns them
/// when they build it, and snapraid refuses only later, once the data has
/// grown past the parity disk, which is the worst moment to find out.
pub fn validate_spec(spec: &ElasticSpec) -> Result<(), CatalogError> {
    validate_array_name(&spec.name)?;
    if !FILESYSTEMS.contains(&spec.filesystem.as_str()) {
        return Err(invalid(format!(
            "'{}' is not a filesystem this app makes",
            spec.filesystem
        )));
    }
    if spec.data.is_empty() {
        return Err(invalid("an Elastic Array needs at least one data disk"));
    }
    if spec.parity.len() > MAX_PARITY {
        return Err(invalid(format!(
            "{} parity disks: the wizard offers at most {MAX_PARITY}",
            spec.parity.len()
        )));
    }
    // A disk in two roles would be formatted twice by the same plan and would
    // then hold either parity or data, whichever step ran last.
    let mut claimed: Vec<(&str, &'static str)> = Vec::new();
    for branch in &spec.data {
        validate_branch_device(&branch.device)?;
        if !component_ok(&branch.disk) {
            return Err(invalid(format!("disk name '{}'", branch.disk)));
        }
        claimed.push((branch.device.as_str(), "data"));
    }
    for branch in &spec.cache {
        validate_branch_device(&branch.device)?;
        if !component_ok(&branch.disk) {
            return Err(invalid(format!("disk name '{}'", branch.disk)));
        }
        claimed.push((branch.device.as_str(), "cache"));
    }
    for parity in &spec.parity {
        validate_branch_device(&parity.device)?;
        parity_directive(parity.index)?;
        claimed.push((parity.device.as_str(), "parity"));
    }
    for i in 0..claimed.len() {
        for j in (i + 1)..claimed.len() {
            if claimed[i].0 == claimed[j].0 {
                return Err(invalid(format!(
                    "device {} is claimed as {} and as {} in the same array",
                    claimed[i].0, claimed[i].1, claimed[j].1
                )));
            }
        }
    }
    let mut indexes: Vec<u8> = spec.parity.iter().map(|p| p.index).collect();
    indexes.sort_unstable();
    indexes.dedup();
    if indexes.len() != spec.parity.len() {
        return Err(invalid("two parity disks share one index"));
    }
    mergerfs_options(&spec.mergerfs)?;
    if spec.has_parity() {
        snapraid_directives(spec)?;
    }
    Ok(())
}

// =============================================================================
// Plans
// =============================================================================

/// What this node can see about an array right now.
///
/// `known` is the difference between "no branch is mounted" and "this node
/// could not read the mount table". Acting on the first is a reconcile; acting
/// on the second would mount a union over branches that are already mounted,
/// or worse, mount a union over EMPTY directories and let every write land on
/// the root filesystem — the §3.4 empty-share trap with the array's whole data
/// path behind it. So an unknown mount table refuses to produce a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    pub known: bool,
    /// Absolute path -> whether a filesystem is mounted there.
    pub mounted: BTreeMap<String, bool>,
}

impl Observed {
    /// A reading where everything is unmounted — the state of a node that has
    /// just booted, and the only shape a caller may construct by hand.
    pub fn nothing_mounted() -> Self {
        Self {
            known: true,
            mounted: BTreeMap::new(),
        }
    }

    fn is_mounted(&self, path: &str) -> bool {
        self.mounted.get(path).copied().unwrap_or(false)
    }
}

/// The mkfs label of one branch. XFS caps a label at 12 bytes.
///
/// UNIQUE BY CONSTRUCTION, and it was not. The first version was
/// `tn-{array}-{role}-{disk}` truncated to 12, which made every data disk of
/// `media` come out as `tn-media-dat` — every label in a multi-disk array
/// identical, which is the one property a label has to have. Twelve bytes
/// cannot hold the array, the role AND the disk, so the array name is what
/// goes: `tnd1-sdg`, `tnd2-sdh`, `tnc1-nvme2n1`, `tnp1-sdj`. Role and index
/// occupy the first four bytes and differ before any truncation can reach
/// them, so two labels of one array can never collide however long the disk
/// names are; the disk suffix is a hint for a human with a rescue shell and
/// may truncate harmlessly.
///
/// Losing the array name costs nothing that matters: nothing mounts by label
/// (the plan uses the device), and which array a disk belongs to is answered
/// by `tentanas.db` and by the snapraid config on the disk itself.
fn branch_label(role: char, index: usize, disk: &str) -> String {
    let mut label = format!("tn{role}{index}-{disk}");
    label.truncate(12);
    label
}

fn mount_options(filesystem: &str) -> Vec<String> {
    // `noatime`: every read of a media file would otherwise be a metadata
    // write, and on a snapraid array a metadata write is a parity change the
    // next sync has to carry.
    let mut opts = vec!["noatime".to_string()];
    if filesystem == "ext4" {
        opts.push("user_xattr".to_string());
    }
    opts
}

fn find(name: &'static str, candidates: &[&str]) -> Result<String, CatalogError> {
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).is_file())
        .map(|p| (*p).to_string())
        .ok_or(CatalogError::ToolMissing(name))
}

/// Tool paths a plan needs, resolved once.
///
/// Injected rather than looked up inside each builder so the plans are
/// testable on a machine with neither mergerfs nor snapraid — the same reason
/// `targets::target_state` takes `installed` as a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub mkfs_xfs: String,
    pub mkfs_ext4: String,
    pub mergerfs: String,
    pub snapraid: String,
}

impl Tools {
    /// The paths as they exist on THIS node. A missing tool is an error with
    /// the tool's name, never a silent fallback to `PATH`.
    pub fn resolve(spec: &ElasticSpec) -> Result<Self, CatalogError> {
        Ok(Self {
            mkfs_xfs: if spec.filesystem == "xfs" {
                find("mkfs.xfs", MKFS_XFS)?
            } else {
                String::new()
            },
            mkfs_ext4: if spec.filesystem == "ext4" {
                find("mkfs.ext4", MKFS_EXT4)?
            } else {
                String::new()
            },
            mergerfs: find("mergerfs", MERGERFS)?,
            snapraid: if spec.has_parity() {
                find("snapraid", SNAPRAID)?
            } else {
                String::new()
            },
        })
    }

    fn mkfs(&self, filesystem: &str) -> Result<&str, CatalogError> {
        match filesystem {
            "xfs" => Ok(&self.mkfs_xfs),
            "ext4" => Ok(&self.mkfs_ext4),
            other => Err(invalid(format!("no mkfs for '{other}'"))),
        }
    }

    /// Placeholder paths for a preview rendered on a node that may not have
    /// the tools installed. The preview is a document, not an execution, and
    /// refusing to SHOW an admin the plan because snapraid is not installed
    /// yet is exactly backwards — the plan is what tells them to install it.
    pub fn for_preview() -> Self {
        Self {
            mkfs_xfs: "mkfs.xfs".to_string(),
            mkfs_ext4: "mkfs.ext4".to_string(),
            mergerfs: "mergerfs".to_string(),
            snapraid: "snapraid".to_string(),
        }
    }
}

/// Creating the array: format every disk, mount every branch, write the
/// snapraid config, mount the union, take the first parity sync.
///
/// The sync is LAST and it is part of the plan, not a follow-up: until it has
/// run, an array with parity disks is an array whose parity protects nothing,
/// and an admin who closed the wizard would have no way to know that.
pub fn plan_create(spec: &ElasticSpec, tools: &Tools) -> Result<Vec<ElasticStep>, CatalogError> {
    validate_spec(spec)?;
    let mut steps = Vec::new();

    for (role, branches) in [('d', &spec.data), ('c', &spec.cache)] {
        for (i, branch) in branches.iter().enumerate() {
            let mountpoint = if role == 'd' {
                data_branch_path(&spec.name, &branch.disk)
            } else {
                cache_branch_path(&spec.name, &branch.disk)
            };
            steps.push(ElasticStep::Mkfs {
                program: tools.mkfs(&spec.filesystem)?.to_string(),
                device: branch.device.clone(),
                filesystem: spec.filesystem.clone(),
                label: branch_label(role, i + 1, &branch.disk),
            });
            steps.push(ElasticStep::Mkdir {
                path: mountpoint.clone(),
            });
            steps.push(ElasticStep::Mount {
                source: branch.device.clone(),
                mountpoint,
                filesystem: spec.filesystem.clone(),
                options: mount_options(&spec.filesystem),
            });
        }
    }

    for parity in &spec.parity {
        let mountpoint = parity_mount_path(&spec.name, parity.index);
        steps.push(ElasticStep::Mkfs {
            program: tools.mkfs(&spec.filesystem)?.to_string(),
            device: parity.device.clone(),
            filesystem: spec.filesystem.clone(),
            label: branch_label('p', parity.index as usize, &parity.disk),
        });
        steps.push(ElasticStep::Mkdir {
            path: mountpoint.clone(),
        });
        steps.push(ElasticStep::Mount {
            source: parity.device.clone(),
            mountpoint,
            filesystem: spec.filesystem.clone(),
            options: mount_options(&spec.filesystem),
        });
    }

    if spec.has_parity() {
        steps.push(ElasticStep::Mkdir {
            path: CONFIG_DIR.trim_end_matches('/').to_string(),
        });
        steps.push(ElasticStep::WriteFile {
            path: spec.config_path(),
            content: snapraid_config(spec)?,
            secret: false,
        });
    } else {
        steps.push(ElasticStep::Note(
            "no parity disk: nothing in this array is protected against a disk failure, \
             and no snapraid configuration is written"
                .to_string(),
        ));
    }

    steps.extend(union_steps(spec, tools)?);

    if spec.has_parity() {
        steps.push(ElasticStep::Run {
            program: tools.snapraid.clone(),
            args: snapraid_args(spec, &SnapraidAction::Sync)?,
        });
    }
    Ok(steps)
}

fn union_steps(spec: &ElasticSpec, tools: &Tools) -> Result<Vec<ElasticStep>, CatalogError> {
    Ok(vec![
        ElasticStep::Mkdir {
            path: spec.union_path(),
        },
        ElasticStep::MergerfsMount {
            program: tools.mergerfs.clone(),
            branches: spec.branch_specs(),
            mountpoint: spec.union_path(),
            options: mergerfs_options(&spec.mergerfs)?,
        },
    ])
}

/// Putting an existing array back after a reboot, or repairing a partial
/// state: mount the branches that are not mounted, then the union.
///
/// It NEVER contains an `Mkfs` step — there is a test pinning exactly that —
/// because this plan runs unattended (§3.4: mounts come back through TentaNas
/// and nothing else), and an unattended plan that could format a disk is one
/// misjudged observation away from destroying the array it was restoring.
///
/// The ONE thing it refuses is an unreadable mount table (`Observed::known`),
/// and that is all — a sentence here used to promise a refusal for a branch
/// whose device is absent, which this function has never made and should not:
/// whether a disk is on the node is a question the CORE answers, with the
/// inventory, before it asks for a plan at all
/// (`tentanas::elastic::array_state`, `BranchProbe::device_present`). A
/// promise in a comment that the code does not keep is worse than no comment,
/// because the next reader stops looking for the guard.
pub fn plan_mount(
    spec: &ElasticSpec,
    observed: &Observed,
    tools: &Tools,
) -> Result<Vec<ElasticStep>, CatalogError> {
    validate_spec(spec)?;
    if !observed.known {
        return Err(invalid(
            "this node could not read its mount table, so it will not mount anything: \
             a union mounted over unmounted branches writes to the root filesystem"
                .to_string(),
        ));
    }
    let mut steps = Vec::new();
    let mut mount_branch = |source: &str, mountpoint: String| {
        if observed.is_mounted(&mountpoint) {
            return;
        }
        steps.push(ElasticStep::Mkdir {
            path: mountpoint.clone(),
        });
        steps.push(ElasticStep::Mount {
            source: source.to_string(),
            mountpoint,
            filesystem: spec.filesystem.clone(),
            options: mount_options(&spec.filesystem),
        });
    };
    for branch in &spec.data {
        mount_branch(&branch.device, data_branch_path(&spec.name, &branch.disk));
    }
    for branch in &spec.cache {
        mount_branch(&branch.device, cache_branch_path(&spec.name, &branch.disk));
    }
    for parity in &spec.parity {
        mount_branch(&parity.device, parity_mount_path(&spec.name, parity.index));
    }
    if observed.is_mounted(&spec.union_path()) {
        steps.push(ElasticStep::Note(format!(
            "{} is already mounted",
            spec.union_path()
        )));
    } else {
        steps.extend(union_steps(spec, tools)?);
    }
    Ok(steps)
}

/// The array's headline feature: one more disk, at any time.
///
/// The union is remounted rather than grown in place because mergerfs takes
/// its branch list at mount time; the config is rewritten because snapraid
/// must learn the disk exists; and the sync comes last because until it runs
/// the new disk is outside parity. The mkfs is here — this is the one
/// non-create plan that formats anything, and it formats exactly the disk
/// being added.
pub fn plan_add_data_disk(
    spec_after: &ElasticSpec,
    added: &Branch,
    tools: &Tools,
) -> Result<Vec<ElasticStep>, CatalogError> {
    validate_spec(spec_after)?;
    if !spec_after.data.iter().any(|b| b.disk == added.disk) {
        return Err(invalid(format!(
            "'{}' is not among the data disks of the array it is being added to",
            added.disk
        )));
    }
    let mountpoint = data_branch_path(&spec_after.name, &added.disk);
    let mut steps = vec![
        ElasticStep::Mkfs {
            program: tools.mkfs(&spec_after.filesystem)?.to_string(),
            device: added.device.clone(),
            filesystem: spec_after.filesystem.clone(),
            label: branch_label(
                'd',
                // The index it will hold in the array it is joining, so an
                // added disk gets the same label a rebuild from scratch would
                // give it.
                spec_after
                    .data
                    .iter()
                    .position(|b| b.disk == added.disk)
                    .map(|i| i + 1)
                    .unwrap_or(spec_after.data.len()),
                &added.disk,
            ),
        },
        ElasticStep::Mkdir {
            path: mountpoint.clone(),
        },
        ElasticStep::Mount {
            source: added.device.clone(),
            mountpoint,
            filesystem: spec_after.filesystem.clone(),
            options: mount_options(&spec_after.filesystem),
        },
        // The union stays UP. This used to be an unmount followed by a fresh
        // mergerfs mount, on the assumption that a branch list is fixed at
        // mount time; MEASURED (2026-09-06, mergerfs 2.42.0) it is not, and
        // the xattr write below adds the branch to a running union. The
        // difference is not cosmetic: the remount version dropped every SMB
        // and NFS client's handles on the array's headline operation.
        //
        // The branch is mounted BEFORE it is added, in that order, for the
        // same reason the create plan mounts branches before the union: a
        // branch added while its filesystem is not mounted would hand the
        // union an empty directory on the root filesystem.
        ElasticStep::AddBranch {
            union: spec_after.union_path(),
            // The MODE travels with it. Without it the disk joins as `RW`
            // while every other data branch is `NC`, and new files stop going
            // to the cache from that moment until the next reboot mounts the
            // union again — see `ElasticSpec::data_branch_mode`.
            branch: spec_after.data_branch_spec(&added.disk),
        },
    ];
    if spec_after.has_parity() {
        steps.push(ElasticStep::WriteFile {
            path: spec_after.config_path(),
            content: snapraid_config(spec_after)?,
            secret: false,
        });
        steps.push(ElasticStep::Run {
            program: tools.snapraid.clone(),
            args: snapraid_args(spec_after, &SnapraidAction::Sync)?,
        });
    }
    Ok(steps)
}

/// One snapraid operation on its own — the "Sync teraz" / "Scrub teraz"
/// buttons and the recovery wizard.
pub fn plan_snapraid(
    spec: &ElasticSpec,
    action: &SnapraidAction,
    tools: &Tools,
) -> Result<Vec<ElasticStep>, CatalogError> {
    validate_spec(spec)?;
    if !spec.has_parity() {
        return Err(invalid(format!(
            "array '{}' has no parity disk, so there is nothing to sync, scrub or fix",
            spec.name
        )));
    }
    Ok(vec![ElasticStep::Run {
        program: tools.snapraid.clone(),
        args: snapraid_args(spec, action)?,
    }])
}

/// The mover, and — when it is coupled — the sync that follows it, as ONE
/// plan.
///
/// §5.3 makes the coupling a requirement, not a convenience: a file the mover
/// has just put on a data disk is covered by parity only after the next sync,
/// so a mover run without one moves bytes OUT of the window this app reports
/// and into a window it does not. Rendering both as one plan is what makes
/// "one sequential job" a fact of the code instead of a sentence in a plan.
///
/// With `coupled_sync` off the plan carries a `Note` saying so — the admin
/// switched it off in the schedule dialog and the job log has to show which
/// choice was in force for THIS run.
pub fn plan_mover(
    spec: &ElasticSpec,
    rules: &MoverRules,
    coupled_sync: bool,
    tools: &Tools,
) -> Result<Vec<ElasticStep>, CatalogError> {
    validate_spec(spec)?;
    if spec.cache.is_empty() {
        return Err(invalid(format!(
            "array '{}' has no cache disk, so the mover has nothing to move",
            spec.name
        )));
    }
    rules.validate()?;
    let mut steps = vec![ElasticStep::MoveFiles {
        from: spec
            .cache
            .iter()
            .map(|b| cache_branch_path(&spec.name, &b.disk))
            .collect(),
        to: spec
            .data
            .iter()
            .map(|b| data_branch_path(&spec.name, &b.disk))
            .collect(),
        rules: rules.clone(),
    }];
    match (coupled_sync, spec.has_parity()) {
        (true, true) => steps.push(ElasticStep::Run {
            program: tools.snapraid.clone(),
            args: snapraid_args(spec, &SnapraidAction::Sync)?,
        }),
        (true, false) => steps.push(ElasticStep::Note(
            "coupled sync is on, but this array has no parity disk: the moved files are \
             not protected by anything"
                .to_string(),
        )),
        (false, _) => steps.push(ElasticStep::Note(
            "coupled sync is OFF: the moved files stay outside parity until the next sync"
                .to_string(),
        )),
    }
    Ok(steps)
}

/// Dissolving the array: unmount the union, then every branch, and touch NO
/// filesystem.
///
/// The danger zone's promise is precise — "dane na dyskach XFS pozostają
/// czytelne osobno" — so this plan must contain no `Mkfs` and no removal of a
/// data disk's contents. The snapraid config and the union go; the disks and
/// what is on them stay, each mountable on its own. The parity file is left
/// where it is: it costs nothing, and an admin who dissolved an array by
/// mistake still has it.
pub fn plan_dissolve(spec: &ElasticSpec) -> Result<Vec<ElasticStep>, CatalogError> {
    validate_spec(spec)?;
    let mut steps = vec![
        ElasticStep::Note(
            "the disks keep their filesystems and their files: every data disk stays \
             mountable on its own"
                .to_string(),
        ),
        ElasticStep::Unmount {
            mountpoint: spec.union_path(),
        },
    ];
    for branch in &spec.cache {
        steps.push(ElasticStep::Unmount {
            mountpoint: cache_branch_path(&spec.name, &branch.disk),
        });
    }
    for branch in &spec.data {
        steps.push(ElasticStep::Unmount {
            mountpoint: data_branch_path(&spec.name, &branch.disk),
        });
    }
    for parity in &spec.parity {
        steps.push(ElasticStep::Unmount {
            mountpoint: parity_mount_path(&spec.name, parity.index),
        });
    }
    Ok(steps)
}

pub(crate) mod execution {
    use super::*;
    use std::ffi::CString;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, RawFd};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::{AtomicU64, Ordering};
    use crate::elastic_namespace::{self, Anchor, Entry, Paths, Worker};
    use std::collections::BTreeSet;
    use crate::elastic_transfer::{path_digest, transfer_file, ScanEntry, ScanFile, TransferEnd, TransferFile, TransferFilePhase};

    const ROOT: &str = "/var/lib/tentanas/elastic";
    const JOURNAL_LIMIT: u64 = 128 * 1024;
    /// The one in-flight file record, serialized with every identity pinned.
    /// With the bounded rules, issues and detail it keeps a journal carrying a
    /// transfer well below `JOURNAL_LIMIT`, whatever the number of files.
    const TRANSFER_RECORD_LIMIT: usize = 48 * 1024;
    const TRANSFER_ISSUE_LIMIT: usize = 16;
    const TRANSFER_TEXT_LIMIT: usize = 256;
    const TRANSFER_DETAIL_LIMIT: usize = 1024;
    /// The mergerfs branch string of the anchor, as the journal stores it.
    const ANCHOR_SOURCE_BYTES: usize = 16 * 1024;
    /// Full stuck records the journal shows. When the list is full the
    /// OLDEST record is summarised away: its digest stays in the skip set, so
    /// the path is still skipped, and the result reports how many stuck paths
    /// it no longer shows in full. Worst case, with every text at its bound
    /// and JSON-escaped, the list costs about 16 KiB of `JOURNAL_LIMIT`.
    const STUCK_LIMIT: usize = 8;
    /// Digits a transfer temporary's sequence may carry, in the journal and on
    /// the branch. A `u64` prints up to 20, but 10 already allows 10 000 000 000
    /// files inside ONE operation — far beyond anything reachable — and every
    /// digit is paid for by each skip entry, each ring entry and each stuck
    /// record. Ten costs nothing reachable and buys about 1 KB of worst case.
    const TEMPORARY_SEQUENCE_DIGITS: usize = 10;
    /// What `identity_text` admits for a disk identity or an owner field.
    const IDENTITY_TEXT_LIMIT: usize = 128;
    /// What `validate_array_name` admits.
    const ARRAY_NAME_LIMIT: usize = 64;
    /// What `ElasticCreateSpec::validate` admits, encoded.
    const SPEC_ENCODED_LIMIT: usize = 15 * 1024;
    /// Paths the mover skips because a record stuck on them. This set is what
    /// later runs consult; it never refuses an operation. It is counted in the
    /// plan-time size check, so a full set narrows what one file's record may
    /// cost, never the right to start an operation.
    ///
    /// COST, measured by
    /// `the_plan_time_check_keeps_every_later_write_inside_the_journal_limit`
    /// on a fixture built AT the validator bounds: a full entry — digest,
    /// operation and a maximal temporary name — is about 209 B of JSON, of
    /// which the temporary field alone is the marginal 91 B. A full set of 64
    /// therefore costs about 13 KB of `JOURNAL_LIMIT`, not 7 KB.
    ///
    /// Kept at 64 with full names (owner, 2026-09-12). Re-run that test after
    /// changing this; it prints state, projection, record headroom and margin.
    const STUCK_PATH_LIMIT: usize = 64;
    /// Identities of evicted paths the journal keeps, so a path that stops
    /// being skipped can still be named. The system log keeps every eviction;
    /// this ring is what an operator sees without leaving the UI.
    ///
    /// Raised 4 → 8 (owner, 2026-09-12) to hold a full rotation of the display
    /// history. COST, on the same measurement and with the ring AT this size:
    /// about 209 B per entry, so eight cost about 1.6 KB. Re-run the plan-time
    /// test after changing this.
    const EVICTED_RING: usize = 8;
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Pending {
        Format(ElasticRole),
        Mount(ElasticRole),
        Config,
        Union,
        Sync,
        Maintenance { operation_id: String, kind: ElasticSnapraidKind },
    }

    /// One mover operation in the array journal. Its size does not depend on
    /// the number of files: only the file in flight is recorded, and every
    /// invocation walks the cache again (moved files have left it).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TransferJournal {
        operation_id: String,
        resume_operation_id: String,
        started_at: String,
        /// Set at Complete, or when the declared Resume closes an unfinished
        /// run. An unfinished transfer blocks every other consumer.
        finished_at: Option<String>,
        rules: MoverRules,
        coupled_sync: bool,
        coupled_sync_result: Option<ElasticSnapraidRun>,
        sync_attempt: u64,
        phase: ElasticMoverPhase,
        /// Data branch (`d1`...) chosen on the first walk; every resume keeps it.
        target: Option<String>,
        /// Files handed to the per-file state machine; names the temporaries.
        sequence: u64,
        current: Option<TransferFile>,
        /// Digests this RUN evicted from the skip set. Per run, like every
        /// other counter here, so a clean later run reports nothing.
        #[serde(default, skip_serializing_if = "is_zero")]
        evicted: u64,
        /// Why the in-flight record's last attempt could neither finish nor
        /// withdraw it. Such a record no longer blocks the declared Resume;
        /// it stays as history and a re-run tries it again.
        current_failed: Option<String>,
        moved_files: u64,
        moved_bytes: u64,
        /// Measured by the latest walk; reset when an invocation starts one.
        skipped_files: u64,
        skipped_bytes: u64,
        refused_files: u64,
        issues: Vec<ElasticMoverIssue>,
        walked: bool,
        detail: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PrivateTopology {
        anchor: Option<Anchor>,
        published: bool,
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_service")]
        service: Option<ElasticServiceState>,
    }

    fn deserialize_service<'de, D>(deserializer: D) -> Result<Option<ElasticServiceState>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ServiceVisitor;

        impl<'de> serde::de::Visitor<'de> for ServiceVisitor {
            type Value = Option<ElasticServiceState>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("niepusty rekord service Elastic")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Err(E::custom("service nie może być null"))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                ElasticServiceState::deserialize(deserializer).map(Some)
            }
        }

        deserializer.deserialize_option(ServiceVisitor)
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Journal {
        schema: u8,
        spec: ElasticCreateSpec,
        stage: ElasticStage,
        formatted: Vec<ElasticRole>,
        pending: Option<Pending>,
        boot_id: String,
        sync_completed_at: Option<String>,
        detail: Option<String>,
        last_run: Option<ElasticSnapraidRun>,
        #[serde(skip_serializing_if = "Option::is_none")]
        private: Option<PrivateTopology>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transfer: Option<TransferJournal>,
        /// Bytes the mover put on data branches after the last confirmed Sync:
        /// `Some` means parity is out of date. Only a successful Sync clears it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stale_parity_bytes: Option<u64>,
        /// The mover operation whose coupled Sync ended without success and
        /// left parity stale. Its finished run is the one operation record
        /// that may stand without a pending; a successful Sync clears it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stale_sync_operation: Option<String>,
        /// Full stuck records of closed operations, oldest first, capped at
        /// `STUCK_LIMIT`; the oldest is summarised away when a newer one
        /// arrives. Its path stays in `stuck_paths`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stuck: Vec<ElasticStuckRecord>,
        /// Every path a record stuck on: what a walk skips. FIFO at
        /// `STUCK_PATH_LIMIT` — an insert may evict the oldest digest, so the
        /// newest stuck path is always the one protected.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stuck_paths: Vec<StuckPath>,
        /// How many digests have been evicted over this array's LIFE. Those
        /// paths are no longer skipped. This figure never decays, so it
        /// belongs to the array state, never to one run's result.
        #[serde(default, skip_serializing_if = "is_zero")]
        stuck_evicted: u64,
        /// The identity of the last `EVICTED_RING` evictions. The ring is
        /// bounded; the system log keeps every one.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evicted_paths: Vec<StuckPath>,
    }

    fn is_zero(value: &u64) -> bool {
        *value == 0
    }

    /// One path the mover no longer touches, by digest, and the operation that
    /// left it. Cheap enough that the set outlives the full records.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StuckPath {
        path_sha256: String,
        operation_id: String,
        /// The temporary this record left on the branch. An evicted path can
        /// then still name the copy no later run will refuse.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temporary: Option<String>,
    }

    fn validate_topology(journal: &Journal) -> Result<(), String> {
        // Every writer bounds this text; a journal that carries more than the
        // bound was not written by this helper, and it would travel on into
        // the observation the core reads.
        if journal.detail.as_ref().is_some_and(|detail| detail.len() > TRANSFER_DETAIL_LIMIT) {
            return Err("zbyt długi opis stanu macierzy".into());
        }
        match (journal.schema, &journal.private) {
            (1, None)
                if journal.spec.cache.is_none()
                    && journal.transfer.is_none()
                    && journal.stuck.is_empty()
                    && journal.stuck_paths.is_empty()
                    && journal.evicted_paths.is_empty()
                    && journal.stuck_evicted == 0
                    && journal.stale_sync_operation.is_none() =>
            {
                Ok(())
            }
            (1, None) => Err("schema 1 nie obsługuje cache Elastic".into()),
            (2, Some(private)) => {
                if private.published && private.anchor.is_none() {
                    return Err("publikacja bez kotwicy".into());
                }
                if let Some(anchor) = &private.anchor {
                    if anchor.boot_id != journal.boot_id
                        || anchor.pid <= 1
                        || anchor.start_ticks == 0
                        || anchor.mount_ns_inode == 0
                        || anchor.exe_inode == 0
                        || anchor.union_device == 0
                        || anchor.union_source.is_empty()
                        // Bounded as the journal stores it: escaped, a byte can
                        // cost two.
                        || json_size(&anchor.union_source) > ANCHOR_SOURCE_BYTES
                        || anchor.union_source.chars().any(char::is_control)
                        || anchor.exe_sha256.len() != 64
                        || !anchor
                            .exe_sha256
                            .bytes()
                            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                        || anchor.exe_sha256.bytes().all(|c| c == b'0')
                    {
                        return Err("nieprawidłowa tożsamość kotwicy".into());
                    }
                }
                if let Some(service) = &private.service {
                    validate_elastic_uuid(&service.operation_id).map_err(|e| e.to_string())?;
                    if service.operation_id == journal.spec.operation_id {
                        return Err("service używa identyfikatora Create".into());
                    }
                }
                if journal.stage == ElasticStage::Ready
                    && (!private.published || private.anchor.is_none()
                        || private.service.as_ref().is_some_and(|service| {
                            service.mode == ElasticServiceMode::Hold || service.pending
                        }))
                {
                    return Err("Ready bez potwierdzenia publikacji".into());
                }
                if let Some(transfer) = &journal.transfer {
                    validate_elastic_uuid(&transfer.operation_id).map_err(|e| e.to_string())?;
                    validate_elastic_uuid(&transfer.resume_operation_id).map_err(|e| e.to_string())?;
                    transfer.rules.validate().map_err(|e| e.to_string())?;
                    let temporary_prefix = format!(".tentanas-transfer-{}-", transfer.operation_id);
                    if transfer.operation_id == journal.spec.operation_id
                        || transfer.resume_operation_id == journal.spec.operation_id
                        || transfer.operation_id == transfer.resume_operation_id
                        || transfer.target.as_ref().is_some_and(|disk| {
                            !(1..=journal.spec.data.len()).any(|index| disk == &format!("d{index}"))
                        })
                        || (transfer.current.is_some() && transfer.target.is_none())
                        || transfer
                            .current
                            .as_ref()
                            .is_some_and(|file| !valid_transfer_record(file, &temporary_prefix))
                        || transfer.moved_files > transfer.sequence
                        || transfer.issues.len() > TRANSFER_ISSUE_LIMIT
                        || transfer.issues.iter().any(|issue| {
                            issue.path.len() > TRANSFER_TEXT_LIMIT || issue.reason.len() > TRANSFER_TEXT_LIMIT
                        })
                        || transfer.detail.as_ref().is_some_and(|detail| detail.len() > TRANSFER_DETAIL_LIMIT)
                        || transfer.current_failed.as_ref().is_some_and(|reason| reason.len() > TRANSFER_DETAIL_LIMIT)
                        || (transfer.current_failed.is_some() && transfer.current.is_none())
                    {
                        return Err("nieprawidłowy trwały dziennik transferu".into());
                    }
                    if (transfer.phase == ElasticMoverPhase::Complete && transfer.current.is_some())
                        || (transfer.finished_at.is_some()
                        && ((transfer.current.is_some() && transfer.current_failed.is_none())
                            || !matches!(
                                transfer.phase,
                                ElasticMoverPhase::Complete | ElasticMoverPhase::NeedsAttention
                            )))
                        || (transfer.phase == ElasticMoverPhase::Complete
                            && (transfer.finished_at.is_none() || transfer_sync_owed(journal, transfer)))
                    {
                        return Err("zakończony transfer bez spójnego stanu".into());
                    }
                }
                if journal
                    .stale_sync_operation
                    .as_ref()
                    .is_some_and(|operation| validate_elastic_uuid(operation).is_err())
                    || (journal.stale_sync_operation.is_some() && journal.stale_parity_bytes.is_none())
                {
                    return Err("nieprawidłowy znacznik nieudanego Sync movera".into());
                }
                if journal.stuck.len() > STUCK_LIMIT
                    || !journal.stuck.iter().all(|record| valid_stuck_record(&journal.spec, record))
                {
                    return Err("nieprawidłowa historia utkniętych rekordów".into());
                }
                let digest = |value: &str| {
                    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                };
                let valid_path = |path: &StuckPath| {
                    digest(&path.path_sha256)
                        && validate_elastic_uuid(&path.operation_id).is_ok()
                        && path.operation_id != journal.spec.operation_id
                        && path.temporary.as_deref().is_none_or(|name| {
                            name.strip_prefix(&format!(".tentanas-transfer-{}-", path.operation_id))
                                .is_some_and(|sequence| {
                                    !sequence.is_empty()
                                        && sequence.len() <= TEMPORARY_SEQUENCE_DIGITS
                                        && sequence.bytes().all(|b| b.is_ascii_digit())
                                })
                        })
                };
                if journal.evicted_paths.len() > EVICTED_RING
                    || !journal.evicted_paths.iter().all(valid_path)
                    || journal.evicted_paths.len() as u64 > journal.stuck_evicted
                {
                    return Err("nieprawidłowy pierścień wypartych ścieżek".into());
                }
                if journal.stuck_paths.len() > STUCK_PATH_LIMIT
                    || !journal.stuck_paths.iter().enumerate().all(|(index, path)| {
                        valid_path(path)
                            && !journal.stuck_paths[..index]
                                .iter()
                                .any(|earlier| earlier.path_sha256 == path.path_sha256)
                    })
                    // Every full record is one of the skipped paths, unless
                    // the skip set was already full when it was kept.
                    || !(journal.stuck_paths.len() >= STUCK_PATH_LIMIT
                        || journal.stuck.iter().all(|record| {
                            journal
                                .stuck_paths
                                .iter()
                                .any(|path| path.path_sha256 == record.path_sha256)
                        }))
                {
                    return Err("nieprawidłowa lista pominiętych ścieżek".into());
                }
                Ok(())
            }
            _ => Err("niezgodna schema i topologia journala".into()),
        }
    }

    fn valid_stuck_record(spec: &ElasticCreateSpec, record: &ElasticStuckRecord) -> bool {
        let hex = |value: &str| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        let text = |value: &str| {
            !value.is_empty() && value.len() <= TRANSFER_TEXT_LIMIT && !value.chars().any(char::is_control)
        };
        let pin = |pin: &ElasticFilePin| pin.sha256.as_deref().is_none_or(hex);
        let prefix = format!(".tentanas-transfer-{}-", record.operation_id);
        validate_elastic_uuid(&record.operation_id).is_ok()
            && record.operation_id != spec.operation_id
            && text(&record.path)
            && hex(&record.path_sha256)
            && text(&record.reason)
            && record
                .target
                .as_ref()
                .is_none_or(|disk| (1..=spec.data.len()).any(|index| disk == &format!("d{index}")))
            && record.temporary.as_deref().is_none_or(|name| {
                name.strip_prefix(&prefix).is_some_and(|sequence| {
                    !sequence.is_empty()
                        && sequence.len() <= TEMPORARY_SEQUENCE_DIGITS
                        && sequence.bytes().all(|b| b.is_ascii_digit())
                })
            })
            && pin(&record.source)
            && record.temporary_copy.as_ref().is_none_or(pin)
            && record.destination.as_ref().is_none_or(pin)
    }

    /// The operation record and the pending that must go with it. `load` and
    /// `save` run the same check, so no state is written that could not be
    /// read back.
    fn validate_operation_record(journal: &Journal) -> Result<(), String> {
        if let Some(run) = &journal.last_run {
            validate_elastic_uuid(&run.operation_id).map_err(|e| e.to_string())?;
            if run.operation_id == journal.spec.operation_id || run.outcome == ElasticSnapraidOutcome::Refused {
                return Err("obcy rekord operacji".into());
            }
            let pending = Some(Pending::Maintenance { operation_id: run.operation_id.clone(), kind: run.kind });
            // Only the mover's own coupled Sync that ended without success
            // pins no pending: it left parity marked stale instead, so the
            // array may mount (and remount after a boot) around it.
            let stale_mover_sync = run.kind == ElasticSnapraidKind::Sync
                && run.finished_at.is_some()
                && journal.stale_parity_bytes.is_some()
                && journal.stale_sync_operation.as_deref() == Some(run.operation_id.as_str());
            if (run.outcome == ElasticSnapraidOutcome::Running && (run.finished_at.is_some() || journal.pending != pending))
                || (matches!(run.outcome, ElasticSnapraidOutcome::Failed | ElasticSnapraidOutcome::NeedsAttention)
                    && journal.pending != pending
                    && !stale_mover_sync)
                || (run.outcome == ElasticSnapraidOutcome::Succeeded && (run.finished_at.is_none() || journal.pending == pending)) {
                return Err("niespójny wynik operacji i pending".into());
            }
        }
        if let Some(Pending::Maintenance { operation_id, kind }) = &journal.pending {
            if !journal.last_run.as_ref().is_some_and(|run| &run.operation_id == operation_id && &run.kind == kind) {
                return Err("pending bez zgodnego rekordu operacji".into());
            }
        }
        Ok(())
    }

    fn decode_journal(bytes: &[u8]) -> Result<Journal, String> {
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| format!("journal: {e}"))?;
        let object = raw.as_object().ok_or("journal nie jest obiektem")?;
        match object.get("schema").and_then(serde_json::Value::as_u64) {
            Some(1) if !object.contains_key("private") => (),
            Some(2)
                if object
                    .get("private")
                    .and_then(serde_json::Value::as_object)
                    .is_some_and(|private| {
                        private.contains_key("anchor") && private.contains_key("published")
                    }) =>
            {}
            _ => return Err("niezgodna schema i obecność private".into()),
        }
        let journal: Journal = serde_json::from_slice(bytes).map_err(|e| format!("journal: {e}"))?;
        validate_topology(&journal)?;
        Ok(journal)
    }

    struct Root {
        path: PathBuf,
        uid: u32,
        node_lock: File,
    }

    fn private_file(path: &Path, uid: u32) -> Result<File, String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| format!("odczyt {}: {e}", path.display()))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(format!("obcy plik lub uprawnienia {}", path.display()));
        }
        Ok(file)
    }

    fn directory(path: &Path, private: bool, uid: u32) -> Result<(), String> {
        let mut current = PathBuf::from("/");
        for component in path.components().skip(1) {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err("nieprawidłowa ścieżka katalogu".into());
            }
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&current)
                        .map_err(|e| format!("katalog {}: {e}", current.display()))?;
                    File::open(current.parent().ok_or("brak rodzica")?)
                        .and_then(|f| f.sync_all())
                        .map_err(|e| e.to_string())?;
                }
                Err(e) => return Err(e.to_string()),
            }
            let metadata = std::fs::symlink_metadata(&current).map_err(|e| e.to_string())?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || (metadata.uid() != 0 && metadata.uid() != uid)
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & libc::S_ISVTX == 0)
                || (private
                    && current == path
                    && (metadata.uid() != uid || metadata.mode() & 0o777 != 0o700))
            {
                return Err(format!("niebezpieczny katalog {}", current.display()));
            }
        }
        Ok(())
    }

    fn lock(path: &Path, uid: u32) -> Result<File, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err("obcy plik blokady Elastic".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!("Elastic busy: {}", std::io::Error::last_os_error()));
        }
        Ok(file)
    }

    impl Root {
        fn open(path: &Path, uid: u32) -> Result<Self, String> {
            directory(path, true, uid)?;
            let node_lock = lock(&path.join(".storage.lock"), uid)?;
            // The node lock is held: nothing else writes here, so every
            // journal temporary in this private directory is a leftover of a
            // write that never finished. Only those are removed.
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    if entry.file_name().to_str().is_some_and(own_temporary) {
                        let _ = remove_stale_temporary(&entry.path(), uid);
                    }
                }
            }
            Ok(Self {
                path: path.to_path_buf(),
                uid,
                node_lock,
            })
        }

        fn array_lock(&self, id: &str) -> Result<File, String> {
            validate_elastic_uuid(id).map_err(|e| e.to_string())?;
            lock(&self.path.join(format!("{id}.lock")), self.uid)
        }

        fn load(&self, id: &str) -> Result<Journal, String> {
            validate_elastic_uuid(id).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            private_file(&self.path.join(format!("{id}.json")), self.uid)?
                .take(JOURNAL_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 > JOURNAL_LIMIT {
                return Err("journal zbyt duży".into());
            }
            let journal = decode_journal(&bytes)?;
            journal.spec.validate().map_err(|e| e.to_string())?;
            validate_elastic_uuid(&journal.boot_id).map_err(|e| e.to_string())?;
            if journal.spec.array_id != id
                || journal.formatted.iter().enumerate().any(|(i, role)| {
                    role_disk(&journal.spec, *role).is_err()
                        || journal.formatted[..i].contains(role)
                })
            {
                return Err("niespójny journal Elastic".into());
            }
            validate_operation_record(&journal)?;
            Ok(journal)
        }

        fn journals(&self) -> Result<Vec<Journal>, String> {
            let mut result = Vec::new();
            for entry in std::fs::read_dir(&self.path).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or("nazwa journala")?;
                    result.push(self.load(id)?);
                }
            }
            Ok(result)
        }

        fn save(&self, journal: &Journal) -> Result<(), String> {
            journal.spec.validate().map_err(|e| e.to_string())?;
            validate_topology(journal)?;
            validate_operation_record(journal)?;
            let target = self.path.join(format!("{}.json", journal.spec.array_id));
            if target.try_exists().map_err(|e| e.to_string())? {
                let old = self.load(&journal.spec.array_id)?;
                if old.spec != journal.spec {
                    return Err("zmiana trwałej specyfikacji Elastic".into());
                }
                if old.schema != journal.schema || old.private.is_some() != journal.private.is_some() {
                    return Err("zmiana trwałej topologii Elastic".into());
                }
                if old
                    .private
                    .as_ref()
                    .and_then(|private| private.service.as_ref())
                    .is_some()
                    && journal
                        .private
                        .as_ref()
                        .and_then(|private| private.service.as_ref())
                        .is_none()
                {
                    return Err("usunięcie stanu service Elastic".into());
                }
                transfer_transition(&old, journal)?;
            }
            let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
            // Never persist a state `load` would refuse to read back.
            if bytes.len() as u64 > JOURNAL_LIMIT {
                return Err("journal przekroczyłby limit odczytu".into());
            }
            atomic_write(&target, &bytes, self.uid)
        }

        fn reserve(
            &self,
            spec: &ElasticCreateSpec,
            boot_id: String,
            guard: impl FnOnce() -> Result<(), String>,
        ) -> Result<Journal, String> {
            spec.validate().map_err(|e| e.to_string())?;
            let journals = self.journals()?;
            if journals.iter().any(|j| j.spec.array_id == spec.array_id) {
                return Err("macierz ma już journal; create nie jest ponawiane".into());
            }
            claims_guard(&journals, spec)?;
            guard()?;
            let journal = Journal {
                schema: 2,
                spec: spec.clone(),
                stage: ElasticStage::Prepared,
                formatted: Vec::new(),
                pending: None,
                boot_id,
                sync_completed_at: None,
                detail: None,
                last_run: None,
                private: Some(PrivateTopology { anchor: None, published: false, service: None }),
                transfer: None,
                stale_parity_bytes: None,
                stale_sync_operation: None,
                stuck: Vec::new(),
                stuck_paths: Vec::new(),
                stuck_evicted: 0,
                evicted_paths: Vec::new(),
            };
            self.save(&journal)?;
            Ok(journal)
        }
    }

    /// Journal writes land through a private temporary in the same private
    /// directory, named after this process and the write.
    const TEMP_PREFIX: &str = ".elastic-";
    const TEMP_SUFFIX: &str = ".new";

    /// Whether a name is one of our own journal temporaries.
    fn own_temporary(name: &str) -> bool {
        name.strip_prefix(TEMP_PREFIX)
            .and_then(|rest| rest.strip_suffix(TEMP_SUFFIX))
            .is_some_and(|middle| {
                let mut parts = middle.split('-');
                let attributed = |part: Option<&str>| {
                    part.is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                };
                attributed(parts.next()) && attributed(parts.next()) && parts.next().is_none()
            })
    }

    /// Removes a leftover journal temporary this executor can attribute to
    /// itself: our name pattern, in our own private directory, a plain 0600
    /// file of ours with one link. Nothing else is ever removed, and the
    /// exclusive node lock means no other executor can be writing one.
    fn remove_stale_temporary(path: &Path, uid: u32) -> Result<(), String> {
        let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(format!("pozostałość zapisu nie należy do wykonawcy: {}", path.display()));
        }
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }

    fn atomic_write(target: &Path, bytes: &[u8], uid: u32) -> Result<(), String> {
        let parent = target.parent().ok_or("brak katalogu zapisu")?;
        directory(parent, false, uid)?;
        match std::fs::symlink_metadata(target) {
            Ok(_) => {
                private_file(target, uid)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
        let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            "{TEMP_PREFIX}{}-{sequence}{TEMP_SUFFIX}",
            std::process::id()
        ));
        let create = || {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&candidate)
        };
        let mut file = match create() {
            Ok(file) => file,
            // A hard reset can leave a temporary behind and a later boot can
            // reuse its pid. Nothing live owns that name: this process is the
            // only one holding this pid, this write is the only user of this
            // sequence, and the node lock excludes every other executor.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                remove_stale_temporary(&candidate, uid)?;
                create().map_err(|e| e.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        };
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&candidate, target).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    }

    fn role_disk(spec: &ElasticCreateSpec, role: ElasticRole) -> Result<&ElasticDiskSpec, String> {
        match role {
            ElasticRole::Data(i) => spec.data.get(usize::from(i).wrapping_sub(1)),
            ElasticRole::Cache => spec.cache.as_ref(),
            ElasticRole::Parity(i) => spec.parity.get(usize::from(i).wrapping_sub(1)),
        }
        .ok_or_else(|| "nieznana rola journala".into())
    }

    fn roles(spec: &ElasticCreateSpec) -> Vec<ElasticRole> {
        (1..=spec.data.len())
            .map(|i| ElasticRole::Data(i as u16))
            .chain(spec.cache.as_ref().map(|_| ElasticRole::Cache))
            .chain((1..=spec.parity.len()).map(|i| ElasticRole::Parity(i as u8)))
            .collect()
    }

    fn mount_path(spec: &ElasticCreateSpec, role: ElasticRole) -> String {
        match role {
            ElasticRole::Data(i) => data_branch_path(&spec.name, &format!("d{i}")),
            ElasticRole::Cache => cache_branch_path(&spec.name, "c1"),
            ElasticRole::Parity(i) => parity_mount_path(&spec.name, i),
        }
    }

    fn boot_id() -> Result<String, String> {
        let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|e| e.to_string())?;
        let value = value.trim().to_string();
        validate_elastic_uuid(&value).map_err(|e| e.to_string())?;
        Ok(value)
    }

    fn tool(candidates: &[&str]) -> Result<PathBuf, String> {
        candidates
            .iter()
            .map(Path::new)
            .find(|p| p.is_file())
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("brak narzędzia {}", candidates[0]))
    }

    fn process_command(
        program: &Path,
        args: &[String],
        with_stdin: bool,
        locks: &[RawFd],
    ) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .stdin(if with_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let inherited = locks.to_vec();
        unsafe {
            command.pre_exec(move || {
                for fd in &inherited {
                    if libc::fcntl(*fd, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        command
    }

    fn run(
        program: &Path,
        args: &[String],
        payload: Option<&[u8]>,
        locks: &[RawFd],
    ) -> Result<Output, String> {
        let mut child = process_command(program, args, payload.is_some(), locks)
            .spawn()
            .map_err(|e| format!("{}: {e}", program.display()))?;
        std::thread::scope(|scope| {
            let writer = payload.map(|payload| {
                let stdin = child.stdin.take();
                scope.spawn(move || {
                    stdin
                        .ok_or_else(|| "brak stdin dziecka".to_string())?
                        .write_all(payload)
                        .map_err(|e| format!("stdin: {e}"))
                })
            });
            let output = child.wait_with_output().map_err(|e| e.to_string());
            if let Some(writer) = writer {
                writer.join().map_err(|_| "błąd wątku stdin")??;
            }
            output
        })
    }

    fn success(output: Output) -> Result<String, String> {
        if !output.status.success() {
            return Err(format!(
                "narzędzie: {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    }

    #[derive(Clone, Debug)]
    struct Device {
        path: String,
        kernel: String,
        bytes: u64,
        wwn: Option<String>,
        serial: Option<String>,
        major_minor: String,
        occupied: bool,
    }

    fn field(node: &serde_json::Value, key: &str) -> Option<String> {
        node.get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .map(|s| s.trim().to_string())
    }

    fn inventory() -> Result<Vec<Device>, String> {
        let output = run(
            &tool(&["/usr/bin/lsblk", "/bin/lsblk"])?,
            &[
                "--json".into(),
                "--bytes".into(),
                "--paths".into(),
                "--output".into(),
                "NAME,KNAME,PATH,TYPE,SIZE,WWN,SERIAL,MAJ:MIN,RO,MOUNTPOINTS".into(),
            ],
            None,
            &[],
        )?;
        let value: serde_json::Value =
            serde_json::from_str(&success(output)?).map_err(|e| e.to_string())?;
        let nodes = value
            .get("blockdevices")
            .and_then(|v| v.as_array())
            .ok_or("brak inventory")?;
        let mut result = Vec::new();
        for node in nodes {
            if field(node, "type").as_deref() != Some("disk") {
                continue;
            }
            let path = field(node, "path").ok_or("brak device path")?;
            validate_branch_device(&path).map_err(|e| e.to_string())?;
            let kernel = Path::new(&path)
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or("brak nazwy urządzenia")?
                .to_string();
            let bytes = node
                .get("size")
                .and_then(|v| v.as_u64())
                .ok_or("brak wielkości urządzenia")?;
            let major_minor = field(node, "maj:min").ok_or("brak maj:min")?;
            let mounts = node
                .get("mountpoints")
                .and_then(|v| v.as_array())
                .ok_or("brak listy mountpoints")?;
            let ro = node
                .get("ro")
                .and_then(|v| v.as_bool())
                .ok_or("brak flagi ro")?;
            let occupied = ro
                || mounts.iter().any(|v| !v.is_null())
                || node
                    .get("children")
                    .and_then(|v| v.as_array())
                    .is_some_and(|v| !v.is_empty());
            result.push(Device {
                path,
                kernel,
                bytes,
                major_minor,
                occupied,
                wwn: field(node, "wwn"),
                serial: field(node, "serial"),
            });
        }
        Ok(result)
    }

    fn identity_device<'a>(
        disk: &ElasticDiskSpec,
        devices: &'a [Device],
    ) -> Result<&'a Device, String> {
        let matches: Vec<_> = devices
            .iter()
            .filter(|d| {
                disk.wwn.as_ref().is_some_and(|v| d.wwn.as_ref() == Some(v))
                    || disk
                        .serial
                        .as_ref()
                        .is_some_and(|v| d.serial.as_ref() == Some(v))
            })
            .collect();
        if matches.len() != 1 {
            return Err("urządzenie nieobecne lub tożsamość niejednoznaczna".into());
        }
        let d = matches[0];
        if d.bytes != disk.bytes
            || disk.wwn.as_ref().is_some_and(|v| d.wwn.as_ref() != Some(v))
            || disk
                .serial
                .as_ref()
                .is_some_and(|v| d.serial.as_ref() != Some(v))
        {
            return Err("zmieniona tożsamość lub wielkość urządzenia".into());
        }
        Ok(d)
    }

    fn resolve<'a>(disk: &ElasticDiskSpec, devices: &'a [Device]) -> Result<&'a Device, String> {
        let d = identity_device(disk, devices)?;
        let metadata = std::fs::metadata(&d.path).map_err(|e| e.to_string())?;
        let actual = format!(
            "{}:{}",
            libc::major(metadata.rdev()),
            libc::minor(metadata.rdev())
        );
        if !metadata.file_type().is_block_device() || actual != d.major_minor {
            return Err("urządzenie zmieniło się podczas odczytu".into());
        }
        Ok(d)
    }

    fn probe_fs(device: &Device) -> Result<BTreeMap<String, String>, String> {
        let output = run(
            &tool(&["/usr/sbin/blkid", "/sbin/blkid", "/usr/bin/blkid"])?,
            &[
                "-p".into(),
                "-o".into(),
                "export".into(),
                device.path.clone(),
            ],
            None,
            &[],
        )?;
        parse_blkid(output)
    }

    fn parse_blkid(output: Output) -> Result<BTreeMap<String, String>, String> {
        if output.status.code() == Some(2) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(BTreeMap::new());
        }
        if !output.stderr.is_empty() {
            return Err("diagnostyka odczytu blkid".into());
        }
        let text = success(output)?;
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            let (key, value) = line.split_once('=').ok_or("nieczytelny blkid")?;
            if key.is_empty()
                || value.is_empty()
                || fields.insert(key.into(), value.into()).is_some()
            {
                return Err("niejednoznaczny blkid".into());
            }
        }
        if fields.is_empty() {
            return Err("pusty wynik udanego blkid".into());
        }
        Ok(fields)
    }

    fn parse_blank_wipefs(output: Output) -> Result<(), String> {
        if !output.stderr.is_empty() {
            return Err("diagnostyka odczytu wipefs".into());
        }
        let value: serde_json::Value = serde_json::from_str(&success(output)?)
            .map_err(|e| format!("nieczytelny wipefs: {e}"))?;
        match value.get("signatures").and_then(|v| v.as_array()) {
            Some(signatures) if signatures.is_empty() => Ok(()),
            _ => Err("wipefs nie potwierdził pustego nośnika".into()),
        }
    }

    fn clean_device(device: &Device) -> Result<(), String> {
        if device.occupied {
            return Err("urządzenie ma partycje, mount lub jest tylko do odczytu".into());
        }
        if std::fs::read_dir(format!("/sys/class/block/{}/holders", device.kernel))
            .map_err(|e| e.to_string())?
            .next()
            .transpose()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("urządzenie ma aktywnych właścicieli".into());
        }
        if !probe_fs(device)?.is_empty() {
            return Err("urządzenie zawiera podpis danych".into());
        }
        parse_blank_wipefs(run(
            &tool(&["/usr/sbin/wipefs", "/sbin/wipefs", "/usr/bin/wipefs"])?,
            &["--no-act".into(), "--json".into(), device.path.clone()],
            None,
            &[],
        )?)?;
        let swap = std::fs::read_to_string("/proc/swaps").map_err(|e| e.to_string())?;
        for line in swap.lines().skip(1) {
            let source = line.split_whitespace().next().ok_or("nieczytelny swap")?;
            let metadata = std::fs::metadata(source).map_err(|e| e.to_string())?;
            if metadata.file_type().is_block_device()
                && format!(
                    "{}:{}",
                    libc::major(metadata.rdev()),
                    libc::minor(metadata.rdev())
                ) == device.major_minor
            {
                return Err("urządzenie jest swapem".into());
            }
        }
        Ok(())
    }

    fn overlaps(a: &Path, b: &Path) -> bool {
        a.starts_with(b) || b.starts_with(a)
    }

    #[derive(Debug)]
    pub(crate) struct MountRow {
        pub(crate) id: u64,
        pub(crate) path: String,
        pub(crate) major_minor: String,
        pub(crate) root: String,
        pub(crate) filesystem: String,
        pub(crate) source: String,
        pub(crate) mount_options: Vec<String>,
        pub(crate) super_options: Vec<String>,
    }

    pub(crate) fn mount_rows() -> Result<Vec<MountRow>, String> {
        let text = std::fs::read_to_string("/proc/self/mountinfo").map_err(|e| e.to_string())?;
        text.lines()
            .map(|line| {
                let (left, right) = line.split_once(" - ").ok_or("nieczytelny mountinfo")?;
                let left: Vec<_> = left.split_whitespace().collect();
                let right: Vec<_> = right.split_whitespace().collect();
                if left.len() < 6 || right.len() < 3 {
                    return Err("niepełny mountinfo".into());
                }
                let path = left[4]
                    .replace("\\040", " ")
                    .replace("\\011", "\t")
                    .replace("\\012", "\n")
                    .replace("\\134", "\\");
                Ok(MountRow {
                    id: left[0].parse().map_err(|_| "nieprawidłowy mount id")?,
                    path,
                    major_minor: left[2].into(),
                    root: left[3]
                        .replace("\\040", " ")
                        .replace("\\011", "\t")
                        .replace("\\012", "\n")
                        .replace("\\134", "\\"),
                    filesystem: right[0].into(),
                    source: right[1]
                        .replace("\\040", " ")
                        .replace("\\011", "\t")
                        .replace("\\012", "\n")
                        .replace("\\134", "\\"),
                    mount_options: left[5].split(',').map(str::to_owned).collect(),
                    super_options: right[2].split(',').map(str::to_owned).collect(),
                })
            })
            .collect()
    }

    fn zfs_namespace_clear(name: &str) -> Result<bool, String> {
        let mounts = mount_rows()?;
        let targets = [union_path(name), branch_root(name)];
        if mounts.iter().any(|m| {
            m.filesystem == "zfs"
                && targets
                    .iter()
                    .any(|p| overlaps(Path::new(&m.path), Path::new(p)))
        }) {
            return Ok(false);
        }
        match tool(&["/usr/sbin/zpool", "/sbin/zpool", "/usr/bin/zpool"]) {
            Ok(zpool) => {
                let names = success(run(
                    &zpool,
                    &["list".into(), "-H".into(), "-o".into(), "name".into()],
                    None,
                    &[],
                )?)?;
                if names.lines().any(|n| n == name) {
                    return Ok(false);
                }
                let zfs = tool(&["/usr/sbin/zfs", "/sbin/zfs", "/usr/bin/zfs"])?;
                let output = success(run(
                    &zfs,
                    &[
                        "list".into(),
                        "-H".into(),
                        "-o".into(),
                        "mountpoint".into(),
                        "-t".into(),
                        "filesystem".into(),
                    ],
                    None,
                    &[],
                )?)?;
                Ok(!output
                    .lines()
                    .filter(|p| p.starts_with('/'))
                    .any(|p| targets.iter().any(|t| overlaps(Path::new(p), Path::new(t)))))
            }
            Err(_) => {
                let modules =
                    std::fs::read_to_string("/proc/modules").map_err(|e| e.to_string())?;
                if modules
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some("zfs"))
                    || Path::new("/sys/module/zfs")
                        .try_exists()
                        .map_err(|e| e.to_string())?
                    || mounts.iter().any(|m| m.filesystem == "zfs")
                {
                    return Err("brak narzędzia i niepotwierdzony stan ZFS".into());
                }
                Ok(true)
            }
        }
    }

    fn vacant_namespace(spec: &ElasticCreateSpec, worker: Option<&Worker>) -> Result<(), String> {
        if !zfs_namespace_clear(&spec.name)? {
            return Err("zajęta przestrzeń ZFS".into());
        }
        for path in [
            union_path(&spec.name),
            branch_root(&spec.name),
            config_path(&spec.name),
            format!("{CONFIG_DIR}{}-{CONTENT_FILE}", spec.name),
        ] {
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.to_string()),
                Ok(_) => return Err("docelowa ścieżka już istnieje".into()),
            }
        }
        let mounts = mount_rows()?;
        if mounts.iter().any(|m| {
            (m.path != "/" && m.path != "/mnt")
                && !(worker.is_some() && m.path == BRANCH_ROOT.trim_end_matches('/') && m.filesystem == "tmpfs")
                && [union_path(&spec.name), branch_root(&spec.name)]
                    .iter()
                    .any(|p| overlaps(Path::new(p), Path::new(&m.path)))
        }) {
            return Err("obcy mount zasłania przestrzeń macierzy".into());
        }
        Ok(())
    }

    fn layout(spec: &ElasticCreateSpec, devices: &[Device]) -> Result<ElasticSpec, String> {
        layout_with(spec, |disk| resolve(disk, devices).map(|device| device.path.clone()))
    }

    /// The array's layout with every disk resolved to its device path by `path_of`.
    fn layout_with(
        spec: &ElasticCreateSpec,
        mut path_of: impl FnMut(&ElasticDiskSpec) -> Result<String, String>,
    ) -> Result<ElasticSpec, String> {
        let mut value = ElasticSpec {
            name: spec.name.clone(),
            filesystem: spec.filesystem.as_str().into(),
            data: Vec::new(),
            cache: Vec::new(),
            parity: Vec::new(),
            mergerfs: MergerfsOptions::default(),
            snapraid: SnapraidOptions::default(),
        };
        for (i, disk) in spec.data.iter().enumerate() {
            value.data.push(Branch {
                disk: format!("d{}", i + 1),
                device: path_of(disk)?,
            });
        }
        if let Some(disk) = &spec.cache {
            value.cache.push(Branch {
                disk: "c1".into(),
                device: path_of(disk)?,
            });
        }
        for (i, disk) in spec.parity.iter().enumerate() {
            value.parity.push(ParityDisk {
                index: (i + 1) as u8,
                disk: format!("p{}", i + 1),
                device: path_of(disk)?,
            });
        }
        validate_spec(&value).map_err(|e| e.to_string())?;
        Ok(value)
    }

    fn filesystem_matches(
        spec: &ElasticCreateSpec,
        role: ElasticRole,
        device: &Device,
    ) -> Result<(), String> {
        let fields = probe_fs(device)?;
        if fields.get("UUID") != Some(&role_disk(spec, role)?.expected_uuid)
            || fields.get("TYPE").map(String::as_str) != Some(spec.filesystem.as_str())
        {
            return Err("UUID lub typ FS niezgodny z journalem".into());
        }
        Ok(())
    }

    fn branch_mounted(
        spec: &ElasticCreateSpec,
        role: ElasticRole,
        device: &Device,
    ) -> Result<bool, String> {
        let path = mount_path(spec, role);
        let rows = mount_rows()?;
        let found: Vec<_> = rows.iter().filter(|m| m.path == path).collect();
        if found.is_empty() {
            if rows.iter().any(|m| m.major_minor == device.major_minor) {
                return Err("nośnik zamontowany poza oczekiwaną ścieżką".into());
            }
            return Ok(false);
        }
        if found.len() != 1
            || found[0].major_minor != device.major_minor
            || found[0].filesystem != spec.filesystem.as_str()
        {
            return Err("obcy mount w ścieżce brancha".into());
        }
        Ok(true)
    }

    fn union_mounted(spec: &ElasticSpec) -> Result<bool, String> {
        let rows = mount_rows()?;
        let found: Vec<_> = rows
            .iter()
            .filter(|m| m.path == spec.union_path())
            .collect();
        if found.is_empty() {
            return Ok(false);
        }
        if found.len() != 1 || found[0].filesystem != "fuse.mergerfs" {
            return Err("obcy mount w ścieżce unii".into());
        }
        let values: BTreeMap<String, String> = [
            "branches",
            "category.create",
            "cache.files",
            "minfreespace",
            "moveonenospc",
        ]
        .into_iter()
        .map(|key| read_union_option(spec, key).map(|value| (key.into(), value)))
        .collect::<Result<_, _>>()?;
        validate_union_options(spec, &values)?;
        Ok(true)
    }

    fn read_union_option(spec: &ElasticSpec, option: &str) -> Result<String, String> {
        let path =
            CString::new(format!("{}/.mergerfs", spec.union_path())).map_err(|e| e.to_string())?;
        let key = CString::new(format!("user.mergerfs.{option}")).map_err(|e| e.to_string())?;
        let mut bytes = vec![0u8; 16384];
        let size = unsafe {
            libc::getxattr(
                path.as_ptr(),
                key.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if size < 0 {
            return Err(format!(
                "odczyt branchy: {}",
                std::io::Error::last_os_error()
            ));
        }
        bytes.truncate(size as usize);
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }

    /// mergerfs `minfreespace` in bytes: the floor every data branch keeps.
    fn min_free_space_bytes(spec: &ElasticSpec) -> Result<u64, String> {
        let size = &spec.mergerfs.min_free_space;
        let digits = size.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let multiplier = match &size[digits.len()..] {
            "" => 1u64,
            "K" => 1 << 10,
            "M" => 1 << 20,
            "G" => 1 << 30,
            "T" => 1 << 40,
            _ => return Err("nieobsługiwany runtime minfreespace".into()),
        };
        digits
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(multiplier))
            .ok_or_else(|| "niepoprawny minfreespace".into())
    }

    fn validate_union_options(
        spec: &ElasticSpec,
        values: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let bytes = min_free_space_bytes(spec)?;
        for (key, expected) in [
            ("branches", spec.branch_specs().join(":")),
            ("category.create", spec.mergerfs.create_policy.clone()),
            ("cache.files", spec.mergerfs.cache_files.clone()),
            ("minfreespace", bytes.to_string()),
            (
                "moveonenospc",
                if spec.mergerfs.move_on_enospc {
                    "mfs"
                } else {
                    "false"
                }
                .to_string(),
            ),
        ] {
            if values.get(key) != Some(&expected) {
                return Err(format!("inna opcja unii: {key}"));
            }
        }
        Ok(())
    }

    fn capacity(path: &str) -> Result<(u64, u64, u64), String> {
        let path = CString::new(path).map_err(|e| e.to_string())?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let stat = unsafe { stat.assume_init() };
        let unit = stat.f_frsize as u64;
        Ok((
            (stat.f_blocks as u64).saturating_mul(unit),
            (stat.f_blocks as u64)
                .saturating_sub(stat.f_bfree as u64)
                .saturating_mul(unit),
            (stat.f_bavail as u64).saturating_mul(unit),
        ))
    }

    fn observe(journal: &Journal, worker: Option<&Worker>) -> ElasticResult {
        let devices = if journal.private.is_some() && worker.is_none() {
            Err("brak zweryfikowanej prywatnej przestrzeni montowań".into())
        } else {
            inventory()
        };
        let mut disks = Vec::new();
        for role in roles(&journal.spec) {
            let mut row = ElasticDiskObservation {
                role,
                kernel_name: None,
                device: None,
                observed_uuid: None,
                filesystem: None,
                device_present: None,
                mounted: None,
                size_bytes: None,
                used_bytes: None,
                free_bytes: None,
                detail: None,
            };
            let result = (|| -> Result<(), String> {
                let devices = devices.as_ref().map_err(Clone::clone)?;
                let wanted = role_disk(&journal.spec, role)?;
                if !devices.iter().any(|d| {
                    wanted
                        .wwn
                        .as_ref()
                        .is_some_and(|w| d.wwn.as_ref() == Some(w))
                        || wanted
                            .serial
                            .as_ref()
                            .is_some_and(|s| d.serial.as_ref() == Some(s))
                }) {
                    row.device_present = Some(false);
                    return Ok(());
                }
                let device = resolve(wanted, devices)?;
                row.device_present = Some(true);
                row.device = Some(device.path.clone());
                row.kernel_name = Some(device.kernel.clone());
                let fields = probe_fs(device)?;
                row.observed_uuid = fields.get("UUID").cloned();
                row.filesystem = fields.get("TYPE").cloned();
                filesystem_matches(&journal.spec, role, device)?;
                let mounted = branch_mounted(&journal.spec, role, device)?;
                row.mounted = Some(mounted);
                if mounted {
                    let (size, used, free) = capacity(&mount_path(&journal.spec, role))?;
                    row.size_bytes = Some(size);
                    row.used_bytes = Some(used);
                    row.free_bytes = Some(free);
                }
                Ok(())
            })();
            if let Err(error) = result {
                row.detail = Some(error);
            }
            disks.push(row);
        }
        let union = devices
            .as_ref()
            .ok()
            .and_then(|d| layout(&journal.spec, d).ok())
            .and_then(|s| union_mounted(&s).ok())
            .map(|mounted| mounted && worker.is_none_or(|worker| worker.public_mount().is_some()));
        let union_readonly = worker.and_then(|worker| worker.union_readonly().ok());
        let stuck = stuck_records(journal);
        let hidden = stuck_hidden(journal);
        ElasticResult {
            array_id: journal.spec.array_id.clone(),
            operation_id: journal.spec.operation_id.clone(),
            owner: journal.spec.owner.clone(),
            stage: journal.stage,
            disks,
            union_mounted: union,
            service: journal.private.as_ref().and_then(|private| private.service.clone()),
            union_readonly,
            sync_completed_at: journal.sync_completed_at.clone(),
            detail: journal.detail.clone(),
            last_run: journal.last_run.clone(),
            last_mover: journal
                .transfer
                .as_ref()
                // The run reports what IT evicted; the array carries the
                // figure that never decays.
                .map(|transfer| mover_run(transfer, &stuck, hidden, transfer.evicted)),
            stale_parity_bytes: journal.stale_parity_bytes,
            parity_stale: journal.stale_parity_bytes.is_some(),
            stuck_records: stuck,
            stuck_hidden: hidden,
            stuck_evicted: journal.stuck_evicted,
            restart_required: false,
        }
    }

    /// The array in this boot without an anchor: a recovery after a boot
    /// stopped part-way. Only the next boot retries it; the state is reported
    /// as it is, with the restart it waits for.
    fn restart_required_state(journal: &Journal) -> ElasticResult {
        let mut state = observe(journal, None);
        // The flag says the array waits for a restart; the detail keeps the
        // cause. No reader has to recognise a sentence to learn the state.
        state.restart_required = true;
        state
    }

    fn pending_operation(
        journal: &mut Journal,
        pending: Pending,
        stage: ElasticStage,
        persist: impl FnOnce(&Journal) -> Result<(), String>,
        operation: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        journal.pending = Some(pending);
        journal.stage = stage;
        persist(journal)?;
        operation()
    }

    /// Branch mountpoints and measurements one mover invocation acts on.
    /// Production derives them from the verified layout; tests point them at
    /// private directories and feed the measurements.
    struct MoverEnv<'a> {
        cache: PathBuf,
        data: Vec<MoverTarget>,
        min_free_bytes: u64,
        space: &'a dyn Fn(&Path) -> Result<(u64, u64), String>,
        /// Descriptors other processes hold open; the argument is the private
        /// mergerfs daemon, whose descriptors must be readable.
        open_files: &'a dyn Fn(u32) -> Result<BTreeSet<(u64, u64)>, String>,
        now_ns: i128,
        boot_id: String,
    }

    struct MoverTarget {
        disk: String,
        disk_id: String,
        path: PathBuf,
    }

    /// One typed mover request together with the proof of the held array lock.
    struct MoverRequest<'a> {
        operation_id: &'a str,
        resume_operation_id: &'a str,
        rules: &'a MoverRules,
        coupled_sync: bool,
        array_lock: &'a File,
    }

    /// What a mover run needs beyond the branch directories and the journal:
    /// the step system below `execute_steps`, the branch check and the
    /// SnapRAID guard and argv. Every journal decision stays in this module.
    trait MoverHost {
        fn steps(&mut self) -> &mut dyn StepSystem;
        /// The paths the walk acts on are the array's own mounted branches.
        fn verify_branches(&mut self, journal: &Journal) -> Result<(), String>;
        /// Everything that must hold before SnapRAID starts: its guard over
        /// branches, union, config and parity files, the layout, the program
        /// and its argv.
        fn prepare_sync(&mut self, root: &Root, journal: &Journal) -> Result<(ElasticSpec, String, Vec<String>), String>;
        /// The same guard after SnapRAID; `Ok(true)` when parity is empty.
        fn sync_guard(&mut self, root: &Root, journal: &Journal) -> Result<bool, String>;
        /// Called after every durable write of the in-flight record.
        fn checkpoint(&mut self, _record: &TransferFile) -> Result<(), String> {
            Ok(())
        }
    }

    fn transfer_ref(journal: &Journal) -> Result<&TransferJournal, String> {
        journal.transfer.as_ref().ok_or_else(|| "brak transferu".to_string())
    }

    fn transfer_mut(journal: &mut Journal) -> Result<&mut TransferJournal, String> {
        journal.transfer.as_mut().ok_or_else(|| "brak transferu".to_string())
    }

    fn bounded_text(value: &str, limit: usize) -> String {
        let mut text: String = value
            .chars()
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        if text.len() > limit {
            let mut end = limit;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        text
    }

    fn under_folder(path: &str, folder: &str) -> bool {
        path.strip_prefix(folder)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    fn note_issue(
        transfer: &mut TransferJournal,
        path: &str,
        kind: ElasticMoverIssueKind,
        reason: &str,
        bytes: u64,
    ) {
        match kind {
            ElasticMoverIssueKind::Skipped => {
                transfer.skipped_files = transfer.skipped_files.saturating_add(1);
                transfer.skipped_bytes = transfer.skipped_bytes.saturating_add(bytes);
            }
            ElasticMoverIssueKind::Refused => {
                transfer.refused_files = transfer.refused_files.saturating_add(1);
            }
            ElasticMoverIssueKind::Attention => {}
        }
        if transfer.issues.len() < TRANSFER_ISSUE_LIMIT {
            transfer.issues.push(ElasticMoverIssue {
                path: bounded_text(path, TRANSFER_TEXT_LIMIT),
                kind,
                reason: bounded_text(reason, TRANSFER_TEXT_LIMIT),
            });
        }
    }

    fn valid_transfer_record(file: &TransferFile, temporary_prefix: &str) -> bool {
        let relative = |path: &str| {
            !path.is_empty()
                && !path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
        };
        relative(&file.source)
            && file.destination == file.source
            && file.temporary.as_deref().is_some_and(|name| {
                name.strip_prefix(temporary_prefix).is_some_and(|sequence| {
                    !sequence.is_empty()
                        && sequence.len() <= TEMPORARY_SEQUENCE_DIGITS
                        && sequence.bytes().all(|b| b.is_ascii_digit())
                })
            })
            && file.directory_intent.as_deref().is_none_or(|intent| {
                relative(intent)
                    && file
                        .destination
                        .strip_prefix(intent)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            && file
                .unread_source
                .as_ref()
                .is_none_or(|error| error.len() <= crate::elastic_transfer::UNREAD_SOURCE_LIMIT)
            && serde_json::to_vec(file).is_ok_and(|bytes| bytes.len() <= TRANSFER_RECORD_LIMIT)
    }

    /// Whether this run owes a coupled Sync: parity is out of date and no
    /// Sync of this operation has succeeded yet. A new run owes it even when
    /// it moves nothing, until one of its Syncs makes parity current.
    fn transfer_sync_owed(journal: &Journal, transfer: &TransferJournal) -> bool {
        transfer.coupled_sync
            && !journal.spec.parity.is_empty()
            && journal.stale_parity_bytes.is_some()
            && !transfer
                .coupled_sync_result
                .as_ref()
                .is_some_and(|run| coupled_sync_success(run, &transfer.operation_id))
    }

    /// Whether this transfer forbids a Resume under `operation_id`. The Hold a
    /// mover entered is released only by the Resume UUID it announced. An
    /// unfinished run blocks it only while a file is in flight that its last
    /// attempt did not give up on; an owed Sync never does — the array then
    /// comes back with parity marked stale and the next run owes the Sync.
    fn transfer_blocks_resume(journal: &Journal, transfer: &TransferJournal, operation_id: &str) -> bool {
        let own_hold = journal
            .private
            .as_ref()
            .and_then(|private| private.service.as_ref())
            .is_some_and(|service| service.operation_id == transfer.operation_id);
        if transfer.finished_at.is_some() {
            return own_hold && operation_id != transfer.resume_operation_id;
        }
        operation_id != transfer.resume_operation_id
            || (transfer.current.is_some() && transfer.current_failed.is_none())
    }

    /// A Resume that finishes under the transfer's declared UUID closes an
    /// unfinished run. It stays NeedsAttention in the history, with a stuck
    /// record kept as it was and appended to the array's stuck history, which
    /// outlives every later operation.
    /// Returns the identity the write evicted from the skip set, if any: the
    /// caller says so only once the write is durable, because this runs on a
    /// CLONE and a refused save drops it.
    fn close_transfer(
        journal: &mut Journal,
        service_operation_id: &str,
    ) -> Result<Option<StuckPath>, String> {
        let Some(transfer) = journal.transfer.as_mut().filter(|transfer| {
            transfer.finished_at.is_none() && transfer.resume_operation_id == service_operation_id
        }) else {
            return Ok(None);
        };
        if transfer.current.is_some() && transfer.current_failed.is_none() {
            return Err("transfer ma plik w toku".into());
        }
        let stuck = stuck_entry(transfer);
        transfer.phase = ElasticMoverPhase::NeedsAttention;
        transfer.finished_at = Some(timestamp()?);
        let carried_detail = transfer.detail.clone();
        if let Some(record) = stuck {
            // The skip set is what later runs consult, so it is kept first, and
            // the NEWEST stuck path is always the one protected: when the set is
            // full its oldest digest is evicted to make room. A path that leaves
            // the set is no longer skipped — a record stuck before its rename
            // has no destination to refuse a later run, which would move the
            // file again and orphan the copy already on the branch — so the
            // eviction is counted and said in the run's detail, which nothing
            // drops, rather than in an issue list that is bounded.
            let known = journal
                .stuck_paths
                .iter()
                .any(|path| path.path_sha256 == record.path_sha256);
            let mut evicted = None;
            if !known {
                if journal.stuck_paths.len() >= STUCK_PATH_LIMIT {
                    // The evicted path keeps its identity: the ring holds the
                    // last few, and the log holds every one. Without it the
                    // temporary left on the branch has no name, and a record
                    // stuck before its rename has no destination to refuse a
                    // later run, so nobody could ever acknowledge the orphan.
                    let dropped = journal.stuck_paths.remove(0);
                    journal.stuck_evicted = journal.stuck_evicted.saturating_add(1);
                    evicted = Some(dropped.clone());
                    journal.evicted_paths.push(dropped);
                    while journal.evicted_paths.len() > EVICTED_RING {
                        journal.evicted_paths.remove(0);
                    }
                }
                journal.stuck_paths.push(StuckPath {
                    path_sha256: record.path_sha256.clone(),
                    operation_id: record.operation_id.clone(),
                    temporary: record.temporary.clone(),
                });
            }
            push_stuck_record(journal, record);
            if evicted.is_some() {
                let total = journal.stuck_evicted;
                let transfer = journal.transfer.as_mut().ok_or("brak transferu")?;
                transfer.evicted = transfer.evicted.saturating_add(1);
                let notice = format!(
                    "lista pominiętych ścieżek pełna ({STUCK_PATH_LIMIT}): najstarsza ścieżka nie jest już pomijana (łącznie wypartych: {total})"
                );
                // Prefixed, never appended: the bound truncates at the tail, so
                // an appended notice would be the first thing a long carried
                // detail cuts away.
                transfer.detail = Some(bounded_text(
                    &match carried_detail {
                        Some(detail) => format!("{notice}; {detail}"),
                        None => notice,
                    },
                    TRANSFER_DETAIL_LIMIT,
                ));
            }
            return Ok(evicted);
        }
        Ok(None)
    }

    /// A text of `limit` bytes that costs the most a bounded text can cost in
    /// the journal. Every text the journal holds passes `bounded_text` or a
    /// validation that refuses control characters, so a byte escapes to at
    /// most two.
    fn worst_text(limit: usize) -> String {
        "\"".repeat(limit)
    }

    /// The journal this operation could still be asked to write before it can
    /// end: the record at its worst case, the failure text a stuck record
    /// writes, every detail and the issue list at their bounds, and the entry
    /// the closing Resume appends to the history and to the skip set. A save
    /// the limit refuses in mid-record cannot be retried away, so a file whose
    /// projection does not fit is refused before it is started.
    /// A finished Sync record at its largest, for pricing a run that has not
    /// written one yet.
    fn worst_case_run(operation_id: &str) -> ElasticSnapraidRun {
        ElasticSnapraidRun {
            operation_id: operation_id.to_string(),
            kind: ElasticSnapraidKind::Sync,
            started_at: "2026-12-31T23:59:59Z".into(),
            finished_at: Some("2026-12-31T23:59:59Z".into()),
            outcome: ElasticSnapraidOutcome::Failed,
            exit_code: Some(i32::MIN),
            total_blocks: Some(u64::MAX),
            checked_blocks: Some(u64::MAX),
            accessed_mb: Some(u64::MAX),
            errors_file: Some(u64::MAX),
            errors_io: Some(u64::MAX),
            errors_data: Some(u64::MAX),
            detail: Some(worst_text(TRANSFER_DETAIL_LIMIT)),
        }
    }

    fn worst_case_journal_size(journal: &Journal, record: &TransferFile) -> Result<usize, String> {
        let mut projected = journal.clone();
        let operation = transfer_ref(&projected)?.operation_id.clone();
        projected.detail = Some(worst_text(TRANSFER_DETAIL_LIMIT));
        // The coupled Sync this run still owes is priced even when nothing has
        // been written yet: at plan time `coupled_sync_result` is always None
        // (a new transfer may not carry one), and the run then writes the whole
        // record, the stale markers included.
        *projected.last_run.get_or_insert_with(|| worst_case_run(&operation)) =
            worst_case_run(&operation);
        projected.stale_parity_bytes = Some(u64::MAX);
        projected.stale_sync_operation = Some(operation.clone());
        let digest = path_digest(&record.source);
        let worst_pin = ElasticFilePin {
            device: u64::MAX,
            inode: u64::MAX,
            size: Some(u64::MAX),
            sha256: Some("f".repeat(64)),
        };
        let transfer = transfer_mut(&mut projected)?;
        transfer.current = Some(crate::elastic_transfer::worst_case_record(record));
        transfer.current_failed = Some(worst_text(TRANSFER_DETAIL_LIMIT));
        transfer.detail = Some(worst_text(TRANSFER_DETAIL_LIMIT));
        *transfer.coupled_sync_result.get_or_insert_with(|| worst_case_run(&operation)) =
            worst_case_run(&operation);
        // The closing write moves these two as well.
        transfer.phase = ElasticMoverPhase::NeedsAttention;
        transfer.finished_at = Some("2026-12-31T23:59:59Z".into());
        transfer.issues = (0..TRANSFER_ISSUE_LIMIT)
            .map(|_| ElasticMoverIssue {
                path: worst_text(TRANSFER_TEXT_LIMIT),
                kind: ElasticMoverIssueKind::Attention,
                reason: worst_text(TRANSFER_TEXT_LIMIT),
            })
            .collect();
        let entry = ElasticStuckRecord {
            operation_id: transfer.operation_id.clone(),
            path: worst_text(TRANSFER_TEXT_LIMIT),
            path_sha256: digest.clone(),
            target: transfer.target.clone(),
            temporary: record.temporary.clone(),
            source: worst_pin.clone(),
            temporary_copy: Some(worst_pin.clone()),
            destination: Some(worst_pin),
            reason: worst_text(TRANSFER_TEXT_LIMIT),
            skipped: false,
        };
        let operation_id = entry.operation_id.clone();
        push_stuck_record(&mut projected, entry);
        if !projected.stuck_paths.iter().any(|path| path.path_sha256 == digest) {
            // The closing write may also evict, which moves one identity into
            // the ring and costs its digest, operation and temporary there.
            if projected.stuck_paths.len() >= STUCK_PATH_LIMIT {
                let dropped = projected.stuck_paths.remove(0);
                projected.stuck_evicted = projected.stuck_evicted.saturating_add(1);
                projected.evicted_paths.push(dropped);
                while projected.evicted_paths.len() > EVICTED_RING {
                    projected.evicted_paths.remove(0);
                }
            }
            projected.stuck_paths.push(StuckPath {
                path_sha256: digest,
                operation_id,
                temporary: record.temporary.clone(),
            });
        }
        if let Some(transfer) = projected.transfer.as_mut() {
            transfer.evicted = transfer.evicted.saturating_add(1);
        }
        // What this projection models, so the next reader can check it against
        // the journal's fields: the in-flight record at its worst, the failure
        // text, every detail and the issue list at their bounds, the entry the
        // closing Resume appends to the history and to the skip set (with the
        // eviction it may force, into the ring), both eviction counters, the
        // coupled Sync record and `last_run` at their worst, and the two stale
        // markers.
        //
        // ...the phase and the finish time the closing write sets, too.
        //
        // What it does NOT cover. Every one of these is bounded, and the list
        // is meant to be COMPLETE — a reader checks it against the journal's
        // fields, so an omission here reads as "already priced" and quietly
        // eats the cushion:
        //   - `transfer.target`, null to "dNN" with the first moved file (+3 B),
        //     and the temporary's `sequence` gaining a digit (+1 B, never more
        //     than `TEMPORARY_SEQUENCE_DIGITS`);
        //   - `journal.stage`, whose longest value is `needs_attention` (at
        //     most +11 B over the shortest);
        //   - `journal.sync_completed_at`, null to an RFC3339 instant (+22 B),
        //     which only a successful coupled Sync writes;
        //   - `journal.pending`, null to a `Pending::Maintenance` naming an
        //     operation UUID and a kind (under +90 B);
        //   - `private.service.pending`, a bool flipped by the closing write
        //     (at most +1 B, `false` being the longer spelling);
        //   - `transfer.walked`, the same shape (at most +1 B);
        //   - the five run counters (`moved_files`, `moved_bytes`,
        //     `skipped_files`, `skipped_bytes`, `refused_files`), each growing
        //     by digits alone and each bounded by `u64::MAX` at 20 digits, so
        //     under +100 B for all five together.
        // That is under 230 B in total, against a cushion worth about 2,050 B:
        // this projection prices `journal.detail` at its bound while `finish`
        // writes `detail = None` on the very write it prices. Any FIELD added
        // to the journal from here on must be added HERE too, or it will eat
        // that slack unseen.
        serde_json::to_vec(&projected)
            .map(|bytes| bytes.len())
            .map_err(|e| e.to_string())
    }

    /// Adds a full record to the display history, summarising the oldest away
    /// when the list is full. Its path stays in the skip set.
    fn push_stuck_record(journal: &mut Journal, record: ElasticStuckRecord) {
        journal.stuck.push(record);
        while journal.stuck.len() > STUCK_LIMIT {
            journal.stuck.remove(0);
        }
    }

    /// Stuck paths the journal still skips but no longer shows in full. A
    /// record whose own digest has been evicted is shown without being skipped,
    /// so it is not one of these.
    fn stuck_hidden(journal: &Journal) -> u64 {
        journal
            .stuck_paths
            .iter()
            .filter(|path| {
                !journal
                    .stuck
                    .iter()
                    .any(|record| record.path_sha256 == path.path_sha256)
            })
            .count() as u64
    }

    /// What an eviction must say where no bound can drop it: which path
    /// stopped being skipped, the operation that left it, and the temporary it
    /// abandoned on the data branch.
    fn log_eviction(dropped: &StuckPath) {
        emit_notice(&format!(
            "elastic mover: ścieżka wyparta z listy pominięć: sha256={} operacja={} kopia tymczasowa={}",
            dropped.path_sha256,
            dropped.operation_id,
            dropped.temporary.as_deref().unwrap_or("nieznana"),
        ));
    }

    /// Where a line no bounded field may drop goes. One call site: the system
    /// log in every real build, and a capturing sink under test — installed by
    /// DEFAULT, so no test run can ever write to the host's system log.
    fn emit_notice(text: &str) {
        #[cfg(test)]
        {
            EVICTION_LOG.with(|log| log.borrow_mut().get_or_insert_with(Vec::new).push(text.to_string()));
            return;
        }
        #[cfg(not(test))]
        crate::syslog_notice(text);
    }

    /// The production sink still type-checks in a test build, where nothing
    /// calls it.
    #[cfg(test)]
    const _: fn(&str) = crate::syslog_notice;

    #[cfg(test)]
    thread_local! {
        /// Everything `emit_notice` said on this thread.
        static EVICTION_LOG: std::cell::RefCell<Option<Vec<String>>> =
            const { std::cell::RefCell::new(None) };
    }

    fn file_pin(pin: &crate::elastic_transfer::TransferPin) -> ElasticFilePin {
        ElasticFilePin {
            device: pin.device,
            inode: pin.inode,
            size: Some(pin.size),
            sha256: Some(pin.sha256.clone()),
        }
    }

    /// The transfer's in-flight record as a stuck record, when its last
    /// attempt gave up on it.
    fn stuck_entry(transfer: &TransferJournal) -> Option<ElasticStuckRecord> {
        let (file, reason) = (transfer.current.as_ref()?, transfer.current_failed.as_ref()?);
        Some(ElasticStuckRecord {
            operation_id: transfer.operation_id.clone(),
            path: bounded_text(&file.source, TRANSFER_TEXT_LIMIT),
            path_sha256: path_digest(&file.source),
            target: transfer.target.clone(),
            // The orphan this record left was verified as ours and deleted, so
            // the record must stop naming it: nothing downstream — this list,
            // the skip set built from it, or the eviction log line — may report
            // an orphan the helper already cleaned up.
            temporary: (!file.temporary_removed).then(|| file.temporary.clone()).flatten(),
            source: ElasticFilePin {
                device: file.source_identity.device,
                inode: file.source_identity.inode,
                size: Some(file.source_identity.size),
                sha256: Some(file.source_identity.sha256.clone()),
            },
            temporary_copy: match (&file.temporary_identity, file.temporary_pin) {
                _ if file.temporary_removed => None,
                (Some(pin), _) => Some(file_pin(pin)),
                (None, Some((device, inode))) => Some(ElasticFilePin { device, inode, size: None, sha256: None }),
                (None, None) => None,
            },
            destination: file.destination_identity.as_ref().map(file_pin),
            reason: bounded_text(reason, TRANSFER_TEXT_LIMIT),
            // Recomputed by `stuck_records` on every read, so the journal
            // stores the value that costs it nothing.
            skipped: false,
        })
    }

    /// The array's stuck records: the closed history, then the running
    /// operation's own record if its last attempt gave up on it.
    fn stuck_records(journal: &Journal) -> Vec<ElasticStuckRecord> {
        let mut records = journal.stuck.clone();
        if let Some(transfer) = journal.transfer.as_ref().filter(|transfer| transfer.finished_at.is_none()) {
            records.extend(stuck_entry(transfer));
        }
        // Whether a shown record is still skipped is a fact about the skip set
        // now, never about what was written when the record was kept.
        for record in &mut records {
            record.skipped = journal
                .stuck_paths
                .iter()
                .any(|path| path.path_sha256 == record.path_sha256);
        }
        records
    }

    fn file_phase_rank(phase: TransferFilePhase) -> u8 {
        match phase {
            TransferFilePhase::CopyIntent => 0,
            TransferFilePhase::CopyConfirmed => 1,
            TransferFilePhase::RenameIntent => 2,
            TransferFilePhase::RenameConfirmed => 3,
            TransferFilePhase::UnlinkIntent => 4,
            TransferFilePhase::UnlinkConfirmed => 5,
            TransferFilePhase::Done => 6,
        }
    }

    /// The durable transfer only moves forward: `Root::save` refuses a write
    /// that contradicts the state already on disk.
    fn transfer_transition(old: &Journal, new: &Journal) -> Result<(), String> {
        use ElasticMoverPhase as Phase;
        let refuse = |why: &str| -> Result<(), String> {
            Err(format!("sprzeczne przejście transferu: {why}"))
        };
        match (&old.transfer, &new.transfer) {
            (None, None) => {}
            (Some(_), None) => return refuse("usunięcie transferu"),
            (Some(previous), Some(next)) if previous.operation_id == next.operation_id => {
                if previous.resume_operation_id != next.resume_operation_id
                    || previous.started_at != next.started_at
                    || previous.rules != next.rules
                    || previous.coupled_sync != next.coupled_sync
                {
                    return refuse("zmiana stałych operacji");
                }
                if previous.finished_at.is_some() && previous != next {
                    return refuse("zmiana zakończonego transferu");
                }
                if previous.target.is_some() && previous.target != next.target {
                    return refuse("zmiana brancha docelowego");
                }
                if next.sequence < previous.sequence
                    || next.moved_files < previous.moved_files
                    || next.moved_bytes < previous.moved_bytes
                    || next.sync_attempt < previous.sync_attempt
                {
                    return refuse("cofnięty licznik");
                }
                let phase_allowed = previous.phase == next.phase
                    || matches!(
                        (previous.phase, next.phase),
                        (Phase::Holding, Phase::Moving | Phase::NeedsAttention)
                            | (Phase::Moving, Phase::Syncing | Phase::Complete | Phase::NeedsAttention)
                            | (Phase::Syncing, Phase::Moving | Phase::Complete | Phase::NeedsAttention)
                            | (Phase::NeedsAttention, Phase::Moving)
                    );
                if !phase_allowed {
                    return refuse("niedozwolona zmiana fazy");
                }
                match (&previous.current, &next.current) {
                    (None, None) => {}
                    (None, Some(file)) => {
                        if file.phase != TransferFilePhase::CopyIntent
                            || previous.sequence.checked_add(1) != Some(next.sequence)
                        {
                            return refuse("plik w toku bez zapowiedzi");
                        }
                    }
                    (Some(before), Some(after)) => {
                        if before.source != after.source
                            || before.destination != after.destination
                            || before.temporary != after.temporary
                            || before.source_identity != after.source_identity
                            || next.sequence != previous.sequence
                            || file_phase_rank(after.phase) < file_phase_rank(before.phase)
                            || (before.temporary_pin.is_some() && before.temporary_pin != after.temporary_pin)
                            || (before.temporary_identity.is_some()
                                && before.temporary_identity != after.temporary_identity)
                            || (before.destination_identity.is_some()
                                && before.destination_identity != after.destination_identity)
                        {
                            return refuse("cofnięty lub podmieniony plik w toku");
                        }
                    }
                    (Some(before), None) => {
                        let completed = before.phase == TransferFilePhase::Done
                            && previous.moved_files.checked_add(1) == Some(next.moved_files);
                        let withdrawn = file_phase_rank(before.phase)
                            <= file_phase_rank(TransferFilePhase::RenameIntent)
                            && next.moved_files == previous.moved_files;
                        if !completed && !withdrawn {
                            return refuse("plik w toku zniknął bez potwierdzenia");
                        }
                    }
                }
            }
            (previous, Some(next)) => {
                if previous.as_ref().is_some_and(|previous| previous.finished_at.is_none()) {
                    return refuse("zastąpienie niedokończonego transferu");
                }
                if next.phase != Phase::Holding
                    || next.finished_at.is_some()
                    || next.current.is_some()
                    || next.evicted != 0
                    || next.current_failed.is_some()
                    || next.target.is_some()
                    || next.sequence != 0
                    || next.moved_files != 0
                    || next.moved_bytes != 0
                    || next.coupled_sync_result.is_some()
                    || next.sync_attempt != 0
                {
                    return refuse("nowy transfer zaczyna się od Holding");
                }
            }
        }
        // Nothing removes a skipped path, and a record is appended only by the
        // write that closes the operation that left it. The display history
        // keeps the newest records: when it is full its oldest is summarised
        // away, and the skip set keeps that path.
        let paths_changed = new.stuck_paths != old.stuck_paths;
        let paths_appended = new.stuck_paths.len() == old.stuck_paths.len() + 1
            && new.stuck_paths.starts_with(&old.stuck_paths);
        // A skipped path is removed ONLY as the eviction that makes room for an
        // insert in the same write: the newest stuck path is always protected.
        let paths_evicted = old.stuck_paths.len() == STUCK_PATH_LIMIT
            && new.stuck_paths.len() == STUCK_PATH_LIMIT
            && new.stuck_paths[..STUCK_PATH_LIMIT - 1] == old.stuck_paths[1..];
        if paths_changed && !paths_appended && !paths_evicted {
            return refuse("zmiana listy pominiętych ścieżek");
        }
        // Both counters move with the eviction, and only with it.
        if new.stuck_evicted != old.stuck_evicted.saturating_add(u64::from(paths_evicted)) {
            return refuse("licznik wypartych ścieżek nie zgadza się z wyparciem");
        }
        let run_evicted = |journal: &Journal| journal.transfer.as_ref().map_or(0, |transfer| transfer.evicted);
        let same_operation = match (&old.transfer, &new.transfer) {
            (Some(before), Some(after)) => before.operation_id == after.operation_id,
            _ => false,
        };
        if same_operation
            && run_evicted(new) != run_evicted(old).saturating_add(u64::from(paths_evicted))
        {
            return refuse("licznik wyparć przebiegu nie zgadza się z wyparciem");
        }
        // The ring grows only with an eviction, and only by the identity that
        // actually left the skip set.
        let ring_ok = match (paths_evicted, old.evicted_paths.len()) {
            (false, _) => new.evicted_paths == old.evicted_paths,
            (true, len) if len < EVICTED_RING => {
                new.evicted_paths.len() == len + 1 && new.evicted_paths.starts_with(&old.evicted_paths)
            }
            (true, _) => {
                new.evicted_paths.len() == EVICTED_RING
                    && new.evicted_paths[..EVICTED_RING - 1] == old.evicted_paths[1..]
            }
        };
        if !ring_ok || (paths_evicted && new.evicted_paths.last() != old.stuck_paths.first()) {
            return refuse("pierścień wypartych ścieżek nie zgadza się z wyparciem");
        }
        let kept = old.stuck.len().min(STUCK_LIMIT.saturating_sub(1));
        let appended = new.stuck.len() == old.stuck.len() + 1 && new.stuck.starts_with(&old.stuck);
        let rolled = old.stuck.len() == STUCK_LIMIT
            && new.stuck.len() == STUCK_LIMIT
            && new.stuck[..kept] == old.stuck[old.stuck.len() - kept..];
        if new.stuck != old.stuck && !appended && !rolled {
            return refuse("zmiana historii utkniętych rekordów");
        }
        if let Some(added) = new.stuck.last().filter(|_| new.stuck != old.stuck) {
            let closes = matches!(
                (&old.transfer, &new.transfer),
                (Some(before), Some(after))
                    if before.finished_at.is_none()
                        && before.current_failed.is_some()
                        && after.finished_at.is_some()
                        && after.operation_id == before.operation_id
                        && added.operation_id == before.operation_id
            );
            if !closes {
                return refuse("utknięty rekord poza zamknięciem jego operacji");
            }
        }
        // The same holds for a skipped path: it appears — and evicts — only in
        // the write that closes the operation whose record stuck on it.
        if let Some(added) = new.stuck_paths.last().filter(|_| paths_changed) {
            let closes = matches!(
                (&old.transfer, &new.transfer),
                (Some(before), Some(after))
                    if before.finished_at.is_none()
                        && before.current_failed.is_some()
                        && after.finished_at.is_some()
                        && after.operation_id == before.operation_id
                        && added.operation_id == before.operation_id
            );
            if !closes {
                return refuse("utknięty rekord poza zamknięciem jego operacji");
            }
        }
        match (old.stale_parity_bytes, new.stale_parity_bytes) {
            (Some(before), Some(after)) if after < before => refuse("zmniejszona nieaktualna parity"),
            // Only the write that records a successful Sync may clear the marker.
            (Some(_), None)
                if new.last_run == old.last_run
                    || !new.last_run.as_ref().is_some_and(|run| {
                        run.kind == ElasticSnapraidKind::Sync
                            && run.outcome == ElasticSnapraidOutcome::Succeeded
                            && run.finished_at.is_some()
                            && run.finished_at == new.sync_completed_at
                    }) =>
            {
                refuse("parity uznana za aktualną bez udanego Sync")
            }
            _ => Ok(()),
        }
    }

    fn mover_run(
        transfer: &TransferJournal,
        stuck: &[ElasticStuckRecord],
        hidden: u64,
        evicted: u64,
    ) -> ElasticMoverRun {
        ElasticMoverRun {
            operation_id: transfer.operation_id.clone(),
            resume_operation_id: transfer.resume_operation_id.clone(),
            phase: transfer.phase,
            started_at: transfer.started_at.clone(),
            finished_at: transfer.finished_at.clone(),
            moved_files: transfer.moved_files,
            moved_bytes: transfer.moved_bytes,
            skipped_files: transfer.skipped_files,
            skipped_bytes: transfer.skipped_bytes,
            refused_files: transfer.refused_files,
            issues: transfer.issues.clone(),
            counts_known: transfer.walked,
            detail: transfer.detail.clone().or_else(|| transfer.current_failed.clone()),
            coupled_sync: transfer.coupled_sync_result.clone(),
            stuck_records: stuck.to_vec(),
            stuck_hidden: hidden,
            stuck_evicted: evicted,
        }
    }

    fn persisted_target(env: &MoverEnv, transfer: &TransferJournal) -> Result<PathBuf, String> {
        let disk = transfer
            .target
            .as_deref()
            .ok_or("transfer w toku bez utrwalonego brancha docelowego")?;
        env.data
            .iter()
            .find(|target| target.disk == disk)
            .map(|target| target.path.clone())
            .ok_or_else(|| "utrwalony branch docelowy nie należy do macierzy".into())
    }

    fn now_ns() -> Result<i128, String> {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        i128::try_from(elapsed.as_nanos()).map_err(|e| e.to_string())
    }

    /// Records why an unfinished transfer stopped, over its last durable
    /// state, so an error never leaves the run looking like it still moves.
    fn mark_attention(root: &Root, array_id: &str, operation_id: &str, error: String) -> String {
        let mut stored = match root.load(array_id) {
            Ok(stored) => stored,
            Err(load) => return format!("{error}; odczyt stanu: {load}"),
        };
        let Some(transfer) = stored.transfer.as_mut().filter(|transfer| {
            transfer.operation_id == operation_id && transfer.finished_at.is_none()
        }) else {
            return error;
        };
        transfer.phase = ElasticMoverPhase::NeedsAttention;
        transfer.detail = Some(bounded_text(&error, TRANSFER_DETAIL_LIMIT));
        stored.stage = ElasticStage::NeedsAttention;
        stored.detail = Some(bounded_text(&error, TRANSFER_DETAIL_LIMIT));
        match root.save(&stored) {
            Ok(()) => error,
            Err(save) => {
                // The detail may be what the limit refused: retry short.
                stored.detail = Some(SHORT_ATTENTION.into());
                if let Some(transfer) = stored.transfer.as_mut() {
                    transfer.detail = Some(SHORT_ATTENTION.into());
                    transfer.issues.clear();
                }
                match root.save(&stored) {
                    Ok(()) => format!("{error}; {save}"),
                    Err(second) => format!("{error}; zapis stanu: {second}"),
                }
            }
        }
    }

    /// Closes this operation's Sync that a gone process left Running: the
    /// history keeps it as an interrupted attempt, parity is marked stale and
    /// no pending remains. Returns whether there was one.
    fn close_interrupted_sync(journal: &mut Journal, operation_id: &str, detail: &str) -> Result<bool, String> {
        let Some(run) = journal.last_run.as_mut().filter(|run| {
            run.operation_id == operation_id
                && run.kind == ElasticSnapraidKind::Sync
                && run.outcome == ElasticSnapraidOutcome::Running
        }) else {
            return Ok(false);
        };
        run.outcome = ElasticSnapraidOutcome::NeedsAttention;
        run.finished_at = Some(timestamp()?);
        run.detail = Some(bounded_text(detail, TRANSFER_DETAIL_LIMIT));
        let run = run.clone();
        journal.pending = None;
        journal.stale_parity_bytes.get_or_insert(0);
        journal.stale_sync_operation = Some(operation_id.to_string());
        if journal.stage == ElasticStage::SyncPending {
            journal.stage = ElasticStage::NeedsAttention;
        }
        if let Some(transfer) = journal
            .transfer
            .as_mut()
            .filter(|transfer| transfer.operation_id == operation_id)
        {
            transfer.coupled_sync_result = Some(run);
        }
        Ok(true)
    }

    /// After a Sync attempt failed past `begin_coupled_sync`, the durable
    /// state must not keep it Running.
    fn close_attempt(root: &Root, array_id: &str, operation_id: &str, error: String) -> String {
        let mut stored = match root.load(array_id) {
            Ok(stored) => stored,
            Err(load) => return format!("{error}; odczyt stanu: {load}"),
        };
        match close_interrupted_sync(&mut stored, operation_id, &error) {
            Ok(true) => match root.save(&stored) {
                Ok(()) => error,
                Err(save) => format!("{error}; zapis stanu: {save}"),
            },
            Ok(false) => error,
            Err(close) => format!("{error}; {close}"),
        }
    }

    /// What became of the in-flight record.
    enum FileOutcome {
        Moved,
        /// A syscall failed while the source was untouched; the copy was
        /// withdrawn and the file stays on the cache.
        RolledBack(String),
    }

    /// The in-flight record could be neither finished nor withdrawn. It stays
    /// exactly as it is and both copies stay on disk; parity is marked stale
    /// and the record no longer blocks the declared Resume.
    /// Gives up on one record, and takes its orphaned copy with it.
    ///
    /// ORDERING: the delete happens BEFORE the durable write, and that
    /// direction is chosen, not incidental. A crash in this window leaves a
    /// record naming a temporary that is already gone — and that reads as
    /// CLEAN: at `CopyIntent` a missing temporary is recreated (ENOENT ->
    /// O_CREAT|O_EXCL), and past it the next run sticks the record again and
    /// repeats this cleanup, which is idempotent because an absent file is a
    /// success here. The reverse order cannot be made safe: a crash between
    /// the write and the unlink would leave a file on the branch that the
    /// journal has already declared removed — an orphan nothing names and
    /// nobody can attribute, which is the exact failure the skip set exists to
    /// prevent.
    ///
    /// `array_lock` DOCUMENTS that the caller holds the array lock for the whole
    /// run; it does not enforce it. Any `&File` satisfies the parameter, so a
    /// caller that moved this work outside the lock would still compile. Real
    /// enforcement would need a token only `Root::array_lock` can mint, which
    /// is deliberately not built here — the parameter is a signpost, and it is
    /// recorded as one rather than counted as a guarantee.
    fn stick_record(
        root: &Root,
        journal: &mut Journal,
        destination_root: &Path,
        array_lock: &File,
        file: &TransferFile,
        error: String,
    ) -> String {
        let _ = array_lock;
        // Cleanup never decides whether the run goes on. A file we could not
        // prove is ours stays exactly where it is, the path stays skipped, and
        // the run continues — a stuck file stopping the run is the thing the
        // skip set was built to avoid.
        // ONLY a delete this run performed clears the record's claim. `Absent`
        // looks like "nothing is there", but it conflates two different facts:
        // the orphan was already cleaned up, OR this record got past its rename
        // and its copy is alive under the DESTINATION name. Clearing the claim
        // in the second case would make the record stop naming a file that is
        // still on the branch — constraint 6 inverted, and the worse direction.
        let (cleanup, unnamed) = match crate::elastic_transfer::remove_orphan_temporary(destination_root, file) {
            Ok(crate::elastic_transfer::OrphanCleanup::Removed { fsync }) => {
                let mut said = format!(
                    "usunięto porzuconą kopię tymczasową: {} operacja={}",
                    file.temporary.as_deref().unwrap_or("nieznana"),
                    journal.transfer.as_ref().map_or("", |t| t.operation_id.as_str()),
                );
                // The file IS gone; only the durability of the directory entry
                // is in doubt. Said as an extra clause, never as a retraction.
                if let Some(error) = fsync {
                    said.push_str(&format!("; wpis katalogu niepotwierdzony: {error}"));
                }
                emit_notice(&format!("elastic mover: {said}"));
                (Some(said), true)
            }
            Ok(crate::elastic_transfer::OrphanCleanup::Absent) => (None, false),
            Err(failure) => {
                let said = format!("porzucona kopia tymczasowa nie została usunięta: {failure}");
                emit_notice(&format!("elastic mover: {said}"));
                (Some(said), false)
            }
        };
        let reason = bounded_text(
            &match &cleanup {
                Some(said) => format!("rekord nierozwiązany: {error}; {said}"),
                None => format!("rekord nierozwiązany: {error}"),
            },
            TRANSFER_DETAIL_LIMIT,
        );
        let parity = !journal.spec.parity.is_empty();
        let removed = unnamed;
        if let Some(transfer) = journal.transfer.as_mut() {
            transfer.current_failed = Some(reason.clone());
            // The record the journal keeps is the one that must stop claiming
            // the deleted copy; `file` is the caller's own snapshot.
            if removed {
                if let Some(current) = transfer.current.as_mut() {
                    current.temporary_removed = true;
                }
            }
            note_issue(transfer, &file.source, ElasticMoverIssueKind::Attention, &reason, 0);
        }
        if parity {
            journal.stale_parity_bytes.get_or_insert(file.source_identity.size);
        }
        match root.save(journal) {
            Ok(()) => reason,
            Err(save) => format!("{reason}; zapis stanu: {save}"),
        }
    }

    /// A journal write the size limit refused leaves the record exactly as
    /// the disk holds it, with nothing saying its attempt gave up — and a
    /// record like that blocks the declared Resume. The last durable state
    /// gets a SHORT, FIXED failure text instead, never the error itself (which
    /// may be what overflowed), and the run becomes releasable.
    const SHORT_STUCK_REASON: &str = "rekord nierozwiązany: zapis dziennika odrzucony";
    const SHORT_ATTENTION: &str = "operacja przerwana; szczegóły nie zmieściły się w dzienniku";

    fn stick_short_record(root: &Root, journal: &mut Journal, error: String) -> String {
        let array_id = journal.spec.array_id.clone();
        let mut stored = match root.load(&array_id) {
            Ok(stored) => stored,
            Err(load) => return format!("{error}; odczyt stanu: {load}"),
        };
        let parity = !stored.spec.parity.is_empty();
        let Some(transfer) = stored.transfer.as_mut().filter(|transfer| {
            transfer.finished_at.is_none() && transfer.current.is_some() && transfer.current_failed.is_none()
        }) else {
            return error;
        };
        let size = transfer.current.as_ref().map_or(0, |file| file.source_identity.size);
        transfer.current_failed = Some(SHORT_STUCK_REASON.into());
        transfer.phase = ElasticMoverPhase::NeedsAttention;
        transfer.detail = Some(SHORT_STUCK_REASON.into());
        // Every byte counts on this write: a re-run measures the issues again.
        transfer.issues.clear();
        if parity {
            stored.stale_parity_bytes.get_or_insert(size);
        }
        stored.stage = ElasticStage::NeedsAttention;
        stored.detail = Some(SHORT_STUCK_REASON.into());
        match root.save(&stored) {
            Ok(()) => {
                *journal = stored;
                format!("{error}; {SHORT_STUCK_REASON}")
            }
            Err(save) => format!("{error}; zapis stanu: {save}"),
        }
    }

    /// Drives the one in-flight record to Done: parents first, then the
    /// per-file state machine. Every durable step goes through `Root::save`.
    fn finish_file(
        root: &Root,
        journal: &mut Journal,
        source: &Path,
        target: &Path,
        array_lock: &File,
        mut file: TransferFile,
        host: &mut impl MoverHost,
    ) -> Result<FileOutcome, String> {
        let durable = std::cell::Cell::new(true);
        // A refused journal write and a failed checkpoint are different: the
        // first leaves the durable record looking healthy, the second does not.
        let save_refused = std::cell::Cell::new(false);
        let mut persist = |record: &TransferFile| -> Result<(), String> {
            let saved = transfer_mut(&mut *journal)
                .map(|transfer| transfer.current = Some(record.clone()))
                .and_then(|()| root.save(&*journal));
            if let Err(error) = saved {
                durable.set(false);
                save_refused.set(true);
                return Err(error);
            }
            let checked = host.checkpoint(record);
            if checked.is_err() {
                durable.set(false);
            }
            checked
        };
        let rollbackable = |phase| {
            matches!(
                phase,
                TransferFilePhase::CopyIntent | TransferFilePhase::CopyConfirmed | TransferFilePhase::RenameIntent
            )
        };
        let mut executed = Ok(TransferEnd::Verified);
        if rollbackable(file.phase) {
            executed = crate::elastic_transfer::prepare_parents(source, target, &mut file, &mut persist)
                .map(|()| TransferEnd::Verified);
        }
        if executed.is_ok() {
            executed = transfer_file(source, target, &mut file, &mut persist);
        }
        match executed {
            Ok(end) if file.phase == TransferFilePhase::Done => {
                let size = file.source_identity.size;
                let transfer = transfer_mut(journal)?;
                transfer.moved_files = transfer
                    .moved_files
                    .checked_add(1)
                    .ok_or("przepełnienie licznika plików")?;
                transfer.moved_bytes = transfer.moved_bytes.saturating_add(size);
                transfer.current = None;
                transfer.current_failed = None;
                if let TransferEnd::SourceUnreadable(error) = end {
                    note_issue(
                        transfer,
                        &file.source,
                        ElasticMoverIssueKind::Attention,
                        &format!("źródło usunięte po tożsamości po błędzie odczytu: {error}"),
                        0,
                    );
                }
                // Parity no longer describes a data branch that just gained a file.
                if !journal.spec.parity.is_empty() {
                    journal.stale_parity_bytes =
                        Some(journal.stale_parity_bytes.unwrap_or(0).saturating_add(size));
                }
                root.save(journal)?;
                Ok(FileOutcome::Moved)
            }
            Ok(_) => Err("transfer pliku bez potwierdzenia".into()),
            // A refused journal write is a crash for the state machine, with
            // one difference: the durable record still looks healthy, so it
            // gets the short fixed failure text and the Resume can release.
            Err(error) if save_refused.get() => Err(stick_short_record(root, journal, error)),
            // A failed checkpoint leaves the record exactly as persisted.
            Err(error) if !durable.get() => Err(error),
            // A syscall failed while the source was untouched: withdraw the copy.
            Err(error) if rollbackable(file.phase) => {
                match crate::elastic_transfer::roll_back(source, target, &file) {
                    Ok(()) => {
                        let transfer = transfer_mut(journal)?;
                        transfer.current = None;
                        transfer.current_failed = None;
                        root.save(journal)?;
                        Ok(FileOutcome::RolledBack(error))
                    }
                    Err(rollback) => Err(stick_record(
                        root,
                        journal,
                        target,
                        array_lock,
                        &file,
                        format!("{error}; wycofanie nieudane: {rollback}"),
                    )),
                }
            }
            Err(error) => Err(stick_record(root, journal, target, array_lock, &file, error)),
        }
    }

    /// The files one walk wants to move, in move order: every "no" (eager)
    /// file, then every "yes" file at least `min_age_secs` old, oldest first.
    /// Pinned subtrees never reach this list. `min_free_pct` only triggers an
    /// extra run in the core and plays no part here.
    fn select_candidates(
        entries: Vec<ScanEntry>,
        rules: &MoverRules,
        now_ns: i128,
    ) -> (Vec<ScanFile>, Vec<(String, String)>) {
        let min_age_ns = i128::from(rules.min_age_secs) * 1_000_000_000;
        let mut eager = Vec::new();
        let mut aged = Vec::new();
        let mut refused = Vec::new();
        for entry in entries {
            match entry {
                ScanEntry::Refused { path, reason } => refused.push((path, reason)),
                ScanEntry::File(file)
                    if rules
                        .eager_folders
                        .iter()
                        .any(|folder| under_folder(&file.path, folder)) =>
                {
                    eager.push(file)
                }
                ScanEntry::File(file) if now_ns.saturating_sub(file.mtime_ns) >= min_age_ns => aged.push(file),
                ScanEntry::File(_) => {}
            }
        }
        aged.sort_by(|left, right| {
            left.mtime_ns
                .cmp(&right.mtime_ns)
                .then_with(|| left.path.cmp(&right.path))
        });
        eager.extend(aged);
        (eager, refused)
    }

    /// Another data branch holding the same path would shadow the moved file
    /// in the union, or be shadowed by it.
    fn shadowing_branch(env: &MoverEnv, target: &MoverTarget, path: &str) -> Option<String> {
        for other in env.data.iter().filter(|other| other.disk != target.disk) {
            match crate::elastic_transfer::entry_exists(&other.path, path) {
                Ok(false) => {}
                Ok(true) => return Some(format!("ścieżka istnieje już na branchu {}", other.disk)),
                Err(error) => return Some(format!("branch {}: {error}", other.disk)),
            }
        }
        None
    }

    /// The owed coupled Sync through the host's guard and argv. A failure
    /// before SnapRAID starts records no attempt; the declared Resume still
    /// releases the array with parity marked stale.
    fn run_owed_sync(
        root: &Root,
        journal: &mut Journal,
        request: &MoverRequest,
        host: &mut impl MoverHost,
    ) -> Result<(), String> {
        let (spec, program, args) = host.prepare_sync(root, journal)?;
        run_coupled_sync(
            root,
            journal,
            &spec,
            request.operation_id,
            |directory, label| capture_snapraid(root, request.array_lock, directory, label, &program, &args),
            |journal| host.sync_guard(root, journal),
        )
    }

    /// What one walk acts on, measured before anything is written: the
    /// candidates in move order, the refused entries, the descriptors other
    /// processes hold open and the target with its free space.
    struct Walk<'e> {
        candidates: Vec<ScanFile>,
        refused: Vec<(String, String)>,
        open: BTreeSet<(u64, u64)>,
        target: Option<(&'e MoverTarget, u64)>,
    }

    /// The read-only half of a walk. An error here stops the walk as a whole:
    /// the cache root, the open-file scan or a free-space probe failed. One
    /// unreadable directory or entry never does; it comes back refused.
    fn plan_walk<'e>(journal: &Journal, env: &'e MoverEnv<'_>, rules: &MoverRules) -> Result<Walk<'e>, String> {
        // lost+found belongs to the branch filesystem, not to the union's data.
        let entries = crate::elastic_transfer::scan_cache(&env.cache, |path| {
            path == "lost+found"
                || rules
                    .pinned_folders
                    .iter()
                    .any(|folder| under_folder(path, folder))
        })?;
        let open = if rules.skip_open_files {
            let daemon = journal
                .private
                .as_ref()
                .and_then(|private| private.anchor.as_ref())
                .map(|anchor| anchor.pid)
                .ok_or("skip_open_files wymaga kotwicy prywatnego mergerfs")?;
            (env.open_files)(daemon)?
        } else {
            BTreeSet::new()
        };
        let (candidates, refused) = select_candidates(entries, rules, env.now_ns);
        let target = if candidates.is_empty() {
            None
        } else {
            Some(match transfer_ref(journal)?.target.clone() {
                Some(disk) => {
                    let target = env
                        .data
                        .iter()
                        .find(|target| target.disk == disk)
                        .ok_or("utrwalony branch docelowy nie należy do macierzy")?;
                    (target, (env.space)(&target.path)?.1)
                }
                None => {
                    // MostFree among the data branches; equal free space goes
                    // to the lower disk_id.
                    let mut chosen: Option<(&MoverTarget, u64)> = None;
                    for target in &env.data {
                        let (_, free) = (env.space)(&target.path)?;
                        if chosen.is_none_or(|(best, best_free)| {
                            free > best_free || (free == best_free && target.disk_id < best.disk_id)
                        }) {
                            chosen = Some((target, free));
                        }
                    }
                    chosen.ok_or("macierz bez brancha data")?
                }
            })
        };
        Ok(Walk { candidates, refused, open, target })
    }

    /// The writing half of a walk: every candidate that passes its checks and
    /// fits is moved, one durable record at a time.
    fn move_walk(
        root: &Root,
        journal: &mut Journal,
        request: &MoverRequest,
        env: &MoverEnv,
        host: &mut impl MoverHost,
        walk: Walk<'_>,
    ) -> Result<(), String> {
        for (path, reason) in walk.refused {
            note_issue(transfer_mut(journal)?, &path, ElasticMoverIssueKind::Refused, &reason, 0);
        }
        let Some((target, free)) = walk.target else {
            return Ok(());
        };
        // A path a record stuck on stays where it is, and so does its copy on
        // the data branch: no later run touches either. The skip set answers
        // for every such path, those whose full record is no longer shown
        // included.
        let stuck: std::collections::BTreeMap<String, String> = journal
            .stuck_paths
            .iter()
            .map(|path| (path.path_sha256.clone(), path.operation_id.clone()))
            .collect();
        // Room is reserved only by a file that passed every other check,
        // oldest first; what does not fit waits for a later run.
        let mut room = free.saturating_sub(env.min_free_bytes);
        for file in walk.candidates {
            if let Some(operation) = stuck.get(&path_digest(&file.path)) {
                note_issue(
                    transfer_mut(journal)?,
                    &file.path,
                    ElasticMoverIssueKind::Refused,
                    &format!("rekord utknął w operacji {operation}; mover go nie rusza"),
                    0,
                );
                continue;
            }
            if walk.open.contains(&(file.device, file.inode)) {
                note_issue(
                    transfer_mut(journal)?,
                    &file.path,
                    ElasticMoverIssueKind::Skipped,
                    "plik otwarty przez inny proces",
                    file.size,
                );
                continue;
            }
            if let Some(reason) = shadowing_branch(env, target, &file.path) {
                note_issue(transfer_mut(journal)?, &file.path, ElasticMoverIssueKind::Refused, &reason, 0);
                continue;
            }
            let needed = file.allocated.max(file.size);
            if needed > room {
                note_issue(
                    transfer_mut(journal)?,
                    &file.path,
                    ElasticMoverIssueKind::Skipped,
                    "brak miejsca na branchu docelowym ponad minfreespace",
                    file.size,
                );
                continue;
            }
            // The name the journal will carry must stay inside the bound the
            // validators enforce; nothing reachable comes near it.
            let sequence = transfer_ref(journal)?.sequence;
            if sequence.to_string().len() > TEMPORARY_SEQUENCE_DIGITS {
                note_issue(
                    transfer_mut(journal)?,
                    &file.path,
                    ElasticMoverIssueKind::Refused,
                    "licznik operacji przekroczył długość nazwy tymczasowej",
                    0,
                );
                continue;
            }
            let temporary = format!(".tentanas-transfer-{}-{sequence}", request.operation_id);
            let planned = crate::elastic_transfer::plan_file(
                &env.cache,
                &target.path,
                &file.path,
                (file.device, file.inode),
                &temporary,
            )
            .and_then(|record| {
                if crate::elastic_transfer::worst_case_record_size(&record)? > TRANSFER_RECORD_LIMIT {
                    return Err("rekord pliku przekroczyłby limit dziennika".into());
                }
                // Everything this operation could still write for the file,
                // including the entry its Resume appends, must fit.
                if worst_case_journal_size(journal, &record)? > JOURNAL_LIMIT as usize {
                    return Err("dziennik z tym plikiem przekroczyłby limit odczytu".into());
                }
                Ok(record)
            });
            let record = match planned {
                Ok(record) => record,
                Err(reason) => {
                    note_issue(transfer_mut(journal)?, &file.path, ElasticMoverIssueKind::Refused, &reason, 0);
                    continue;
                }
            };
            room -= needed;
            let transfer = transfer_mut(journal)?;
            transfer.sequence = transfer
                .sequence
                .checked_add(1)
                .ok_or("przepełnienie licznika transferu")?;
            // The target becomes the operation's target with its first file.
            transfer.target.get_or_insert_with(|| target.disk.clone());
            transfer.current = Some(record.clone());
            root.save(journal)?;
            host.checkpoint(&record)?;
            if let FileOutcome::RolledBack(reason) =
                finish_file(root, journal, &env.cache, &target.path, request.array_lock, record, host)?
            {
                note_issue(
                    transfer_mut(journal)?,
                    &file.path,
                    ElasticMoverIssueKind::Refused,
                    &format!("wycofano kopię: {reason}"),
                    0,
                );
            }
        }
        Ok(())
    }

    /// One walk of the cache under the held Hold: finish the file in flight,
    /// move the selected files that fit the target and run the Sync owed. A
    /// walk that cannot go on is reported, and the Sync its operation owes
    /// still runs before the run stops.
    fn transfer_all(
        root: &Root,
        journal: &mut Journal,
        request: &MoverRequest,
        env: &MoverEnv,
        host: &mut impl MoverHost,
    ) -> Result<(), String> {
        let transfer = transfer_mut(journal)?;
        transfer.phase = ElasticMoverPhase::Moving;
        transfer.walked = false;
        transfer.skipped_files = 0;
        transfer.skipped_bytes = 0;
        transfer.refused_files = 0;
        transfer.issues.clear();
        transfer.detail = None;
        root.save(journal)?;
        if let Some(file) = transfer_ref(journal)?.current.clone() {
            let target = persisted_target(env, transfer_ref(journal)?)?;
            let path = file.source.clone();
            if let FileOutcome::RolledBack(reason) =
                finish_file(root, journal, &env.cache, &target, request.array_lock, file, host)?
            {
                note_issue(
                    transfer_mut(journal)?,
                    &path,
                    ElasticMoverIssueKind::Refused,
                    &format!("wycofano kopię: {reason}"),
                    0,
                );
            }
        }
        // The rules the operation announced: a restart runs under them
        // whatever its request carries.
        let rules = transfer_ref(journal)?.rules.clone();
        let walk_error = match plan_walk(journal, env, &rules) {
            Ok(walk) => {
                move_walk(root, journal, request, env, host, walk)?;
                None
            }
            Err(error) => {
                note_issue(
                    transfer_mut(journal)?,
                    ".",
                    ElasticMoverIssueKind::Refused,
                    &format!("przegląd cache przerwany: {error}"),
                    0,
                );
                Some(error)
            }
        };
        transfer_mut(journal)?.walked = walk_error.is_none();
        root.save(journal)?;
        if transfer_sync_owed(journal, transfer_ref(journal)?) {
            transfer_mut(journal)?.phase = ElasticMoverPhase::Syncing;
            root.save(journal)?;
            let synced = run_owed_sync(root, journal, request, host).and_then(|()| {
                if transfer_sync_owed(journal, transfer_ref(journal)?) {
                    return Err("sprzężony Sync nie potwierdził parity".to_string());
                }
                Ok(())
            });
            if let Err(error) = synced {
                return Err(match walk_error {
                    Some(walk) => format!("{walk}; {error}"),
                    None => error,
                });
            }
        }
        if let Some(error) = walk_error {
            return Err(error);
        }
        let transfer = transfer_mut(journal)?;
        transfer.phase = ElasticMoverPhase::Complete;
        transfer.finished_at = Some(timestamp()?);
        journal.detail = Some("service Hold: mover zakończony, wymagany Resume".into());
        root.save(journal)
    }

    /// Restore's mount plan and executor after a fresh boot, stopped before
    /// publishing: the branches and a new private union come back, RO.
    fn recover_private_mounts(
        root: &Root,
        journal: &mut Journal,
        array_lock: &File,
        boot: &str,
        system: &mut dyn StepSystem,
    ) -> Result<(), String> {
        let spec = layout_with(&journal.spec, |disk| system.device(disk).map(|device| device.path))?;
        for role in roles(&journal.spec) {
            let device = system.device(role_disk(&journal.spec, role)?)?;
            system.filesystem_matches(&journal.spec, role, &device)?;
        }
        if spec.has_parity() {
            system.config_matches(&spec, root.uid)?;
        }
        let private = journal.private.as_mut().ok_or("brak prywatnej topologii")?;
        private.anchor = None;
        private.published = false;
        journal.boot_id = boot.into();
        journal.stage = ElasticStage::Mounting;
        root.save(journal)?;
        let mut observed = Observed::nothing_mounted();
        for role in roles(&journal.spec) {
            let device = system.device(role_disk(&journal.spec, role)?)?;
            if system.branch_mounted(&journal.spec, role, &device)? {
                observed.mounted.insert(mount_path(&journal.spec, role), true);
            }
        }
        if system.union_mounted(&spec)? {
            observed.mounted.insert(spec.union_path(), true);
        }
        let tools = system.tools(&spec)?;
        let steps = plan_mount(&spec, &observed, &tools).map_err(|e| e.to_string())?;
        execute_steps(root, array_lock, journal, &spec, steps, StepMode::Recover, system)?;
        system.set_union_readonly(true)
    }

    /// A fresh boot left the last durable state of an interrupted run: close
    /// whatever was running as interrupted, forget mount intents the boot
    /// made void, remount privately without publishing and stand the Hold up
    /// again on the new private union.
    fn recover_after_boot(
        root: &Root,
        journal: &mut Journal,
        request: &MoverRequest,
        env: &MoverEnv,
        host: &mut impl MoverHost,
    ) -> Result<(), String> {
        let mut recovered = journal.clone();
        close_interrupted_sync(&mut recovered, request.operation_id, "Sync przerwany zmianą boot")?;
        // A new boot took every earlier mount with it: an intent to mount a
        // branch, the union or the config describes nothing that still exists.
        if matches!(
            recovered.pending,
            Some(Pending::Mount(_) | Pending::Union | Pending::Config)
        ) {
            recovered.pending = None;
        }
        if recovered.pending.is_some() {
            return Err("po zmianie boot pozostał obcy pending".into());
        }
        let transfer = transfer_mut(&mut recovered)?;
        if transfer.phase != ElasticMoverPhase::NeedsAttention {
            transfer.phase = ElasticMoverPhase::NeedsAttention;
            transfer.detail = Some("transfer przerwany zmianą boot".into());
        }
        recovered.stage = ElasticStage::NeedsAttention;
        root.save(&recovered)?;
        *journal = recovered;
        recover_private_mounts(root, journal, request.array_lock, &env.boot_id, host.steps())?;
        let private = journal.private.as_ref().ok_or("brak prywatnej topologii")?;
        if journal.boot_id != env.boot_id
            || private.published
            || !private.anchor.as_ref().is_some_and(|anchor| anchor.boot_id == env.boot_id)
        {
            return Err("odtworzenie po zmianie boot bez prywatnej, niepublikowanej unii".into());
        }
        if !host.steps().union_readonly()? {
            return Err("odtworzona unia nie jest RO".into());
        }
        // Recover mode never publishes, so nothing else clears the union
        // intent: the private union is confirmed here, whatever ran the steps.
        match journal.pending {
            None | Some(Pending::Union) => journal.pending = None,
            Some(_) => return Err("odtworzenie zostawiło niedokończony krok montowania".into()),
        }
        // The new private union is RO: the Hold barrier stands again.
        journal
            .private
            .as_mut()
            .and_then(|private| private.service.as_mut())
            .ok_or("brak stanu service")?
            .pending = false;
        journal.stage = ElasticStage::NeedsAttention;
        root.save(journal)
    }

    /// The whole mover operation under the one held array lock: Hold (or its
    /// recovery after a boot), the per-file transfer, the coupled Sync and
    /// Complete. Everything beyond the branches goes through `host`.
    fn run_mover(
        root: &Root,
        journal: &mut Journal,
        request: &MoverRequest,
        env: &MoverEnv,
        host: &mut impl MoverHost,
    ) -> Result<(), String> {
        request.rules.validate().map_err(|error| error.to_string())?;
        validate_elastic_uuid(request.operation_id).map_err(|e| e.to_string())?;
        validate_elastic_uuid(request.resume_operation_id).map_err(|e| e.to_string())?;
        if request.operation_id == journal.spec.operation_id
            || request.resume_operation_id == journal.spec.operation_id
            || request.operation_id == request.resume_operation_id
        {
            return Err("nieprawidłowy UUID movera".into());
        }
        if journal.spec.cache.is_none() {
            return Err("macierz nie ma cache".into());
        }
        // A restart is the same operation under the same Resume. It runs with
        // the rules and coupling that operation announced, so a core that lost
        // its own intent can still finish it.
        let restarting = journal.transfer.as_ref().is_some_and(|transfer| {
            transfer.operation_id == request.operation_id
                && transfer.resume_operation_id == request.resume_operation_id
        });
        if journal
            .transfer
            .as_ref()
            .is_some_and(|transfer| transfer.finished_at.is_none())
            && !restarting
        {
            return Err("unfinished transfer wymaga tego samego operation_id i resume_operation_id".into());
        }
        let fresh = journal.boot_id != env.boot_id;
        let owned_sync = Pending::Maintenance {
            operation_id: request.operation_id.to_string(),
            kind: ElasticSnapraidKind::Sync,
        };
        if journal.pending.as_ref().is_some_and(|pending| {
            !(restarting
                && (*pending == owned_sync
                    || (fresh && matches!(pending, Pending::Mount(_) | Pending::Union | Pending::Config))))
        }) {
            return Err("mover wymaga braku zwykłego pending".into());
        }
        let hold = journal
            .private
            .as_ref()
            .and_then(|private| private.service.as_ref())
            .filter(|service| service.mode == ElasticServiceMode::Hold)
            .cloned();
        let array_id = journal.spec.array_id.clone();
        if restarting {
            let transfer = transfer_ref(journal)?;
            if transfer.phase == ElasticMoverPhase::Complete {
                return Ok(());
            }
            if transfer.finished_at.is_some() {
                return Err("operacja movera została już zamknięta".into());
            }
            if !hold.is_some_and(|service| {
                service.operation_id == request.operation_id && (fresh || !service.pending)
            }) {
                return Err("wznowienie transferu wymaga potwierdzonego service Hold tej operacji".into());
            }
            if fresh {
                if let Err(error) = recover_after_boot(root, journal, request, env, host) {
                    let error = mark_attention(root, &array_id, request.operation_id, error);
                    if let Ok(stored) = root.load(&array_id) {
                        *journal = stored;
                    }
                    return Err(error);
                }
            }
            host.verify_branches(journal)?;
            // The Hold barrier must still stand before anything moves.
            if !host.steps().union_readonly()? {
                return Err("wznowienie transferu wymaga unii potwierdzonej jako RO".into());
            }
        } else {
            if fresh {
                return Err("po zmianie boot mover wznawia wyłącznie własny niedokończony transfer".into());
            }
            if hold.is_some() {
                return Err("mover wymaga Online przed rozpoczęciem Hold".into());
            }
            host.verify_branches(journal)?;
            let mut announced = journal.clone();
            announced.transfer = Some(TransferJournal {
                operation_id: request.operation_id.into(),
                resume_operation_id: request.resume_operation_id.into(),
                started_at: timestamp()?,
                finished_at: None,
                rules: request.rules.clone(),
                coupled_sync: request.coupled_sync,
                coupled_sync_result: None,
                sync_attempt: 0,
                phase: ElasticMoverPhase::Holding,
                target: None,
                sequence: 0,
                current: None,
                evicted: 0,
                current_failed: None,
                moved_files: 0,
                moved_bytes: 0,
                skipped_files: 0,
                skipped_bytes: 0,
                refused_files: 0,
                issues: Vec::new(),
                walked: false,
                detail: None,
            });
            // The transfer is announced in the same durable write as the Hold
            // intent, before the union goes RO.
            if let Err(error) = enter_service_with(
                root,
                announced,
                request.operation_id,
                request.array_lock,
                || host.steps().set_union_readonly(true),
            ) {
                return Err(mark_attention(root, &array_id, request.operation_id, error));
            }
            *journal = root.load(&array_id)?;
        }
        if let Err(error) = transfer_all(root, journal, request, env, host) {
            let error = mark_attention(root, &array_id, request.operation_id, error);
            if let Ok(stored) = root.load(&array_id) {
                *journal = stored;
            }
            return Err(error);
        }
        Ok(())
    }

    fn begin_coupled_sync(root: &Root, journal: &mut Journal, run_record: &ElasticSnapraidRun) -> Result<u64, String> {
        let mut prepared = journal.clone();
        let transfer = prepared.transfer.as_ref().ok_or("brak transferu")?;
        let owned = Pending::Maintenance {
            operation_id: run_record.operation_id.clone(),
            kind: ElasticSnapraidKind::Sync,
        };
        if transfer.operation_id != run_record.operation_id
            || run_record.kind != ElasticSnapraidKind::Sync
            || run_record.outcome != ElasticSnapraidOutcome::Running
            || transfer.current.is_some()
            // Never overwrite another operation's intent.
            || prepared.pending.as_ref().is_some_and(|pending| *pending != owned)
            || prepared.private.as_ref().and_then(|private| private.service.as_ref())
                .is_none_or(|service| service.mode != ElasticServiceMode::Hold)
        {
            return Err("coupled Sync wymaga własnego transferu w Hold po zakończeniu plików".into());
        }
        let attempt = transfer.sync_attempt
            .checked_add(1).ok_or("przepełnienie licznika prób sync")?;
        prepared.transfer.as_mut().ok_or("brak transferu")?.phase = ElasticMoverPhase::Syncing;
        prepared.transfer.as_mut().ok_or("brak transferu")?.coupled_sync_result = Some(run_record.clone());
        prepared.transfer.as_mut().ok_or("brak transferu")?.sync_attempt = attempt;
        prepared.last_run = Some(run_record.clone());
        prepared.pending = Some(owned);
        prepared.stage = ElasticStage::SyncPending;
        root.save(&prepared)?;
        *journal = prepared;
        Ok(attempt)
    }

    /// The Sync a mover owes, as one more attempt of the same operation. It
    /// runs only with parity and never after this operation confirmed one.
    fn run_coupled_sync(
        root: &Root,
        journal: &mut Journal,
        spec: &ElasticSpec,
        operation_id: &str,
        process: impl FnOnce(&Path, &str) -> Result<(Option<i32>, Result<SnapraidLog, String>), String>,
        post_guard: impl FnOnce(&Journal) -> Result<bool, String>,
    ) -> Result<(), String> {
        if spec.parity.is_empty()
            || journal
                .transfer
                .as_ref()
                .and_then(|transfer| transfer.coupled_sync_result.as_ref())
                .is_some_and(|run| coupled_sync_success(run, operation_id))
        {
            return Ok(());
        }
        // The run directory first: a failure here leaves no attempt behind.
        let runs = root.path.join(format!("{}.runs", journal.spec.array_id));
        directory(&runs, true, root.uid)?;
        let operation = runs.join(format!("mover-{operation_id}"));
        directory(&operation, true, root.uid)?;
        File::open(&runs)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        let run_record = ElasticSnapraidRun {
            operation_id: operation_id.to_string(),
            kind: ElasticSnapraidKind::Sync,
            started_at: timestamp()?,
            finished_at: None,
            outcome: ElasticSnapraidOutcome::Running,
            exit_code: None,
            total_blocks: None,
            checked_blocks: None,
            accessed_mb: None,
            errors_file: None,
            errors_io: None,
            errors_data: None,
            detail: None,
        };
        let attempt = begin_coupled_sync(root, journal, &run_record)?;
        let label = format!("sync-{attempt}");
        let guard_journal = journal.clone();
        match perform_maintenance(
            root,
            journal,
            run_record,
            spec,
            || process(&operation, &label),
            || post_guard(&guard_journal),
            true,
        ) {
            Ok(run) if run.outcome == ElasticSnapraidOutcome::Succeeded => Ok(()),
            Ok(run) => Err(run.detail.unwrap_or_else(|| "proces SnapRAID nie zakończył się sukcesem".into())),
            Err(error) => {
                // An attempt recorded as Running must not outlive this call.
                let array_id = journal.spec.array_id.clone();
                let error = close_attempt(root, &array_id, operation_id, error);
                if let Ok(stored) = root.load(&array_id) {
                    *journal = stored;
                }
                Err(error)
            }
        }
    }

    /// The production host: the private worker of this invocation.
    struct WorkerHost<'a, 'w> {
        steps: LiveSteps<'a, 'w>,
    }

    impl MoverHost for WorkerHost<'_, '_> {
        fn steps(&mut self) -> &mut dyn StepSystem {
            &mut self.steps
        }

        fn verify_branches(&mut self, journal: &Journal) -> Result<(), String> {
            // An unmounted mountpoint would hand the walk an empty directory
            // of the root filesystem.
            let devices = inventory()?;
            for role in roles(&journal.spec) {
                if matches!(role, ElasticRole::Parity(_)) {
                    continue;
                }
                let device = resolve(role_disk(&journal.spec, role)?, &devices)?;
                filesystem_matches(&journal.spec, role, device)?;
                if !branch_mounted(&journal.spec, role, device)? {
                    return Err("mover wymaga zamontowanych branchy macierzy".into());
                }
            }
            Ok(())
        }

        fn prepare_sync(&mut self, root: &Root, journal: &Journal) -> Result<(ElasticSpec, String, Vec<String>), String> {
            maintenance_guard(root, journal)?;
            let spec = layout(&journal.spec, &inventory()?)?;
            let tools = Tools::resolve(&spec).map_err(|e| e.to_string())?;
            let args = snapraid_args(&spec, &SnapraidAction::Sync).map_err(|e| e.to_string())?;
            Ok((spec, tools.snapraid.clone(), args))
        }

        fn sync_guard(&mut self, root: &Root, journal: &Journal) -> Result<bool, String> {
            maintenance_guard(root, journal).map(|(_, empty)| empty)
        }
    }

    fn mover(
        root: &Root,
        journal: &mut Journal,
        request: &MoverRequest,
        worker: &mut Worker<'_>,
    ) -> Result<ElasticMoverResult, String> {
        let spec = layout(&journal.spec, &inventory()?)?;
        let cache = spec.cache.first().ok_or("macierz nie ma cache")?;
        let space = |path: &Path| -> Result<(u64, u64), String> {
            let (size, _, free) = capacity(path.to_str().ok_or("ścieżka brancha nie jest UTF-8")?)?;
            Ok((size, free))
        };
        let env = MoverEnv {
            cache: PathBuf::from(cache_branch_path(&spec.name, &cache.disk)),
            data: spec
                .data
                .iter()
                .zip(&journal.spec.data)
                .map(|(branch, disk)| MoverTarget {
                    disk: branch.disk.clone(),
                    disk_id: disk.disk_id.clone(),
                    path: PathBuf::from(data_branch_path(&spec.name, &branch.disk)),
                })
                .collect(),
            min_free_bytes: min_free_space_bytes(&spec)?,
            space: &space,
            open_files: &crate::elastic_transfer::open_file_identities,
            now_ns: now_ns()?,
            boot_id: boot_id()?,
        };
        let mut host = WorkerHost {
            steps: LiveSteps { worker: Some(worker) },
        };
        run_mover(root, journal, request, &env, &mut host)?;
        let state = observe(journal, host.steps.worker.as_deref());
        let run = state.last_mover.clone().ok_or("brak wyniku movera")?;
        Ok(ElasticMoverResult { state, run })
    }

    fn timestamp() -> Result<String, String> {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs() as libc::time_t;
        let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
        if unsafe { libc::gmtime_r(&seconds, time.as_mut_ptr()) }.is_null() {
            return Err("brak czasu UTC".into());
        }
        let time = unsafe { time.assume_init() };
        Ok(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            time.tm_year + 1900,
            time.tm_mon + 1,
            time.tm_mday,
            time.tm_hour,
            time.tm_min,
            time.tm_sec
        ))
    }

    fn valid_timestamp(value: &str) -> bool {
        let bytes = value.as_bytes();
        if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
            return false;
        }
        if (0..20).any(|index| ![4, 7, 10, 13, 16, 19].contains(&index) && !bytes[index].is_ascii_digit()) {
            return false;
        }
        let number = |start: usize, end: usize| value[start..end].parse::<u32>().ok();
        let (year, month, day, hour, minute, second) = match (number(0, 4), number(5, 7), number(8, 10), number(11, 13), number(14, 16), number(17, 19)) {
            (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) => (year, month, day, hour, minute, second),
            _ => return false,
        };
        if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 { return false; }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        day >= 1 && day <= days[(month - 1) as usize]
    }

    fn coupled_sync_success(run: &ElasticSnapraidRun, operation_id: &str) -> bool {
        run.operation_id == operation_id
            && run.kind == ElasticSnapraidKind::Sync
            && run.outcome == ElasticSnapraidOutcome::Succeeded
            && run.finished_at.as_ref().is_some_and(|finished| valid_timestamp(finished) && valid_timestamp(&run.started_at) && finished.as_str() >= run.started_at.as_str())
            && run.exit_code == Some(0)
            && !run.errors_file.is_some_and(|value| value > 0)
            && !run.errors_io.is_some_and(|value| value > 0)
            && !run.errors_data.is_some_and(|value| value > 0)
    }

    fn plan_role(plan: &ElasticSpec, device: &str) -> Result<ElasticRole, String> {
        if let Some(index) = plan.data.iter().position(|d| d.device == device) {
            return Ok(ElasticRole::Data((index + 1) as u16));
        }
        if plan.cache.iter().any(|d| d.device == device) {
            return Ok(ElasticRole::Cache);
        }
        plan.parity
            .iter()
            .find(|d| d.device == device)
            .map(|d| ElasticRole::Parity(d.index))
            .ok_or_else(|| "urządzenie planu poza specyfikacją".into())
    }

    /// What one `execute_steps` call may do beyond mounting.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum StepMode {
        /// Create: may format and run the first Sync; publishes the union.
        Create,
        /// Restore: never formats; publishes the union.
        Restore,
        /// Mover recovery after a boot: never formats and never publishes;
        /// the private union waits under Hold for the declared Resume.
        Recover,
    }

    /// The effects below `execute_steps`: devices, filesystems, mounts, tools
    /// and the private mergerfs. The executor keeps every pending intent,
    /// save and mode decision; tests replace only this boundary.
    trait StepSystem {
        /// The array disk as a present block device.
        fn device(&mut self, disk: &ElasticDiskSpec) -> Result<Device, String>;
        fn filesystem_matches(
            &mut self,
            spec: &ElasticCreateSpec,
            role: ElasticRole,
            device: &Device,
        ) -> Result<(), String>;
        fn branch_mounted(
            &mut self,
            spec: &ElasticCreateSpec,
            role: ElasticRole,
            device: &Device,
        ) -> Result<bool, String>;
        fn union_mounted(&mut self, spec: &ElasticSpec) -> Result<bool, String>;
        fn directory_empty(&mut self, path: &str) -> Result<bool, String>;
        fn make_directory(&mut self, path: &str, uid: u32) -> Result<(), String>;
        fn clean_device(&mut self, device: &Device) -> Result<(), String>;
        fn mount(
            &mut self,
            filesystem: &str,
            options: &[String],
            device: &Device,
            mountpoint: &str,
            locks: &[RawFd],
        ) -> Result<(), String>;
        fn run_tool(&mut self, program: &Path, args: &[String], locks: &[RawFd]) -> Result<(), String>;
        fn write_file(&mut self, path: &str, content: &str, uid: u32) -> Result<(), String>;
        /// Whether a private worker hosts the union.
        fn has_worker(&self) -> bool;
        fn start_mergerfs(&mut self, program: &Path, args: &[String], log: &File) -> Result<Anchor, String>;
        fn publish(&mut self, anchor: &Anchor) -> Result<(), String>;
        fn set_union_readonly(&mut self, readonly: bool) -> Result<(), String>;
        fn union_readonly(&mut self) -> Result<bool, String>;
        fn config_matches(&mut self, spec: &ElasticSpec, uid: u32) -> Result<(), String>;
        fn tools(&mut self, spec: &ElasticSpec) -> Result<Tools, String>;
    }

    /// The live system: real devices and tools, and the private worker if any.
    struct LiveSteps<'a, 'w> {
        worker: Option<&'a mut Worker<'w>>,
    }

    impl StepSystem for LiveSteps<'_, '_> {
        fn device(&mut self, disk: &ElasticDiskSpec) -> Result<Device, String> {
            let devices = inventory()?;
            resolve(disk, &devices).cloned()
        }

        fn filesystem_matches(
            &mut self,
            spec: &ElasticCreateSpec,
            role: ElasticRole,
            device: &Device,
        ) -> Result<(), String> {
            filesystem_matches(spec, role, device)
        }

        fn branch_mounted(
            &mut self,
            spec: &ElasticCreateSpec,
            role: ElasticRole,
            device: &Device,
        ) -> Result<bool, String> {
            branch_mounted(spec, role, device)
        }

        fn union_mounted(&mut self, spec: &ElasticSpec) -> Result<bool, String> {
            union_mounted(spec)
        }

        fn directory_empty(&mut self, path: &str) -> Result<bool, String> {
            Ok(std::fs::read_dir(path).map_err(|e| e.to_string())?.next().is_none())
        }

        fn make_directory(&mut self, path: &str, uid: u32) -> Result<(), String> {
            if self.worker.is_some() && Path::new(path).starts_with(BRANCH_ROOT) {
                private_directory(Path::new(BRANCH_ROOT), Path::new(path), uid)
            } else {
                directory(Path::new(path), false, uid)
            }
        }

        fn clean_device(&mut self, device: &Device) -> Result<(), String> {
            clean_device(device)
        }

        fn mount(
            &mut self,
            filesystem: &str,
            options: &[String],
            device: &Device,
            mountpoint: &str,
            locks: &[RawFd],
        ) -> Result<(), String> {
            success(run(
                &tool(&["/usr/bin/mount", "/bin/mount"])?,
                &[
                    "-t".into(),
                    filesystem.into(),
                    "-o".into(),
                    options.join(","),
                    device.path.clone(),
                    mountpoint.into(),
                ],
                None,
                locks,
            )?)
            .map(|_| ())
        }

        fn run_tool(&mut self, program: &Path, args: &[String], locks: &[RawFd]) -> Result<(), String> {
            success(run(program, args, None, locks)?).map(|_| ())
        }

        fn write_file(&mut self, path: &str, content: &str, uid: u32) -> Result<(), String> {
            atomic_write(Path::new(path), content.as_bytes(), uid)
        }

        fn has_worker(&self) -> bool {
            self.worker.is_some()
        }

        fn start_mergerfs(&mut self, program: &Path, args: &[String], log: &File) -> Result<Anchor, String> {
            self.worker
                .as_deref_mut()
                .ok_or("brak prywatnego wykonawcy")?
                .start_mergerfs(program, args, log)
        }

        fn publish(&mut self, anchor: &Anchor) -> Result<(), String> {
            self.worker
                .as_deref_mut()
                .ok_or("brak prywatnego wykonawcy")?
                .publish(anchor)
                .map(|_| ())
        }

        fn set_union_readonly(&mut self, readonly: bool) -> Result<(), String> {
            self.worker
                .as_deref_mut()
                .ok_or("brak prywatnego wykonawcy")?
                .set_union_readonly(readonly)
        }

        fn union_readonly(&mut self) -> Result<bool, String> {
            self.worker
                .as_deref()
                .ok_or("brak prywatnego wykonawcy")?
                .union_readonly()
        }

        fn config_matches(&mut self, spec: &ElasticSpec, uid: u32) -> Result<(), String> {
            let mut content = String::new();
            private_file(Path::new(&spec.config_path()), uid)?
                .read_to_string(&mut content)
                .map_err(|e| e.to_string())?;
            if content != snapraid_config(spec).map_err(|e| e.to_string())? {
                return Err("konfiguracja niezgodna z journalem".into());
            }
            Ok(())
        }

        fn tools(&mut self, spec: &ElasticSpec) -> Result<Tools, String> {
            Tools::resolve(spec).map_err(|e| e.to_string())
        }
    }

    fn execute_steps(
        root: &Root,
        array_lock: &File,
        journal: &mut Journal,
        plan: &ElasticSpec,
        steps: Vec<ElasticStep>,
        mode: StepMode,
        system: &mut dyn StepSystem,
    ) -> Result<(), String> {
        // Recovery stands its union up in a private namespace; without a
        // worker the executor would mount it in the host namespace, public.
        if mode == StepMode::Recover && !system.has_worker() {
            return Err("odtworzenie wymaga prywatnego wykonawcy".into());
        }
        let locks = [root.node_lock.as_raw_fd(), array_lock.as_raw_fd()];
        for step in steps {
            match step {
                ElasticStep::Note(_) => (),
                ElasticStep::Mkdir { path } => system.make_directory(&path, root.uid)?,
                ElasticStep::Mkfs {
                    program,
                    device,
                    filesystem,
                    label,
                } => {
                    if mode != StepMode::Create {
                        return Err("restore nie może formatować".into());
                    }
                    let role = plan_role(plan, &device)?;
                    if journal.formatted.contains(&role) || journal.pending.is_some() {
                        return Err("formatowanie już rozpoczęte".into());
                    }
                    let disk = role_disk(&journal.spec, role)?.clone();
                    let current = system.device(&disk)?;
                    system.clean_device(&current)?;
                    let expected_uuid = disk.expected_uuid.clone();
                    let args = if filesystem == "ext4" {
                        vec![
                            "-F".into(),
                            "-U".into(),
                            expected_uuid,
                            "-L".into(),
                            label,
                            current.path.clone(),
                        ]
                    } else {
                        vec![
                            "-f".into(),
                            "-m".into(),
                            format!("uuid={expected_uuid}"),
                            "-L".into(),
                            label,
                            current.path.clone(),
                        ]
                    };
                    pending_operation(
                        journal,
                        Pending::Format(role),
                        ElasticStage::Formatting,
                        |journal| root.save(journal),
                        || system.run_tool(Path::new(&program), &args, &locks),
                    )?;
                    let refreshed = system.device(&disk)?;
                    system.filesystem_matches(&journal.spec, role, &refreshed)?;
                    journal.formatted.push(role);
                    journal.pending = None;
                    root.save(journal)?;
                }
                ElasticStep::Mount {
                    source,
                    mountpoint,
                    filesystem,
                    options,
                } => {
                    let role = plan_role(plan, &source)?;
                    if mountpoint != mount_path(&journal.spec, role) {
                        return Err("mountpoint niezgodny z rolą".into());
                    }
                    let device = system.device(role_disk(&journal.spec, role)?)?;
                    system.filesystem_matches(&journal.spec, role, &device)?;
                    if system.branch_mounted(&journal.spec, role, &device)? {
                        continue;
                    }
                    if !system.directory_empty(&mountpoint)? {
                        return Err("katalog brancha nie jest pusty".into());
                    }
                    pending_operation(
                        journal,
                        Pending::Mount(role),
                        ElasticStage::Mounting,
                        |journal| root.save(journal),
                        || system.mount(&filesystem, &options, &device, &mountpoint, &locks),
                    )?;
                    if !system.branch_mounted(&journal.spec, role, &device)? {
                        return Err("brak mount po wykonaniu".into());
                    }
                    journal.pending = None;
                    root.save(journal)?;
                }
                ElasticStep::WriteFile { path, content, .. } => {
                    pending_operation(
                        journal,
                        Pending::Config,
                        ElasticStage::Mounting,
                        |journal| root.save(journal),
                        || system.write_file(&path, &content, root.uid),
                    )?;
                    journal.pending = None;
                    root.save(journal)?;
                }
                ElasticStep::MergerfsMount {
                    program,
                    branches,
                    mountpoint,
                    options,
                } => {
                    for role in roles(&journal.spec) {
                        let device = system.device(role_disk(&journal.spec, role)?)?;
                        system.filesystem_matches(&journal.spec, role, &device)?;
                        if !system.branch_mounted(&journal.spec, role, &device)? {
                            return Err("brak potwierdzonego brancha".into());
                        }
                    }
                    if system.union_mounted(plan)? {
                        continue;
                    }
                    if !system.directory_empty(&mountpoint)? {
                        return Err("katalog unii nie jest pusty".into());
                    }
                    let args = vec!["-o".into(), options.join(","), branches.join(":"), mountpoint];
                    if system.has_worker() {
                        journal.pending = Some(Pending::Union);
                        journal.stage = ElasticStage::Mounting;
                        root.save(journal)?;
                        let log_path = root.path.join(format!(
                            "{}.mergerfs-{}-{}.log",
                            journal.spec.array_id,
                            std::process::id(),
                            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
                        ));
                        let log = OpenOptions::new()
                            .write(true).create_new(true).mode(0o600)
                            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                            .open(log_path).map_err(|e| e.to_string())?;
                        File::open(&root.path).and_then(|file| file.sync_all()).map_err(|e| e.to_string())?;
                        let anchor = system.start_mergerfs(Path::new(&program), &args, &log)?;
                        let private = journal.private.as_mut().ok_or("worker bez private journala")?;
                        private.anchor = Some(anchor);
                        private.published = false;
                        root.save(journal)?;
                        if !system.union_mounted(plan)? {
                            return Err("brak potwierdzonej prywatnej unii".into());
                        }
                        // Recovery stops here: its caller confirms the private
                        // union and clears the union intent without publishing.
                        if mode != StepMode::Recover {
                            publish_private(root, journal, |anchor| system.publish(anchor))?;
                        }
                        continue;
                    }
                    // Demon FUSE nie dziedziczy blokad krótkich operacji dyskowych.
                    pending_operation(
                        journal,
                        Pending::Union,
                        ElasticStage::Mounting,
                        |journal| root.save(journal),
                        || system.run_tool(Path::new(&program), &args, &[]),
                    )?;
                    if !system.union_mounted(plan)? {
                        return Err("brak potwierdzonej unii".into());
                    }
                    journal.pending = None;
                    root.save(journal)?;
                }
                ElasticStep::Run { program, args } => {
                    if mode != StepMode::Create || journal.spec.parity.is_empty() {
                        return Err("nieoczekiwany sync".into());
                    }
                    pending_operation(
                        journal,
                        Pending::Sync,
                        ElasticStage::SyncPending,
                        |journal| root.save(journal),
                        || system.run_tool(Path::new(&program), &args, &locks),
                    )?;
                    journal.sync_completed_at = Some(timestamp()?);
                    journal.pending = None;
                    root.save(journal)?;
                }
                _ => return Err("niedozwolony krok wykonawcy create/restore".into()),
            }
        }
        Ok(())
    }

    fn private_directory(base: &Path, path: &Path, uid: u32) -> Result<(), String> {
        let relative = path
            .strip_prefix(base)
            .map_err(|_| "katalog poza prywatnym overlay")?;
        directory(base, false, uid)?;
        let overlay_device = std::fs::metadata(base).map_err(|e| e.to_string())?.dev();
        let mut current = base.to_path_buf();
        for component in relative.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err("nieprawidłowy katalog prywatnego brancha".into());
            }
            current.push(component);
            let created = match std::fs::DirBuilder::new().mode(0o711).create(&current) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(error) => return Err(error.to_string()),
            };
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                .open(&current)
                .map_err(|e| e.to_string())?;
            let metadata = directory.metadata().map_err(|e| e.to_string())?;
            if metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
                return Err("obcy katalog prywatnego brancha".into());
            }
            if created {
                if unsafe { libc::fchmod(directory.as_raw_fd(), 0o711) } != 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                directory.sync_all().map_err(|e| e.to_string())?;
                File::open(current.parent().ok_or("brak rodzica brancha")?)
                    .and_then(|file| file.sync_all())
                    .map_err(|e| e.to_string())?;
            } else if metadata.dev() == overlay_device && metadata.mode() & 0o111 != 0o111 {
                return Err("nietrawersowalny katalog w prywatnym overlay".into());
            }
        }
        Ok(())
    }

    fn finish(
        root: &Root,
        journal: &mut Journal,
        outcome: Result<(), String>,
        worker: Option<&Worker>,
    ) -> Result<ElasticResult, String> {
        match outcome {
            Ok(()) => {
                let mut completed = journal.clone();
                let mut evicted = None;
                if let Some(service) = completed.private.as_ref().and_then(|private| private.service.as_ref()) {
                    if service.mode != ElasticServiceMode::Online {
                        return Err("service Hold nie może zakończyć się jako Ready".into());
                    }
                    let worker = worker.ok_or("service Online wymaga workera")?;
                    if worker.public_mount().is_none() || worker.union_readonly()? {
                        return Err("service Online bez potwierdzonej publikacji RW".into());
                    }
                    let service_operation_id = service.operation_id.clone();
                    completed.private.as_mut().ok_or("brak prywatnej topologii")?.service.as_mut()
                        .ok_or("brak stanu service")?.pending = false;
                    evicted = close_transfer(&mut completed, &service_operation_id)?;
                }
                completed.stage = ElasticStage::Ready;
                completed.detail = None;
                root.save(&completed)?;
                // Only now: a refused save drops the clone, and an eviction
                // that never reached the disk must not appear in the log.
                if let Some(dropped) = evicted {
                    log_eviction(&dropped);
                }
                *journal = completed;
            }
            Err(error) => {
                let mut failed = journal.clone();
                failed.stage = ElasticStage::NeedsAttention;
                failed.detail = Some(bounded_text(&error, TRANSFER_DETAIL_LIMIT));
                root.save(&failed)?;
                *journal = failed;
            }
        }
        Ok(observe(journal, worker))
    }

    fn publish_private(
        root: &Root,
        journal: &mut Journal,
        publish: impl FnOnce(&Anchor) -> Result<(), String>,
    ) -> Result<(), String> {
        let private = journal.private.as_mut().ok_or("brak private journala")?;
        let anchor = private.anchor.clone().ok_or("brak trwałej kotwicy")?;
        private.published = false;
        journal.pending = Some(Pending::Union);
        journal.stage = ElasticStage::Mounting;
        root.save(journal)?;
        publish(&anchor)?;
        let mut completed = journal.clone();
        completed
            .private
            .as_mut()
            .ok_or("brak private journala")?
            .published = true;
        completed.pending = None;
        root.save(&completed)?;
        *journal = completed;
        Ok(())
    }

    #[derive(Default)]
    struct SnapraidLog {
        fields: BTreeMap<String, Vec<String>>,
        progress: Vec<(u8, u64)>,
        nothing: usize,
        nothing_status: usize,
        clean: usize,
        error: bool,
    }

    fn read_lines(
        file: File,
        mut consume: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut reader = BufReader::new(file);
        loop {
            let mut bytes = Vec::new();
            let count = reader
                .by_ref()
                .take(1024 * 1024 + 1)
                .read_until(b'\n', &mut bytes)
                .map_err(|e| e.to_string())?;
            if count == 0 {
                return Ok(());
            }
            if count > 1024 * 1024 {
                return Err("zbyt długa linia logu".into());
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| "log nie jest UTF-8")?;
            for line in text.trim_end_matches('\n').split('\r') {
                if !line.is_empty() {
                    consume(line)?;
                }
            }
        }
    }

    impl SnapraidLog {
        fn field(&self, key: &str) -> &[String] {
            self.fields.get(key).map(Vec::as_slice).unwrap_or(&[])
        }

        fn number(&self, key: &str) -> Result<Option<u64>, String> {
            match self.field(key) {
                [] => Ok(None),
                [value] => value
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("nieprawidłowy licznik {key}")),
                _ => Err(format!("powtórzony licznik {key}")),
            }
        }

        fn parse(log: File, stdout: File) -> Result<Self, String> {
            let mut result = Self::default();
            read_lines(log, |line| {
                if line == "msg:status: Nothing to do" {
                    result.nothing_status += 1;
                }
                let prefix = if line.starts_with("summary:") {
                    "summary:"
                } else if line.starts_with("conf:file:") {
                    "conf:file:"
                } else {
                    ""
                };
                let (key, value) = line[prefix.len()..].split_once(':').unwrap_or(("", ""));
                let key = if prefix == "conf:file:" {
                    "conf:file".to_string()
                } else {
                    format!("{prefix}{key}")
                };
                let value = if prefix == "conf:file:" {
                    &line[prefix.len()..]
                } else {
                    value
                };
                if key.starts_with("summary:")
                    || matches!(
                        key.as_str(),
                        "command"
                            | "conf:file"
                            | "blocksize"
                            | "mode"
                            | "block_count"
                            | "info_count"
                    )
                {
                    let values = result.fields.entry(key).or_default();
                    if values.len() >= 3 {
                        return Err("zduplikowane pole logu".into());
                    }
                    values.push(value.to_string());
                }
                if line.starts_with("error:")
                    || line.starts_with("parity_error:")
                    || line.starts_with("msg:error:")
                    || line.starts_with("msg:fatal:")
                {
                    result.error = true;
                }
                Ok(())
            })?;
            read_lines(stdout, |line| {
                let line = line.trim();
                if line == "Nothing to do" {
                    result.nothing += 1;
                }
                if line == "Everything OK" {
                    result.clean += 1;
                }
                if let Some((percent, rest)) = line.split_once("% completed, ") {
                    let (mb, _) = rest.split_once(" MB accessed").ok_or("niepełny postęp")?;
                    if result.progress.len() >= 2 {
                        return Err("powtórzony postęp końcowy".into());
                    }
                    result.progress.push((
                        percent.parse().map_err(|_| "nieprawidłowy postęp")?,
                        mb.parse().map_err(|_| "nieprawidłowy odczyt MB")?,
                    ));
                }
                Ok(())
            })?;
            Ok(result)
        }

        fn identity(&self, spec: &ElasticSpec, command: &str) -> Result<(), String> {
            for key in self.fields.keys().filter(|key| key.starts_with("summary:")) {
                if !matches!(
                    key.as_str(),
                    "summary:exit"
                        | "summary:error_file"
                        | "summary:error_io"
                        | "summary:error_data"
                        | "summary:equal"
                        | "summary:added"
                        | "summary:removed"
                        | "summary:updated"
                        | "summary:moved"
                        | "summary:copied"
                        | "summary:restored"
                ) {
                    return Err("nieznane podsumowanie narzędzia".into());
                }
            }
            for (key, expected) in [
                ("command", command.to_string()),
                ("conf:file", spec.config_path()),
                (
                    "blocksize",
                    (u64::from(spec.snapraid.block_size_kib) * 1024).to_string(),
                ),
                ("mode", format!("par{}", spec.parity.len())),
            ] {
                if self.field(key) != [expected] {
                    return Err(format!("obcy lub niepełny log: {key}"));
                }
            }
            Ok(())
        }

        fn scan(&self) -> Result<bool, String> {
            let mut changed = false;
            for key in [
                "equal", "added", "removed", "updated", "moved", "copied", "restored",
            ] {
                let value = self
                    .number(&format!("summary:{key}"))?
                    .ok_or("niepełne podsumowanie scan")?;
                if key != "equal" && value != 0 {
                    changed = true;
                }
            }
            Ok(changed)
        }

        fn apply(
            &self,
            run: &mut ElasticSnapraidRun,
            spec: &ElasticSpec,
            empty_parity: bool,
        ) -> Result<(), String> {
            let command = if run.kind == ElasticSnapraidKind::Sync {
                "sync"
            } else {
                "scrub"
            };
            self.identity(spec, command)?;
            run.total_blocks = self.number("block_count")?;
            run.errors_file = self.number("summary:error_file")?;
            run.errors_io = self.number("summary:error_io")?;
            run.errors_data = self.number("summary:error_data")?;
            if self.error {
                return Err("diagnostyka błędu w logu".into());
            }
            let errors = [run.errors_file, run.errors_io, run.errors_data];
            if errors.iter().flatten().any(|v| *v != 0) {
                return Err("narzędzie zgłosiło błędy".into());
            }
            if run.kind == ElasticSnapraidKind::Sync {
                let scan = if self.scan()? { "diff" } else { "equal" };
                if errors == [None; 3]
                    && empty_parity
                    && self.nothing == 1
                    && self.clean == 0
                    && self.progress.is_empty()
                    && self.field("summary:exit") == [scan]
                {
                    run.total_blocks = Some(0);
                    return Ok(());
                }
                if self.field("summary:exit") != [scan, "ok"] {
                    return Err("niepełne zakończenie sync".into());
                }
                let idle = self.nothing == 1
                    && self.nothing_status == 1
                    && self.progress.is_empty()
                    && self.clean == 0;
                let worked = self.nothing == 0
                    && self.nothing_status == 0
                    && matches!(self.progress.as_slice(), [(100, _)])
                    && self.clean == 1;
                if !idle && !worked {
                    return Err("niepełna praca sync".into());
                }
            } else {
                let checked = self
                    .number("info_count")?
                    .ok_or("brak liczby bloków scrub")?;
                if checked == 0
                    || run.total_blocks.is_none_or(|total| checked > total)
                    || !matches!(self.progress.as_slice(), [(100, _)])
                    || self.clean != 1
                    || self.nothing != 0
                    || self.field("summary:exit") != ["ok"]
                {
                    return Err("niepełny pełny scrub".into());
                }
                run.checked_blocks = Some(checked);
            }
            if errors != [Some(0); 3] {
                return Err("brak liczników błędów".into());
            }
            run.accessed_mb = self.progress.first().map(|(_, mb)| *mb);
            Ok(())
        }
    }

    fn maintenance_guard(root: &Root, journal: &Journal) -> Result<(ElasticSpec, bool), String> {
        if journal.spec.parity.is_empty() {
            return Err("no_parity".into());
        }
        if journal.formatted.len() != roles(&journal.spec).len() {
            return Err("nieukończone formatowanie".into());
        }
        if journal.sync_completed_at.is_none() {
            return Err("niepotwierdzony pierwszy sync".into());
        }
        let devices = inventory()?;
        let spec = layout(&journal.spec, &devices)?;
        for role in roles(&journal.spec) {
            let device = resolve(role_disk(&journal.spec, role)?, &devices)?;
            filesystem_matches(&journal.spec, role, device)?;
            if !branch_mounted(&journal.spec, role, device)? {
                return Err("niezamontowany branch".into());
            }
        }
        if !union_mounted(&spec)? {
            return Err("niezamontowana unia".into());
        }
        let mut config = String::new();
        private_file(Path::new(&spec.config_path()), root.uid)?
            .take(JOURNAL_LIMIT + 1)
            .read_to_string(&mut config)
            .map_err(|e| e.to_string())?;
        if config != snapraid_config(&spec).map_err(|e| e.to_string())? {
            return Err("config niezgodny z UUID i journalem".into());
        }
        let mut empty = true;
        for (directive, path) in snapraid_directives(&spec).map_err(|e| e.to_string())? {
            if directive != "content" && !directive.ends_with("parity") {
                continue;
            }
            let path = Path::new(&path);
            directory(
                path.parent().ok_or("brak katalogu metadanych")?,
                false,
                root.uid,
            )?;
            let file = private_file(path, root.uid)?;
            let metadata = file.metadata().map_err(|e| e.to_string())?;
            let expected = spec
                .parity
                .iter()
                .find(|p| path.starts_with(parity_mount_path(&spec.name, p.index)))
                .map(|p| parity_mount_path(&spec.name, p.index))
                .unwrap_or_else(|| CONFIG_DIR.trim_end_matches('/').into());
            if metadata.dev()
                != std::fs::metadata(&expected)
                    .map_err(|e| e.to_string())?
                    .dev()
            {
                return Err("metadane na obcym filesystemie".into());
            }
            if directive != "content" && metadata.len() != 0 {
                empty = false;
            }
        }
        Ok((spec, empty))
    }

    fn capture_snapraid(
        root: &Root,
        array_lock: &File,
        directory: &Path,
        label: &str,
        program: &str,
        args: &[String],
    ) -> Result<(Option<i32>, Result<SnapraidLog, String>), String> {
        let open = |suffix: &str| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.join(format!("{label}.{suffix}")))
                .map_err(|e| e.to_string())
        };
        let log = open("log")?;
        let stdout = open("stdout")?;
        let stderr = open("stderr")?;
        File::open(directory)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        let mut argv = vec![
            "-l".to_string(),
            format!("/proc/self/fd/{}", log.as_raw_fd()),
        ];
        argv.extend_from_slice(args);
        let mut command = process_command(
            Path::new(program),
            &argv,
            false,
            &[
                root.node_lock.as_raw_fd(),
                array_lock.as_raw_fd(),
                log.as_raw_fd(),
            ],
        );
        command
            .current_dir("/")
            .stdout(stdout.try_clone().map_err(|e| e.to_string())?)
            .stderr(stderr.try_clone().map_err(|e| e.to_string())?);
        let result = command.spawn().and_then(|mut child| child.wait());
        for file in [&log, &stdout, &stderr] {
            file.sync_all().map_err(|e| e.to_string())?;
        }
        let status = result.map_err(|e| e.to_string())?;
        let mut parsed = SnapraidLog::parse(
            private_file(&directory.join(format!("{label}.log")), root.uid)?,
            private_file(&directory.join(format!("{label}.stdout")), root.uid)?,
        );
        if stderr.metadata().map_err(|e| e.to_string())?.len() != 0 {
            if let Ok(log) = &mut parsed {
                log.error = true;
            }
        }
        Ok((status.code(), parsed))
    }

    fn maintenance(
        root: &Root,
        mut journal: Journal,
        operation_id: &str,
        kind: ElasticSnapraidKind,
        worker: Option<&Worker>,
        held_array_lock: Option<&File>,
    ) -> Result<ElasticSnapraidResult, String> {
        let owned_array_lock;
        let array_lock = match held_array_lock {
            Some(lock) => lock,
            None => {
                owned_array_lock = root.array_lock(&journal.spec.array_id)?;
                &owned_array_lock
            }
        };
        if journal.private.is_some() && !worker.is_some_and(|worker| worker.public_mount().is_some()) {
            return Err("brak potwierdzonej publikacji prywatnej macierzy".into());
        }
        if journal.pending.is_some() || journal.stage != ElasticStage::Ready
            || journal.transfer.as_ref().is_some_and(|transfer| transfer.finished_at.is_none())
        {
            return Err("macierz wymaga uwagi lub operacja trwa".into());
        }
        if operation_id == journal.spec.operation_id
            || journal
                .last_run
                .as_ref()
                .is_some_and(|run| run.operation_id == operation_id)
        {
            return Err("ponowne użycie ID create".into());
        }
        let mut run = ElasticSnapraidRun {
            operation_id: operation_id.into(),
            kind,
            started_at: timestamp()?,
            finished_at: None,
            outcome: ElasticSnapraidOutcome::Running,
            exit_code: None,
            total_blocks: None,
            checked_blocks: None,
            accessed_mb: None,
            errors_file: None,
            errors_io: None,
            errors_data: None,
            detail: None,
        };
        let (spec, empty) = match maintenance_guard(root, &journal) {
            Ok(value) => value,
            Err(error) => {
                run.outcome = ElasticSnapraidOutcome::Refused;
                run.finished_at = Some(timestamp()?);
                run.detail = Some(
                    if error == "no_parity" {
                        "no_parity"
                    } else {
                        "precondition_failed"
                    }
                    .into(),
                );
                return Ok(ElasticSnapraidResult {
                    state: observe(&journal, worker),
                    run,
                });
            }
        };
        let path = root.path.join(format!("{}.runs", journal.spec.array_id));
        directory(&path, true, root.uid)?;
        let path = path.join(operation_id);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| format!("operacja już użyta lub katalog niedostępny: {e}"))?;
        File::open(path.parent().ok_or("brak katalogu operacji")?)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        let tools = Tools::resolve(&spec).map_err(|e| e.to_string())?;
        if kind == ElasticSnapraidKind::Scrub {
            let (code, diff) = capture_snapraid(
                root,
                &array_lock,
                &path,
                "diff",
                &tools.snapraid,
                &snapraid_args(&spec, &SnapraidAction::Diff).map_err(|e| e.to_string())?,
            )?;
            let diff = diff?;
            diff.identity(&spec, "diff")?;
            if diff.error {
                return Err("błąd odczytowego pre-diff".into());
            }
            let changed = diff.scan()?;
            if code != Some(if changed { 2 } else { 0 })
                || diff.field("summary:exit") != [if changed { "diff" } else { "equal" }]
            {
                return Err("niepełny wynik pre-diff".into());
            }
            if changed || empty {
                run.outcome = ElasticSnapraidOutcome::Refused;
                run.finished_at = Some(timestamp()?);
                run.detail = Some(
                    if changed {
                        "unsynced_changes"
                    } else {
                        "empty_parity"
                    }
                    .into(),
                );
                return Ok(ElasticSnapraidResult {
                    state: observe(&journal, worker),
                    run,
                });
            }
        }
        if maintenance_guard(root, &journal).is_err() {
            run.outcome = ElasticSnapraidOutcome::Refused;
            run.finished_at = Some(timestamp()?);
            run.detail = Some("precondition_failed".into());
            return Ok(ElasticSnapraidResult {
                state: observe(&journal, worker),
                run,
            });
        }
        let action = if kind == ElasticSnapraidKind::Sync {
            SnapraidAction::Sync
        } else {
            SnapraidAction::Scrub
        };
        let steps = plan_snapraid(&spec, &action, &tools).map_err(|e| e.to_string())?;
        let [ElasticStep::Run { program, args }] = steps.as_slice() else {
            return Err("nieprawidłowy plan ręcznej operacji".into());
        };
        let guard_journal = journal.clone();
        let run = perform_maintenance(
            root,
            &mut journal,
            run,
            &spec,
            || capture_snapraid(root, &array_lock, &path, "run", program, args),
            || maintenance_guard(root, &guard_journal).map(|(_, empty)| empty),
            false,
        )?;
        Ok(ElasticSnapraidResult {
            state: observe(&journal, worker),
            run,
        })
    }

    fn perform_maintenance(
        root: &Root,
        journal: &mut Journal,
        mut run: ElasticSnapraidRun,
        spec: &ElasticSpec,
        operation: impl FnOnce() -> Result<(Option<i32>, Result<SnapraidLog, String>), String>,
        post_guard: impl FnOnce() -> Result<bool, String>,
        hold_after_success: bool,
    ) -> Result<ElasticSnapraidRun, String> {
        let kind = run.kind;
        if hold_after_success {
            let transfer = journal.transfer.as_ref().ok_or("mover Sync wymaga trwałego transferu")?;
            let owned = Pending::Maintenance {
                operation_id: run.operation_id.clone(),
                kind: ElasticSnapraidKind::Sync,
            };
            if transfer.operation_id != run.operation_id
                || run.kind != ElasticSnapraidKind::Sync
                || run.outcome != ElasticSnapraidOutcome::Running
                || journal.pending.as_ref().is_some_and(|pending| *pending != owned)
                || transfer.current.is_some()
                || journal.private.as_ref().and_then(|private| private.service.as_ref())
                    .is_none_or(|service| service.mode != ElasticServiceMode::Hold)
            {
                return Err("mover Sync wymaga zgodnego transferu w Hold".into());
            }
        }
        journal.last_run = Some(run.clone());
        let mut captured = None;
        let outcome = pending_operation(
            journal,
            Pending::Maintenance {
                operation_id: run.operation_id.clone(),
                kind,
            },
            ElasticStage::SyncPending,
            |journal| root.save(journal),
            || {
                captured = Some(operation()?);
                Ok(())
            },
        );
        let outcome = outcome.and_then(|()| {
            let (code, parsed) = captured.as_ref().ok_or("brak wyniku procesu")?;
            run.exit_code = *code;
            let log = parsed.as_ref().map_err(Clone::clone)?;
            let checked = post_guard();
            let parsed = log.apply(&mut run, spec, checked.as_ref().copied().unwrap_or(false));
            checked?;
            if *code != Some(0) {
                return Err("proces SnapRAID nie zakończył się sukcesem".into());
            }
            parsed
        });
        run.finished_at = Some(timestamp()?);
        match outcome {
            Ok(()) => {
                run.outcome = ElasticSnapraidOutcome::Succeeded;
                if kind == ElasticSnapraidKind::Sync {
                    journal.sync_completed_at = run.finished_at.clone();
                    journal.stale_parity_bytes = None;
                    journal.stale_sync_operation = None;
                }
                journal.pending = None;
                journal.stage = if hold_after_success { ElasticStage::NeedsAttention } else { ElasticStage::Ready };
                journal.detail = None;
            }
            Err(error) => {
                run.outcome = if run.exit_code.is_some_and(|code| code != 0) {
                    ElasticSnapraidOutcome::Failed
                } else {
                    ElasticSnapraidOutcome::NeedsAttention
                };
                run.detail = Some(bounded_text(&error, TRANSFER_DETAIL_LIMIT));
                journal.stage = ElasticStage::NeedsAttention;
                journal.detail = Some(bounded_text(&error, TRANSFER_DETAIL_LIMIT));
            }
        }
        journal.last_run = Some(run.clone());
        if hold_after_success {
            if let Some(transfer) = journal.transfer.as_mut() {
                transfer.coupled_sync_result = Some(run.clone());
                if run.outcome != ElasticSnapraidOutcome::Succeeded {
                    transfer.phase = ElasticMoverPhase::NeedsAttention;
                    transfer.detail = run
                        .detail
                        .as_deref()
                        .map(|detail| bounded_text(detail, TRANSFER_DETAIL_LIMIT));
                }
            }
            // A mover's failed Sync does not pin a pending: the array records
            // parity as out of date, so the declared Resume can release it.
            if run.outcome != ElasticSnapraidOutcome::Succeeded {
                journal.pending = None;
                journal.stale_parity_bytes.get_or_insert(0);
                journal.stale_sync_operation = Some(run.operation_id.clone());
            }
        }
        root.save(journal)?;
        Ok(run)
    }

    fn create(root: &Root, spec: &ElasticCreateSpec, mut worker: Option<&mut Worker>) -> Result<ElasticResult, String> {
        if worker.is_none() {
            return Err("nowy Create wymaga prywatnego wykonawcy".into());
        }
        let array_lock = root.array_lock(&spec.array_id)?;
        let devices = inventory()?;
        let layout = layout(spec, &devices)?;
        let tools = Tools::resolve(&layout).map_err(|e| e.to_string())?;
        let steps = plan_create(&layout, &tools).map_err(|e| e.to_string())?;
        let mut journal = root.reserve(spec, boot_id()?, || {
            vacant_namespace(spec, worker.as_deref())?;
            for disk in spec.data.iter().chain(spec.cache.iter()).chain(&spec.parity) {
                clean_device(resolve(disk, &devices)?)?;
            }
            directory(Path::new(CONFIG_DIR.trim_end_matches('/')), false, root.uid)?;
            directory(
                Path::new(BRANCH_ROOT.trim_end_matches('/')),
                false,
                root.uid,
            )?;
            Ok(())
        })?;
        let outcome = execute_steps(root, &array_lock, &mut journal, &layout, steps, StepMode::Create, &mut LiveSteps { worker: worker.as_deref_mut() });
        finish(root, &mut journal, outcome, worker.as_deref())
    }

    fn restore_checkpoint_guard(journal: &Journal) -> Result<(), String> {
        if journal.private.as_ref().and_then(|private| private.service.as_ref())
            .is_some_and(|service| service.mode == ElasticServiceMode::Hold)
        {
            return Err("Restore nie konsumuje trwałego service Hold".into());
        }
        if journal.transfer.as_ref().is_some_and(|transfer| {
            transfer.finished_at.is_none()
                && !journal.private.as_ref().and_then(|private| private.service.as_ref())
                    .is_some_and(|service| service.mode == ElasticServiceMode::Online && service.pending
                        && service.operation_id == transfer.resume_operation_id)
        }) {
            return Err("Restore nie konsumuje niedokończonego transferu".into());
        }
        if matches!(
            journal.pending,
            Some(Pending::Sync | Pending::Maintenance { .. })
        ) || (!journal.spec.parity.is_empty() && journal.sync_completed_at.is_none())
        {
            return Err("sync niepotwierdzony; restore nie wykonuje sync".into());
        }
        Ok(())
    }

    fn service_guard(journal: &Journal, operation_id: &str) -> Result<(), String> {
        let private = journal.private.as_ref().ok_or("service wymaga prywatnej topologii")?;
        validate_elastic_uuid(operation_id).map_err(|e| e.to_string())?;
        if operation_id == journal.spec.operation_id {
            return Err("operacja service nie może zastępować Create".into());
        }
        // A mover owns its Hold until its declared Resume, finished or not, and
        // an unfinished mover blocks every other operation.
        let hold_owner = journal
            .private
            .as_ref()
            .and_then(|private| private.service.as_ref())
            .filter(|service| service.mode == ElasticServiceMode::Hold)
            .map(|service| service.operation_id.as_str());
        if journal.transfer.as_ref().is_some_and(|transfer| {
            transfer.operation_id != operation_id
                && (transfer.finished_at.is_none() || hold_owner == Some(transfer.operation_id.as_str()))
        }) {
            return Err("transfer movera zajmuje service Hold".into());
        }
        if journal.private.as_ref().and_then(|private| private.service.as_ref())
            .is_some_and(|service| service.operation_id == operation_id)
        {
            return Err("operacja service została już użyta".into());
        }
        if journal.pending.is_some()
            || journal.formatted.len() != roles(&journal.spec).len()
            || (!journal.spec.parity.is_empty() && journal.sync_completed_at.is_none())
        {
            return Err("service wymaga ukończonego Create i sync bez pending".into());
        }
        match private.service.as_ref() {
            None if journal.stage == ElasticStage::Ready => Ok(()),
            Some(service) if service.mode == ElasticServiceMode::Hold
                && matches!(journal.stage, ElasticStage::NeedsAttention | ElasticStage::Mounting) => Ok(()),
            Some(service) if service.mode == ElasticServiceMode::Online
                && !service.pending && journal.stage == ElasticStage::Ready => Ok(()),
            _ => Err("nieprawidłowy trwały stan service".into()),
        }
    }

    fn enter_service_with(
        root: &Root,
        mut journal: Journal,
        operation_id: &str,
        _array_lock: &File,
        set_readonly: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        service_guard(&journal, operation_id)?;
        journal.private.as_mut().ok_or("brak prywatnej topologii")?.service = Some(ElasticServiceState {
            mode: ElasticServiceMode::Hold,
            operation_id: operation_id.to_string(),
            pending: true,
        });
        journal.stage = ElasticStage::Mounting;
        root.save(&journal)?;
        if let Err(error) = set_readonly() {
            journal.stage = ElasticStage::NeedsAttention;
            journal.detail = Some(bounded_text(&format!("service: {error}"), TRANSFER_DETAIL_LIMIT));
            root.save(&journal)?;
            return Err(error);
        }
        let mut completed = journal;
        completed.pending = None;
        completed.stage = ElasticStage::NeedsAttention;
        completed.detail = Some("service Hold: unia potwierdzona jako globalnie RO".into());
        completed.private.as_mut().ok_or("brak prywatnej topologii")?.service.as_mut()
            .ok_or("brak stanu service")?.pending = false;
        root.save(&completed)?;
        Ok(())
    }

    fn authorize_resume(
        root: &Root,
        journal: &mut Journal,
        operation_id: &str,
    ) -> Result<(), String> {
        validate_elastic_uuid(operation_id).map_err(|e| e.to_string())?;
        let mut online = journal.clone();
        // A Sync of this mover left Running cannot still run: its SnapRAID
        // process would hold the array lock this Resume holds. It closes as an
        // interrupted attempt and the array is released with parity stale.
        if let Some(transfer) = journal.transfer.as_ref().filter(|transfer| {
            transfer.finished_at.is_none() && transfer.resume_operation_id == operation_id
        }) {
            close_interrupted_sync(&mut online, &transfer.operation_id, "Sync przerwany przed Resume")?;
        }
        let private = online.private.as_ref().ok_or("Resume wymaga prywatnej topologii")?;
        if operation_id == online.spec.operation_id
            || private.service.as_ref().is_some_and(|service| service.operation_id == operation_id)
            || online
                .transfer
                .as_ref()
                .is_some_and(|transfer| transfer_blocks_resume(&online, transfer, operation_id))
            || online.pending.is_some()
            || online.formatted.len() != roles(&online.spec).len()
            || (!online.spec.parity.is_empty() && online.sync_completed_at.is_none())
            || !private.service.as_ref().is_some_and(|service| service.mode == ElasticServiceMode::Hold)
            || !matches!(online.stage, ElasticStage::NeedsAttention | ElasticStage::Mounting)
        {
            return Err("Resume wymaga trwałego service Hold bez zwykłego pending".into());
        }
        online.private.as_mut().ok_or("brak prywatnej topologii")?.service = Some(ElasticServiceState {
            mode: ElasticServiceMode::Online,
            operation_id: operation_id.to_string(),
            pending: true,
        });
        online.stage = ElasticStage::Mounting;
        root.save(&online)?;
        *journal = online;
        Ok(())
    }

    fn restore_mount_guard(
        journal: &Journal,
        current_boot: &str,
        union_mounted: impl FnOnce() -> Result<bool, String>,
    ) -> Result<(), String> {
        if matches!(journal.pending, Some(Pending::Union))
            && current_boot == journal.boot_id
            && !union_mounted()?
        {
            return Err("niepewne zakończenie mount w tym samym boot; wymagany restart".into());
        }
        if journal.formatted.len() != roles(&journal.spec).len() {
            return Err("nieukończone formatowanie; restore nie formatuje".into());
        }
        Ok(())
    }

    fn restore(root: &Root, mut journal: Journal, mut worker: Option<&mut Worker>, held_array_lock: Option<&File>) -> Result<ElasticResult, String> {
        let owned_array_lock;
        let array_lock = match held_array_lock {
            Some(lock) => lock,
            None => {
                owned_array_lock = root.array_lock(&journal.spec.array_id)?;
                &owned_array_lock
            }
        };
        let outcome = (|| -> Result<(), String> {
            let current_boot = boot_id()?;
            restore_checkpoint_guard(&journal)?;
            let devices = inventory()?;
            let spec = layout(&journal.spec, &devices)?;
            for role in roles(&journal.spec) {
                filesystem_matches(
                    &journal.spec,
                    role,
                    resolve(role_disk(&journal.spec, role)?, &devices)?,
                )?;
            }
            restore_mount_guard(&journal, &current_boot, || union_mounted(&spec))?;
            let mut observed = Observed::default();
            observed.known = true;
            for role in roles(&journal.spec) {
                if branch_mounted(
                    &journal.spec,
                    role,
                    resolve(role_disk(&journal.spec, role)?, &devices)?,
                )? {
                    observed
                        .mounted
                        .insert(mount_path(&journal.spec, role), true);
                }
            }
            if union_mounted(&spec)? {
                observed.mounted.insert(spec.union_path(), true);
            }
            if spec.has_parity() {
                let mut content = String::new();
                private_file(Path::new(&spec.config_path()), root.uid)?
                    .read_to_string(&mut content)
                    .map_err(|e| e.to_string())?;
                if content != snapraid_config(&spec).map_err(|e| e.to_string())? {
                    return Err("konfiguracja niezgodna z journalem".into());
                }
            }
            if journal.boot_id != current_boot {
                if let Some(private) = journal.private.as_mut() {
                    private.anchor = None;
                    private.published = false;
                    journal.stage = ElasticStage::Mounting;
                }
            }
            journal.boot_id = current_boot;
            journal.pending = None;
            root.save(&journal)?;
            let tools = Tools::resolve(&spec).map_err(|e| e.to_string())?;
            execute_steps(
                root,
                &array_lock,
                &mut journal,
                &spec,
                plan_mount(&spec, &observed, &tools).map_err(|e| e.to_string())?,
                StepMode::Restore,
                &mut LiveSteps { worker: worker.as_deref_mut() },
            )?;
            if let Some(worker) = worker.as_deref_mut() {
                let private = journal.private.as_ref().ok_or("worker bez private journala")?;
                if !private.published || worker.public_mount().is_none() {
                    publish_private(root, &mut journal, |anchor| worker.publish(anchor).map(|_| ()))?;
                }
            }
            if journal.private.as_ref().and_then(|private| private.service.as_ref())
                .is_some_and(|service| service.mode == ElasticServiceMode::Online)
            {
                let worker = worker.as_deref_mut().ok_or("Resume wymaga workera")?;
                if worker.public_mount().is_none() {
                    return Err("Resume nie potwierdził publikacji RW".into());
                }
                worker.set_union_readonly(false)?;
                if worker.union_readonly()? {
                    return Err("Resume nie potwierdził publikacji RW".into());
                }
            }
            Ok(())
        })();
        finish(root, &mut journal, outcome, worker.as_deref())
    }

    #[derive(Serialize, Deserialize)]
    enum PrivateResponse {
        State(Box<ElasticResult>),
        Maintenance(Box<ElasticSnapraidResult>),
        Mover(Box<ElasticMoverResult>),
    }

    impl PrivateResponse {
        fn public_result(mut self, published: bool) -> Result<serde_json::Value, String> {
            let state = match &mut self {
                Self::State(state) => state.as_mut(),
                Self::Maintenance(result) => &mut result.state,
                Self::Mover(result) => &mut result.state,
            };
            state.union_mounted = state.union_mounted.map(|mounted| mounted && published);
            match self {
                Self::State(state) => serde_json::to_value(state),
                Self::Maintenance(result) => serde_json::to_value(result),
                Self::Mover(result) => serde_json::to_value(result),
            }
            .map_err(|e| e.to_string())
        }
    }

    fn private_paths(spec: &ElasticCreateSpec) -> Paths {
        Paths {
            branch_root: PathBuf::from(BRANCH_ROOT.trim_end_matches('/')),
            union_path: PathBuf::from(union_path(&spec.name)),
        }
    }

    fn authorize_publication(
        root: &Root,
        spec: &ElasticCreateSpec,
        anchor: &Anchor,
    ) -> Result<(), String> {
        let stored = root.load(&spec.array_id)?;
        let private = stored
            .private
            .as_ref()
            .ok_or("publiczny journal przy publikacji prywatnej")?;
        if private.service.as_ref().is_some_and(|service| service.mode == ElasticServiceMode::Hold) {
            return Err("publikacja zablokowana przez service Hold".into());
        }
        if stored.spec != *spec
            || stored.pending != Some(Pending::Union)
            || private.published
            || private.anchor.as_ref() != Some(anchor)
            || stored.boot_id != boot_id()?
        {
            return Err("brak dokładnego trwałego zamiaru publikacji".into());
        }
        Ok(())
    }

    fn private_create(root: &Root, spec: &ElasticCreateSpec) -> Result<serde_json::Value, String> {
        let journals = root.journals()?;
        if journals
            .iter()
            .any(|journal| journal.spec.array_id == spec.array_id)
        {
            return Err("macierz ma już journal; create nie jest ponawiane".into());
        }
        claims_guard(&journals, spec)?;
        vacant_namespace(spec, None)?;
        let devices = inventory()?;
        let layout = layout(spec, &devices)?;
        let tools = Tools::resolve(&layout).map_err(|e| e.to_string())?;
        let paths = private_paths(spec);
        directory(&paths.branch_root, true, root.uid)?;
        elastic_namespace::preflight(&paths, Path::new(&tools.mergerfs))?;
        let result = elastic_namespace::run(
            &paths,
            Entry::Fresh,
            &[root.node_lock.as_raw_fd()],
            |worker| {
                create(root, spec, Some(worker)).map(|state| PrivateResponse::State(Box::new(state)))
            },
            |anchor| authorize_publication(root, spec, anchor),
        )?;
        result.value.public_result(result.public_after.is_some())
    }

    fn private_operation(
        root: &Root,
        mut journal: Journal,
        command: &crate::HelperCommand,
    ) -> Result<serde_json::Value, String> {
        let spec = journal.spec.clone();
        let current_boot = boot_id()?;
        let fresh = current_boot != journal.boot_id;
        let restoring = matches!(command, crate::HelperCommand::ElasticRestore { .. });
        let mover_command = matches!(command, crate::HelperCommand::ElasticMover { .. });
        let service_command = matches!(command,
            crate::HelperCommand::ElasticEnterService { .. } | crate::HelperCommand::ElasticResume { .. });
        if fresh && matches!(command, crate::HelperCommand::ElasticInspect { .. })
            && journal.private.as_ref().and_then(|private| private.service.as_ref())
                .is_some_and(|service| service.mode == ElasticServiceMode::Hold)
        {
            return PrivateResponse::State(Box::new(observe(&journal, None))).public_result(false);
        }
        if service_command && fresh && !matches!(command, crate::HelperCommand::ElasticResume { .. }) {
            return Err("service wymaga istniejącej kotwicy".into());
        }
        if matches!(command, crate::HelperCommand::ElasticSync { .. } | crate::HelperCommand::ElasticScrub { .. }) {
            if let Some(service) = journal.private.as_ref().and_then(|private| private.service.as_ref()) {
                if service.mode == ElasticServiceMode::Hold || service.pending {
                    return Err("SnapRAID niedostępny podczas service Hold/pending".into());
                }
            }
        }
        if restoring {
            restore_checkpoint_guard(&journal)?;
        }
        if fresh {
            if !restoring && !matches!(command, crate::HelperCommand::ElasticResume { .. }) && !mover_command {
                return Err("prywatna macierz wymaga Restore po zmianie boot".into());
            }
            let devices = inventory()?;
            let layout = layout(&spec, &devices)?;
            for role in roles(&spec) {
                filesystem_matches(&spec, role, resolve(role_disk(&spec, role)?, &devices)?)?;
            }
            restore_mount_guard(&journal, &current_boot, || {
                Err("sonda unii przed prywatnym Restore".into())
            })?;
            let tools = Tools::resolve(&layout).map_err(|e| e.to_string())?;
            let paths = private_paths(&spec);
            directory(&paths.branch_root, true, root.uid)?;
            elastic_namespace::preflight(&paths, Path::new(&tools.mergerfs))?;
        }
        let anchor = journal
            .private
            .as_ref()
            .ok_or("brak prywatnej topologii")?
            .anchor
            .clone();
        // A recovery after a boot that stopped part-way: either before its
        // new anchor (no anchor in this boot) or after it, with a mount step
        // of the held, unfinished mover still open. Both wait for the next
        // boot, and both are refused for every other command, so Inspect
        // reports the state with the restart instead of failing.
        let held_recovery = journal
            .private
            .as_ref()
            .and_then(|private| private.service.as_ref())
            .is_some_and(|service| service.mode == ElasticServiceMode::Hold)
            && journal.transfer.as_ref().is_some_and(|transfer| transfer.finished_at.is_none())
            && matches!(
                journal.pending,
                Some(Pending::Mount(_) | Pending::Union | Pending::Config)
            );
        if !fresh
            && (anchor.is_none() || held_recovery)
            && matches!(command, crate::HelperCommand::ElasticInspect { .. })
        {
            return PrivateResponse::State(Box::new(restart_required_state(&journal))).public_result(false);
        }
        let entry = if fresh {
            Entry::Fresh
        } else {
            Entry::Existing(
                anchor
                    .as_ref()
                    .ok_or("brak kotwicy w tym samym boot; wymagany restart")?,
            )
        };
        let array_lock = root.array_lock(&spec.array_id)?;
        if let crate::HelperCommand::ElasticResume { operation_id, .. } = command {
            authorize_resume(root, &mut journal, operation_id)?;
        }
        let result = elastic_namespace::run(
            &private_paths(&spec),
            entry,
            &[root.node_lock.as_raw_fd(), array_lock.as_raw_fd()],
            |worker| match command {
                crate::HelperCommand::ElasticRestore { .. } => {
                    restore(root, journal, Some(worker), Some(&array_lock))
                        .map(|state| PrivateResponse::State(Box::new(state)))
                }
                crate::HelperCommand::ElasticEnterService { operation_id, .. } => enter_service_with(
                    root,
                    journal,
                    operation_id,
                    &array_lock,
                    || worker.set_union_readonly(true),
                )
                .and_then(|()| root.load(&spec.array_id))
                .map(|stored| PrivateResponse::State(Box::new(observe(&stored, Some(worker))))),
                crate::HelperCommand::ElasticResume { .. } => restore(root, journal, Some(worker), Some(&array_lock))
                    .map(|state| PrivateResponse::State(Box::new(state))),
                crate::HelperCommand::ElasticMover { operation_id, resume_operation_id, rules, coupled_sync, .. } => mover(
                    root,
                    &mut journal,
                    &MoverRequest {
                        operation_id,
                        resume_operation_id,
                        rules,
                        coupled_sync: *coupled_sync,
                        array_lock: &array_lock,
                    },
                    worker,
                ).map(|run| PrivateResponse::Mover(Box::new(run))),
                crate::HelperCommand::ElasticInspect { .. } => {
                    Ok(PrivateResponse::State(Box::new(observe(&journal, Some(worker)))))
                }
                crate::HelperCommand::ElasticSync { operation_id, .. } => maintenance(
                    root,
                    journal,
                    operation_id,
                    ElasticSnapraidKind::Sync,
                    Some(worker),
                    Some(&array_lock),
                )
                .map(|result| PrivateResponse::Maintenance(Box::new(result))),
                crate::HelperCommand::ElasticScrub { operation_id, .. } => maintenance(
                    root,
                    journal,
                    operation_id,
                    ElasticSnapraidKind::Scrub,
                    Some(worker),
                    Some(&array_lock),
                )
                .map(|result| PrivateResponse::Maintenance(Box::new(result))),
                _ => Err("nieprawidłowa operacja prywatnej macierzy".into()),
            },
            |anchor| authorize_publication(root, &spec, anchor),
        )?;
        result.value.public_result(result.public_after.is_some())
    }

    pub(crate) fn execute(command: &crate::HelperCommand) -> Result<String, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("wykonawca Elastic wymaga root".into());
        }
        let root = Root::open(Path::new(ROOT), 0)?;
        let value = match command {
            crate::HelperCommand::ElasticCreate { operation } => {
                return serde_json::to_string(&private_create(&root, operation)?).map_err(|e| e.to_string());
            }
            crate::HelperCommand::ElasticEnterService { array_id, owner, .. }
            | crate::HelperCommand::ElasticResume { array_id, owner, .. }
            | crate::HelperCommand::ElasticMover { array_id, owner, .. } => {
                let journal = root.load(array_id)?;
                if &journal.spec.owner != owner { return Err("macierz niedostępna dla właściciela".into()); }
                if journal.private.is_none() {
                    return Err("service wymaga prywatnej topologii".into());
                }
                serde_json::to_value(private_operation(&root, journal, command)?)
            }
            crate::HelperCommand::ElasticSync { array_id, owner, operation_id }
            | crate::HelperCommand::ElasticScrub { array_id, owner, operation_id } => {
                let journal = root.load(array_id)?;
                if &journal.spec.owner != owner { return Err("macierz niedostępna dla właściciela".into()); }
                if journal.private.is_some() {
                    return serde_json::to_string(&private_operation(&root, journal, command)?).map_err(|e| e.to_string());
                }
                let kind = if matches!(command, crate::HelperCommand::ElasticSync { .. }) { ElasticSnapraidKind::Sync } else { ElasticSnapraidKind::Scrub };
                serde_json::to_value(maintenance(&root, journal, operation_id, kind, None, None)?)
            }
            crate::HelperCommand::ElasticInspect { array_id, owner }
            | crate::HelperCommand::ElasticRestore { array_id, owner } => {
                let journal = root.load(array_id)?;
                if &journal.spec.owner != owner {
                    return Err("macierz niedostępna dla właściciela".into());
                }
                if journal.private.is_some() {
                    return serde_json::to_string(&private_operation(&root, journal, command)?).map_err(|e| e.to_string());
                }
                let result = if matches!(command, crate::HelperCommand::ElasticRestore { .. }) {
                    restore(&root, journal, None, None)?
                } else {
                    observe(&journal, None)
                };
                serde_json::to_value(result)
            }
            crate::HelperCommand::ElasticClaims { name } => {
                let journals = root.journals()?;
                let disks = journals
                    .iter()
                    .flat_map(|j| j.spec.data.iter().chain(j.spec.cache.iter()).chain(&j.spec.parity))
                    .map(|d| ElasticClaim {
                        disk_id: d.disk_id.clone(),
                        wwn: d.wwn.clone(),
                        serial: d.serial.clone(),
                    })
                    .collect();
                serde_json::to_value(ElasticClaimsResult {
                    name_claimed: name
                        .as_ref()
                        .map(|n| journals.iter().any(|j| &j.spec.name == n)),
                    namespace_clear: name.as_ref().and_then(|n| zfs_namespace_clear(n).ok()),
                    disks,
                })
            }
            _ => return Err("nie jest poleceniem Elastic".into()),
        }
        .map_err(|e| e.to_string())?;
        serde_json::to_string(&value).map_err(|e| e.to_string())
    }

    fn claims_guard(journals: &[Journal], spec: &ElasticCreateSpec) -> Result<(), String> {
        for journal in journals {
            if journal.spec.name == spec.name {
                return Err("nazwa zarezerwowana".into());
            }
            for disk in spec.data.iter().chain(spec.cache.iter()).chain(&spec.parity) {
                if journal
                    .spec
                    .data
                    .iter()
                    .chain(journal.spec.cache.iter())
                    .chain(&journal.spec.parity)
                    .any(|old| {
                        old.disk_id == disk.disk_id
                            || old.expected_uuid == disk.expected_uuid
                            || disk
                                .wwn
                                .as_ref()
                                .is_some_and(|v| old.wwn.as_ref() == Some(v))
                            || disk
                                .serial
                                .as_ref()
                                .is_some_and(|v| old.serial.as_ref() == Some(v))
                    })
                {
                    return Err("nośnik zarezerwowany".into());
                }
            }
        }
        Ok(())
    }

    fn zfs_name_guard(journals: &[Journal], name: &str) -> Result<(), String> {
        let pool = name.split('/').next().ok_or("brak nazwy puli")?;
        if journals.iter().any(|j| j.spec.name == pool) {
            return Err("nazwa zarezerwowana przez Elastic".into());
        }
        Ok(())
    }

    fn zfs_path_guard(journals: &[Journal], path: &str) -> Result<(), String> {
        if path == "none" || path == "legacy" || path == "-" {
            return Ok(());
        }
        let path = Path::new(path);
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("niejednoznaczny mountpoint ZFS".into());
        }
        if journals.iter().any(|j| {
            [union_path(&j.spec.name), branch_root(&j.spec.name)]
                .iter()
                .any(|reserved| overlaps(path, Path::new(reserved)))
        }) {
            return Err("mountpoint przecina rezerwację Elastic".into());
        }
        Ok(())
    }

    fn physical_device(path: &str, devices: &[Device]) -> Result<Device, String> {
        let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_block_device() {
            return Err("ZFS: źródło nie jest block device".into());
        }
        let major_minor = format!(
            "{}:{}",
            libc::major(metadata.rdev()),
            libc::minor(metadata.rdev())
        );
        let mut sys = std::fs::canonicalize(format!("/sys/dev/block/{major_minor}"))
            .map_err(|e| e.to_string())?;
        if sys
            .join("partition")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            sys = sys.parent().ok_or("brak rodzica partycji")?.to_path_buf();
        }
        let kernel = sys
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("brak kernel name")?;
        let found: Vec<_> = devices.iter().filter(|d| d.kernel == kernel).collect();
        if found.len() != 1 {
            return Err("nieznana tożsamość fizyczna nośnika ZFS".into());
        }
        Ok(found[0].clone())
    }

    fn zfs_disk_guard(journals: &[Journal], device: &Device) -> Result<(), String> {
        if device.wwn.is_none() && device.serial.is_none() && !journals.is_empty() {
            return Err("brak tożsamości do porównania rezerwacji ZFS".into());
        }
        if journals
            .iter()
            .flat_map(|j| j.spec.data.iter().chain(j.spec.cache.iter()).chain(&j.spec.parity))
            .any(|disk| {
                disk.wwn
                    .as_ref()
                    .is_some_and(|v| device.wwn.as_ref() == Some(v))
                    || disk
                        .serial
                        .as_ref()
                        .is_some_and(|v| device.serial.as_ref() == Some(v))
            })
        {
            return Err("nośnik zarezerwowany przez Elastic".into());
        }
        Ok(())
    }

    #[derive(Debug)]
    struct ZfsDataset {
        name: String,
        mountpoint: String,
        canmount: String,
    }

    fn zfs_datasets(zfs: &Path) -> Result<Vec<ZfsDataset>, String> {
        let text = success(run(
            zfs,
            &[
                "list".into(),
                "-H".into(),
                "-o".into(),
                "name,mountpoint,canmount".into(),
                "-t".into(),
                "filesystem".into(),
            ],
            None,
            &[],
        )?)?;
        text.lines()
            .map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                if fields.len() != 3 || !["on", "off", "noauto"].contains(&fields[2]) {
                    return Err("nieczytelne właściwości ZFS".into());
                }
                crate::validate_dataset_name(fields[0]).map_err(|e| e.to_string())?;
                Ok(ZfsDataset {
                    name: fields[0].into(),
                    mountpoint: fields[1].into(),
                    canmount: fields[2].into(),
                })
            })
            .collect()
    }

    fn dataset_under(name: &str, root: &str) -> bool {
        name == root || name.starts_with(&format!("{root}/"))
    }

    fn child_mountpoint(name: &str, datasets: &[ZfsDataset]) -> Result<String, String> {
        let (parent, child) = name.rsplit_once('/').ok_or("brak rodzica datasetu")?;
        let parent = datasets
            .iter()
            .find(|d| d.name == parent)
            .ok_or("brak właściwości rodzica")?;
        if parent.mountpoint == "none" || parent.mountpoint == "legacy" {
            return Ok(parent.mountpoint.clone());
        }
        Ok(format!(
            "{}/{child}",
            parent.mountpoint.trim_end_matches('/')
        ))
    }

    fn zfs_namespace_command(
        command: &crate::HelperCommand,
        journals: &[Journal],
        zfs: &Path,
    ) -> Result<(), String> {
        use crate::HelperCommand::*;
        match command {
            ZpoolCreate {
                pool, mountpoint, ..
            } => {
                zfs_name_guard(journals, pool)?;
                zfs_path_guard(journals, mountpoint)
            }
            ZpoolAdd { pool, .. } | ZpoolAttach { pool, .. } | ZpoolReplace { pool, .. } => {
                zfs_name_guard(journals, pool)
            }
            ZfsCreate {
                name,
                kind,
                properties,
                ..
            } => {
                zfs_name_guard(journals, name)?;
                if *kind == crate::DatasetKind::Volume {
                    return Ok(());
                }
                let mountpoint = match properties.iter().find(|(key, _)| key == "mountpoint") {
                    Some((_, value)) => value.clone(),
                    None => child_mountpoint(name, &zfs_datasets(zfs)?)?,
                };
                zfs_path_guard(journals, &mountpoint)
            }
            ZfsClone { target, .. } => {
                zfs_name_guard(journals, target)?;
                zfs_path_guard(journals, &child_mountpoint(target, &zfs_datasets(zfs)?)?)
            }
            ZfsMount { dataset } => {
                zfs_name_guard(journals, dataset)?;
                let datasets = zfs_datasets(zfs)?;
                let row = datasets
                    .iter()
                    .find(|d| &d.name == dataset)
                    .ok_or("brak datasetu")?;
                zfs_path_guard(journals, &row.mountpoint)
            }
            ZfsSet { name, property, .. } | ZfsInherit { name, property } => {
                zfs_name_guard(journals, name)?;
                let datasets = zfs_datasets(zfs)?;
                let root = datasets
                    .iter()
                    .find(|d| &d.name == name)
                    .ok_or("brak datasetu")?;
                let replacement = if property == "mountpoint" {
                    Some(match command {
                        ZfsSet { value, .. } => value.clone(),
                        _ => child_mountpoint(name, &datasets)?,
                    })
                } else {
                    None
                };
                for row in datasets.iter().filter(|d| dataset_under(&d.name, name)) {
                    zfs_path_guard(journals, &row.mountpoint)?;
                    if let Some(new) = &replacement {
                        zfs_path_guard(journals, new)?;
                        if let Ok(suffix) =
                            Path::new(&row.mountpoint).strip_prefix(&root.mountpoint)
                        {
                            zfs_path_guard(
                                journals,
                                &Path::new(new).join(suffix).to_string_lossy(),
                            )?;
                        }
                    }
                }
                Ok(())
            }
            _ => Err("nieobsługiwany guard przestrzeni ZFS".into()),
        }
    }

    struct Importable {
        name: String,
        guid: String,
        leaves: Vec<String>,
    }

    fn mount_imported(
        name: &str,
        journals: &[Journal],
        datasets: &[ZfsDataset],
        mut mount: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<String, String> {
        let outcome = (|| -> Result<String, String> {
            let rows: Vec<_> = datasets
                .iter()
                .filter(|d| dataset_under(&d.name, name))
                .collect();
            if rows.is_empty() {
                return Err("brak właściwości zaimportowanej puli".into());
            }
            for row in &rows {
                zfs_path_guard(journals, &row.mountpoint)?;
            }
            for row in rows
                .into_iter()
                .filter(|d| d.canmount == "on" && d.mountpoint.starts_with('/'))
            {
                mount(&row.name)?;
            }
            Ok("Pula zaimportowana; mountpointy sprawdzone".into())
        })();
        outcome.map_err(|e| {
            format!("Pula pozostaje zaimportowana po import -N; montowanie nieukończone: {e}")
        })
    }

    fn import_scan(text: &str, guid: &str) -> Result<Importable, String> {
        let mut rows: Vec<Importable> = Vec::new();
        let mut config = false;
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix("pool:") {
                rows.push(Importable {
                    name: name.trim().into(),
                    guid: String::new(),
                    leaves: Vec::new(),
                });
                config = false;
            } else if let Some(row) = rows.last_mut() {
                if let Some(id) = line.strip_prefix("id:") {
                    row.guid = id.trim().into();
                } else if line == "config:" {
                    config = true;
                } else if config && !line.is_empty() {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    let name = fields[0];
                    if name == row.name
                        || ["logs", "cache", "spares", "special", "dedup"].contains(&name)
                        || ["mirror-", "raidz", "draid", "replacing-", "spare-"]
                            .iter()
                            .any(|p| name.starts_with(p))
                    {
                        continue;
                    }
                    if fields.len() < 2
                        || !["ONLINE", "DEGRADED", "AVAIL", "INUSE"].contains(&fields[1])
                    {
                        return Err("nieznana lub niedostępna pozycja import scan".into());
                    }
                    crate::validate_vdev_name(name).map_err(|e| e.to_string())?;
                    row.leaves.push(if name.starts_with('/') {
                        name.into()
                    } else {
                        format!("/dev/{name}")
                    });
                }
            }
        }
        let mut matches: Vec<_> = rows.into_iter().filter(|r| r.guid == guid).collect();
        if matches.len() != 1 || matches[0].leaves.is_empty() {
            return Err("niejednoznaczny import scan".into());
        }
        Ok(matches.remove(0))
    }

    pub(crate) fn guarded_zfs(
        command: &crate::HelperCommand,
        payload: &[u8],
    ) -> Result<String, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("guard ZFS wymaga root".into());
        }
        let root = Root::open(Path::new(ROOT), 0)?;
        let journals = root.journals()?;
        let resolved = command.resolve_exec().map_err(|e| e.to_string())?;
        let zfs = tool(&["/usr/sbin/zfs", "/sbin/zfs", "/usr/bin/zfs"])?;
        let locks = [root.node_lock.as_raw_fd()];
        use crate::HelperCommand::*;
        if let ZpoolImport { guid, new_name, .. } = command {
            let scan = success(run(&resolved.program, &["import".into()], None, &[])?)?;
            let scan = import_scan(&scan, guid)?;
            zfs_name_guard(&journals, &scan.name)?;
            let name = if new_name.is_empty() {
                &scan.name
            } else {
                new_name
            };
            zfs_name_guard(&journals, name)?;
            let devices = inventory()?;
            for path in &scan.leaves {
                zfs_disk_guard(&journals, &physical_device(path, &devices)?)?;
            }
            let mut args = resolved.args.clone();
            args.insert(1, "-N".into());
            success(run(&resolved.program, &args, None, &locks)?)?;
            let outcome = (|| -> Result<String, String> {
                let actual_guid = success(run(
                    &resolved.program,
                    &[
                        "get".into(),
                        "-H".into(),
                        "-o".into(),
                        "value".into(),
                        "guid".into(),
                        name.clone(),
                    ],
                    None,
                    &[],
                )?)?;
                if actual_guid.trim() != guid {
                    return Err("inna tożsamość zaimportowanej puli".into());
                }
                let datasets = zfs_datasets(&zfs)?;
                mount_imported(name, &journals, &datasets, |dataset| {
                    success(run(&zfs, &["mount".into(), dataset.into()], None, &locks)?).map(|_| ())
                })
            })();
            return outcome.map_err(|e| {
                format!("Pula pozostaje zaimportowana po import -N; montowanie nieukończone: {e}")
            });
        }
        zfs_namespace_command(command, &journals, &zfs)?;
        let paths: Vec<&str> = match command {
            ZpoolCreate { vdevs, .. } => vdevs
                .iter()
                .flat_map(|v| v.devices.iter().map(String::as_str))
                .collect(),
            ZpoolAdd { vdev, .. } => vdev.devices.iter().map(String::as_str).collect(),
            ZpoolAttach { device, .. } => vec![device],
            ZpoolReplace { new, .. } => vec![new],
            _ => Vec::new(),
        };
        if !paths.is_empty() {
            let devices = inventory()?;
            for path in paths {
                zfs_disk_guard(&journals, &physical_device(path, &devices)?)?;
            }
        }
        success(run(
            &resolved.program,
            &resolved.args,
            if payload.is_empty() {
                None
            } else {
                Some(payload)
            },
            &locks,
        )?)
    }

    #[cfg(test)]
    pub(crate) mod tests {
        use super::*;
        use std::os::fd::FromRawFd;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::os::unix::process::ExitStatusExt;

        // Fork zachowuje cudze deskryptory CLOEXEC aż do exec, więc nie może nakładać się na pomiar drop/reopen.
        pub(crate) static FORK_REOPEN: std::sync::Mutex<()> = std::sync::Mutex::new(());

        fn output(code: i32, stdout: &str, stderr: &str) -> Output {
            Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }

        #[test]
        fn blank_signature_probes_refuse_io_errors_before_reservation() {
            for (code, stdout, stderr) in [
                (2, "", "Input/output error"),
                (8, "", ""),
                (0, "UUID=a\nUUID=b\n", ""),
                (0, "", ""),
                (0, "not-export", ""),
            ] {
                let dir = Temp::new();
                let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
                assert!(root
                    .reserve(&spec(), boot(), || {
                        parse_blkid(output(code, stdout, stderr))?;
                        panic!("błędny odczyt nie może dopuścić formatowania")
                    })
                    .is_err());
                assert!(root.journals().expect("journal").is_empty());
            }
            assert!(parse_blkid(output(2, "", ""))
                .expect("brak sygnatur blkid")
                .is_empty());
            assert_eq!(
                parse_blkid(output(0, "UUID=abc\nTYPE=ext4\n", "")).expect("FS")["TYPE"],
                "ext4"
            );
            for (code, stdout, stderr) in [
                (1, "", ""),
                (0, "{}", ""),
                (0, "{\"signatures\":[]}", "read error"),
                (0, "{\"signatures\":[{\"type\":\"zfs_member\"}]}", ""),
            ] {
                assert!(parse_blank_wipefs(output(code, stdout, stderr)).is_err());
            }
            parse_blank_wipefs(output(0, "{\"signatures\":[]}", ""))
                .expect("niezależny brak sygnatur");
        }

        struct Temp(PathBuf);
        impl Temp {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "tentanas-elastic-test-{}-{}",
                    std::process::id(),
                    NEXT_FILE.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .expect("prywatny katalog");
                Self(path)
            }
        }
        impl Drop for Temp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        fn spec() -> ElasticCreateSpec {
            ElasticCreateSpec {
                array_id: "11111111-1111-4111-8111-111111111111".into(),
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                owner: ElasticOwner {
                    org_id: "org".into(),
                    addon_id: "nas".into(),
                },
                name: "media".into(),
                filesystem: ElasticFilesystem::Ext4,
                data: vec![ElasticDiskSpec {
                    disk_id: "serial:test".into(),
                    serial: Some("test".into()),
                    wwn: None,
                    bytes: 32 << 30,
                    expected_uuid: "33333333-3333-4333-8333-333333333333".into(),
                }],
                cache: None,
                parity: Vec::new(),
            }
        }

        fn cache_disk() -> ElasticDiskSpec {
            ElasticDiskSpec {
                disk_id: "serial:cache".into(),
                serial: Some("cache".into()),
                wwn: None,
                bytes: 16 << 30,
                expected_uuid: "66666666-6666-4666-8666-666666666666".into(),
            }
        }

        #[test]
        fn cached_spec_survives_root_reopen() {
            let mut cached = spec();
            cached.cache = Some(cache_disk());
            cached.validate().expect("cache spec");
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            root.reserve(&cached, boot(), || Ok(())).expect("reserve");
            drop(root);
            let reopened_root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("reopen root");
            let reopened = reopened_root.load(&cached.array_id).expect("reopen");
            assert_eq!(reopened.spec.cache, cached.cache);
            assert_eq!(roles(&reopened.spec), vec![ElasticRole::Data(1), ElasticRole::Cache]);
            assert_eq!(mount_path(&reopened.spec, ElasticRole::Cache), cache_branch_path("media", "c1"));
        }

        #[test]
        fn cache_duplicate_identity_and_invalid_uuid_are_refused() {
            let mut duplicate = spec();
            duplicate.cache = Some(duplicate.data[0].clone());
            assert!(duplicate.validate().is_err());
            let mut duplicate_uuid = spec();
            let mut same_uuid = cache_disk();
            same_uuid.disk_id = "serial:other-cache".into();
            same_uuid.serial = Some("other-cache".into());
            same_uuid.expected_uuid = duplicate_uuid.data[0].expected_uuid.clone();
            duplicate_uuid.cache = Some(same_uuid);
            assert!(duplicate_uuid.validate().is_err());
            let mut cache_parity = spec();
            let cache = cache_disk();
            let mut parity = cache.clone();
            parity.expected_uuid = "99999999-9999-4999-8999-999999999999".into();
            parity.disk_id = "serial:other-parity".into();
            parity.bytes = 32 << 30;
            cache_parity.cache = Some(cache);
            cache_parity.parity.push(parity);
            assert!(cache_parity.validate().is_err());
            let mut invalid = spec();
            let mut disk = cache_disk();
            disk.expected_uuid = "not-a-uuid".into();
            invalid.cache = Some(disk);
            assert!(invalid.validate().is_err());
        }

        #[test]
        fn cache_claim_is_reserved_across_arrays() {
            let mut cached = spec();
            cached.cache = Some(cache_disk());
            let mut other = cached.clone();
            other.array_id = "77777777-7777-4777-8777-777777777777".into();
            other.operation_id = "88888888-8888-4888-8888-888888888888".into();
            other.name = "other".into();
            other.data[0].disk_id = "serial:other-data".into();
            other.data[0].serial = Some("other-data".into());
            other.data[0].expected_uuid = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into();
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            root.reserve(&cached, boot(), || Ok(())).expect("first reserve");
            let mut callback_calls = 0;
            assert!(root.reserve(&other, boot(), || {
                callback_calls += 1;
                Ok(())
            }).is_err());
            assert_eq!(callback_calls, 0);
            assert_eq!(root.journals().expect("journals").len(), 1);
            let mut distinct = other.clone();
            distinct.array_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
            distinct.operation_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc".into();
            distinct.name = "distinct".into();
            distinct.cache.as_mut().expect("cache").disk_id = "serial:distinct-cache".into();
            distinct.cache.as_mut().expect("cache").serial = Some("distinct-cache".into());
            distinct.cache.as_mut().expect("cache").expected_uuid = "dddddddd-dddd-4ddd-8ddd-dddddddddddd".into();
            root.reserve(&distinct, boot(), || Ok(())).expect("distinct cache");
        }

        #[test]
        fn zfs_guard_checks_cache_identity_and_allows_distinct_device() {
            let mut cached = spec();
            cached.cache = Some(cache_disk());
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            root.reserve(&cached, boot(), || Ok(())).expect("reserve");
            let journals = root.journals().expect("journals");
            let base = Device {
                path: "/dev/vdc".into(), kernel: "vdc".into(), bytes: 16 << 30,
                wwn: None, serial: Some("cache".into()), major_minor: "252:32".into(), occupied: false,
            };
            assert!(zfs_disk_guard(&journals, &base).is_err());
            let distinct = Device { serial: Some("free".into()), ..base };
            zfs_disk_guard(&journals, &distinct).expect("distinct device");
        }

        #[test]
        fn schema_one_with_cache_is_refused_without_migration() {
            let mut cached = spec();
            cached.cache = Some(cache_disk());
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let bytes = serde_json::to_vec(&serde_json::json!({
                "schema": 1, "spec": cached, "stage": "prepared", "formatted": [],
                "pending": null, "boot_id": boot(), "sync_completed_at": null,
                "detail": null, "last_run": null
            })).expect("schema 1");
            atomic_write(&root.path.join(format!("{}.json", spec().array_id)), &bytes, root.uid)
                .expect("write journal");
            assert!(root.load(&spec().array_id).is_err());
        }

        #[test]
        fn existing_no_cache_journal_roundtrip_omits_cache() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = legacy_journal(&root);
            let encoded = serde_json::to_value(&journal).expect("journal");
            assert!(encoded["spec"].get("cache").is_none());
            root.save(&journal).expect("save");
            let reopened = root.load(&journal.spec.array_id).expect("reopen");
            assert!(reopened.spec.cache.is_none());
        }

        #[test]
        fn cache_role_is_mounted_and_restored_without_mkfs() {
            let plan = ElasticSpec {
                name: "media".into(),
                filesystem: "ext4".into(),
                data: vec![Branch { disk: "d1".into(), device: "/dev/vdb".into() }],
                cache: vec![Branch { disk: "c1".into(), device: "/dev/vdc".into() }],
                parity: Vec::new(),
                mergerfs: MergerfsOptions::default(),
                snapraid: SnapraidOptions::default(),
            };
            let create = plan_create(&plan, &Tools::for_preview()).expect("create plan");
            assert!(create.iter().any(|step| matches!(step,
                ElasticStep::Mount { mountpoint, .. } if mountpoint == &cache_branch_path("media", "c1"))));
            let restore = plan_mount(&plan, &Observed::nothing_mounted(), &Tools::for_preview())
                .expect("restore plan");
            assert!(restore.iter().any(|step| matches!(step,
                ElasticStep::Mount { mountpoint, .. } if mountpoint == &cache_branch_path("media", "c1"))));
            assert!(!restore.iter().any(ElasticStep::is_destructive));
        }

        #[test]
        fn cache_is_rw_data_is_nc_and_snapraid_excludes_cache() {
            let plan = ElasticSpec {
                name: "media".into(),
                filesystem: "ext4".into(),
                data: vec![Branch { disk: "d1".into(), device: "/dev/vdb".into() }],
                cache: vec![Branch { disk: "c1".into(), device: "/dev/vdc".into() }],
                parity: vec![ParityDisk { index: 1, disk: "p1".into(), device: "/dev/vdd".into() }],
                mergerfs: MergerfsOptions::default(),
                snapraid: SnapraidOptions::default(),
            };
            let branches = plan.branch_specs();
            assert!(branches.iter().any(|value| value == &format!("{}=RW", cache_branch_path("media", "c1"))));
            assert!(branches.iter().any(|value| value == &format!("{}=NC", data_branch_path("media", "d1"))));
            let config = snapraid_config(&plan).expect("snapraid config");
            assert!(!config.contains("/cache/"));
        }
        fn boot() -> String {
            "44444444-4444-4444-8444-444444444444".into()
        }

        fn legacy_journal(root: &Root) -> Journal {
            let bytes = serde_json::to_vec(&serde_json::json!({
                "schema": 1, "spec": spec(), "stage": "prepared", "formatted": [],
                "pending": null, "boot_id": boot(), "sync_completed_at": null,
                "detail": null, "last_run": null
            }))
            .expect("publiczny historyczny format");
            atomic_write(
                &root.path.join(format!("{}.json", spec().array_id)),
                &bytes,
                root.uid,
            )
            .expect("historyczny journal");
            root.load(&spec().array_id).expect("odczyt schema 1")
        }

        fn anchor_fixture() -> Anchor {
            Anchor {
                boot_id: boot(),
                pid: 42,
                start_ticks: 100,
                mount_ns_inode: 1000,
                exe_device: 1,
                exe_inode: 2000,
                exe_sha256: "a".repeat(64),
                union_device: 99,
                union_source: "data/d1".into(),
            }
        }

        fn snap_spec() -> ElasticSpec {
            ElasticSpec {
                name: "media".into(),
                filesystem: "xfs".into(),
                data: vec![Branch {
                    disk: "d1".into(),
                    device: "/dev/vdb".into(),
                }],
                cache: vec![],
                parity: vec![ParityDisk {
                    index: 1,
                    disk: "p1".into(),
                    device: "/dev/vdc".into(),
                }],
                mergerfs: MergerfsOptions::default(),
                snapraid: SnapraidOptions::default(),
            }
        }

        fn run_record(kind: ElasticSnapraidKind) -> ElasticSnapraidRun {
            ElasticSnapraidRun {
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                kind,
                started_at: "2026-09-08T12:00:00Z".into(),
                finished_at: None,
                outcome: ElasticSnapraidOutcome::Running,
                exit_code: None,
                total_blocks: None,
                checked_blocks: None,
                accessed_mb: None,
                errors_file: None,
                errors_io: None,
                errors_data: None,
                detail: None,
            }
        }

        fn log_text(command: &str, body: &str) -> String {
            format!(
                "command:{command}\nconf:file:{}\nblocksize:262144\nmode:par1\n{body}",
                snap_spec().config_path()
            )
        }

        fn scan_text(changed: bool) -> String {
            format!(
                "summary:equal:0\nsummary:added:{}\nsummary:removed:0\nsummary:updated:0\nsummary:moved:0\nsummary:copied:0\nsummary:restored:0\nsummary:exit:{}\n",
                u8::from(changed),
                if changed { "diff" } else { "equal" }
            )
        }

        fn parsed_log(log: &str, output: &str) -> Result<SnapraidLog, String> {
            let dir = Temp::new();
            std::fs::write(dir.0.join("log"), log).expect("log");
            std::fs::write(dir.0.join("out"), output).expect("out");
            SnapraidLog::parse(
                File::open(dir.0.join("log")).expect("log"),
                File::open(dir.0.join("out")).expect("out"),
            )
        }

        #[test]
        fn manual_parser_accepts_empty_and_unchanged_sync_without_inventing_errors() {
            let empty = log_text("sync", &scan_text(false));
            let log = parsed_log(&empty, "Initializing...\nNothing to do\n").expect("parse");
            let mut run = run_record(ElasticSnapraidKind::Sync);
            log.apply(&mut run, &snap_spec(), true).expect("empty sync");
            assert_eq!(run.total_blocks, Some(0));
            assert_eq!(
                (run.errors_file, run.errors_io, run.errors_data),
                (None, None, None)
            );
            assert!(log
                .apply(
                    &mut run_record(ElasticSnapraidKind::Sync),
                    &snap_spec(),
                    false
                )
                .is_err());
            let nochange = log_text(
                "sync",
                &(scan_text(false)
                    + "msg:status: Nothing to do\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n"),
            );
            parsed_log(&nochange, "Nothing to do\n")
                .expect("parse")
                .apply(
                    &mut run_record(ElasticSnapraidKind::Sync),
                    &snap_spec(),
                    false,
                )
                .expect("nochange");
            let changed = nochange
                .replace("summary:added:0", "summary:added:1")
                .replace("summary:exit:equal", "summary:exit:diff")
                .replace("msg:status: Nothing to do\n", "");
            parsed_log(
                &changed,
                "1%, 0 MB\r100% completed, 18 MB accessed in 0:00\r\nEverything OK\n",
            )
            .expect("parse")
            .apply(
                &mut run_record(ElasticSnapraidKind::Sync),
                &snap_spec(),
                false,
            )
            .expect("sync CR");
        }

        #[test]
        fn manual_sync_accepts_exact_recorded_nochange_without_everything_ok() {
            let text = r#"version:none
unixtime:1788853605
time:2026-09-08 07:46:45
command:sync
argv:0:/usr/bin/snapraid
argv:1:-l
argv:2:/proc/self/fd/6
argv:3:-c
argv:4:/etc/tentanas/snapraid-e2-xfs-two.conf
argv:5:sync
selftest:
msg:progress: Self test...
conf:file:/etc/tentanas/snapraid-e2-xfs-two.conf
uuid:by-uuid:254:16:27cb314c-2120-4e95-8308-748c49464afc: found ../../vdb
blocksize:262144
data:d1:/mnt/tentanas-branches/e2-xfs-two/data/d1/
mode:par2
parity:0:/mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.parity
2-parity:0:/mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.2-parity
autosave:500000000000
filter:exclude /lost+found/
filter:exclude /tmp/
filter:exclude *.unrecoverable
filter:exclude .AppleDouble
filter:exclude ._AppleDouble
filter:exclude .DS_Store
content:/etc/tentanas/e2-xfs-two-snapraid.content
msg:progress: Loading state from /etc/tentanas/e2-xfs-two-snapraid.content...
msg:verbose:        2 files
msg:verbose:        0 hardlinks
msg:verbose:        0 symlinks
msg:verbose:        0 empty dirs
uuid:by-uuid:254:48:664d6dc0-ae24-4241-a8cc-d2f0d9f62a31: found ../../vdd
uuid:by-uuid:254:64:23387aa6-ec97-407d-b436-0b8b2d5f8f62: found ../../vde
msg:progress: Scanning...
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/data/d1/ 
msg:progress: Scanned d1 in 0 seconds
msg:verbose:        2 equal
msg:verbose:        0 added
msg:verbose:        0 removed
msg:verbose:        0 updated
msg:verbose:        0 moved
msg:verbose:        0 copied
msg:verbose:        0 restored
summary:equal:2
summary:added:0
summary:removed:0
summary:updated:0
summary:moved:0
summary:copied:0
summary:restored:0
summary:exit:equal
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/data/d1/ 
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.parity 
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.2-parity 
memory:used:258525
memory:block:17
memory:extent:88
memory:file:192
memory:link:88
memory:dir:80
msg:progress: Using 0 MiB of memory for the file-system.
msg:progress: Initializing...
msg:progress: Resizing...
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/data/d1/ 
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.parity 
statfs:xfs: /mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.2-parity 
msg:progress: Saving state to /etc/tentanas/e2-xfs-two-snapraid.content...
msg:progress: Saving state to /mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.content...
msg:progress: Saving state to /mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.content...
msg:verbose:        2 files
msg:verbose:        0 hardlinks
msg:verbose:        0 symlinks
msg:verbose:        0 empty dirs
msg:progress: Verifying...
msg:progress: Verified /etc/tentanas/e2-xfs-two-snapraid.content in 0 seconds
msg:progress: Verified /mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.content in 0 seconds
msg:progress: Verified /mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.content in 0 seconds
msg:progress: Using 48 MiB of memory for 64 cached blocks.
msg:progress: Selecting...
msg:progress: Syncing...
msg:status: Nothing to do
summary:error_file:0
summary:error_io:0
summary:error_data:0
summary:exit:ok
"#;
            let output = r#"Self test...
Loading state from /etc/tentanas/e2-xfs-two-snapraid.content...
Scanning...
Scanned d1 in 0 seconds
Using 0 MiB of memory for the file-system.
Initializing...
Resizing...
Saving state to /etc/tentanas/e2-xfs-two-snapraid.content...
Saving state to /mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.content...
Saving state to /mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.content...
Verifying...
Verified /etc/tentanas/e2-xfs-two-snapraid.content in 0 seconds
Verified /mnt/tentanas-branches/e2-xfs-two/parity/1/snapraid.content in 0 seconds
Verified /mnt/tentanas-branches/e2-xfs-two/parity/2/snapraid.content in 0 seconds
Using 48 MiB of memory for 64 cached blocks.
Selecting...
Syncing...
Nothing to do
"#;
            let mut spec = snap_spec();
            spec.name = "e2-xfs-two".into();
            spec.parity.push(ParityDisk {
                index: 2,
                disk: "p2".into(),
                device: "/dev/vde".into(),
            });
            let mut run = run_record(ElasticSnapraidKind::Sync);
            parsed_log(text, output)
                .expect("rzeczywiste logi")
                .apply(&mut run, &spec, false)
                .expect("niezmieniony niepusty sync");
            assert_eq!(
                (run.total_blocks, run.checked_blocks, run.accessed_mb),
                (None, None, None)
            );
            assert_eq!(
                (run.errors_file, run.errors_io, run.errors_data),
                (Some(0), Some(0), Some(0))
            );
            for bad in [
                text.replace("msg:status: Nothing to do\n", ""),
                text.replace("summary:exit:equal\n", ""),
                text.replace("summary:exit:ok\n", ""),
                text.replace("summary:error_io:0\n", ""),
                text.replace("summary:error_io:0", "summary:error_io:1"),
                text.replace("summary:added:0", "summary:added:1"),
                text.to_string() + "msg:status: Nothing to do\n",
                text.to_string() + "summary:exit:ok\n",
            ] {
                assert!(
                    parsed_log(&bad, output)
                        .expect("log")
                        .apply(&mut run_record(ElasticSnapraidKind::Sync), &spec, false)
                        .is_err()
                );
            }
            for bad in [
                output.replace("Nothing to do\n", ""),
                output.to_string() + "Nothing to do\n",
                output.to_string() + "Everything OK\n",
                output.to_string() + "100% completed, 18 MB accessed\n",
            ] {
                assert!(
                    parsed_log(text, &bad)
                        .expect("stdout")
                        .apply(&mut run_record(ElasticSnapraidKind::Sync), &spec, false)
                        .is_err()
                );
            }
        }

        #[test]
        fn manual_scrub_proves_full_work_with_holes_and_zero_rounded_mb() {
            let text = log_text(
                "scrub",
                "block_count:9\ninfo_count:1\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n",
            );
            let output = "1%, 0 MB\r100% completed, 0 MB accessed in 0:00\r\nEverything OK\r\n";
            let mut run = run_record(ElasticSnapraidKind::Scrub);
            parsed_log(&text, output)
                .expect("parse")
                .apply(&mut run, &snap_spec(), false)
                .expect("full one block");
            assert_eq!(
                (run.total_blocks, run.checked_blocks, run.accessed_mb),
                (Some(9), Some(1), Some(0))
            );
            for bad in [
                text.replace("info_count:1", "info_count:10"),
                text.replace("info_count:1", "info_count:0"),
                text.replace("error_io:0", "error_io:1"),
                text.replace("command:scrub", "command:sync"),
                text.replace("mode:par1", "mode:par2"),
                text.replace("summary:exit:ok\n", ""),
                text.clone() + "summary:exit:ok\n",
                text.clone() + "block_count:9\n",
                text.replace("media.conf", "other.conf"),
            ] {
                assert!(
                    parsed_log(&bad, output)
                        .expect("parse")
                        .apply(
                            &mut run_record(ElasticSnapraidKind::Scrub),
                            &snap_spec(),
                            false
                        )
                        .is_err(),
                    "{bad}"
                );
            }
            assert!(parsed_log(&text, &output.replace("100%", "99%"))
                .expect("parse")
                .apply(&mut run, &snap_spec(), false)
                .is_err());
        }

        #[test]
        fn manual_result_persists_known_failure_without_clearing_pending_or_sync() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for code in [Some(0), Some(1), None] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut journal = legacy_journal(&root);
                journal.stage = ElasticStage::Ready;
                journal.sync_completed_at = Some("2026-09-08T10:00:00Z".into());
                root.save(&journal).expect("save");
                let body = "block_count:9\ninfo_count:1\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n";
                let result = perform_maintenance(
                    &root,
                    &mut journal,
                    run_record(ElasticSnapraidKind::Scrub),
                    &snap_spec(),
                    || {
                        let saved = root.load(&spec().array_id).expect("pending before child");
                        assert!(matches!(
                            saved.pending,
                            Some(Pending::Maintenance {
                                kind: ElasticSnapraidKind::Scrub,
                                ..
                            })
                        ));
                        Ok((
                            code,
                            parsed_log(
                                &log_text("scrub", body),
                                "100% completed, 0 MB accessed\nEverything OK\n",
                            ),
                        ))
                    },
                    || Ok(false),
                    false,
                )
                .expect("typed result");
                assert_eq!(result.exit_code, code);
                assert_eq!(
                    journal.sync_completed_at.as_deref(),
                    Some("2026-09-08T10:00:00Z")
                );
                drop(root);
                let root = Root::open(&dir.0, uid).expect("reopen");
                let persisted = root.load(&spec().array_id).expect("saved result");
                assert_eq!(persisted.last_run.as_ref(), Some(&result));
                if code == Some(0) {
                    assert_eq!(result.outcome, ElasticSnapraidOutcome::Succeeded);
                    assert!(persisted.pending.is_none());
                } else {
                    assert_eq!(persisted.stage, ElasticStage::NeedsAttention);
                    assert!(persisted.pending.is_some());
                    let restored = restore(&root, persisted, None, None).expect("typed restore refusal");
                    assert_eq!(restored.stage, ElasticStage::NeedsAttention);
                    assert!(root
                        .load(&spec().array_id)
                        .expect("preserved")
                        .pending
                        .is_some());
                }
            }
        }

        #[test]
        fn mover_sync_hold_result_persists_success_and_failure_states() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for code in [Some(0), Some(1), None] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut journal = ready_service_journal(&root);
                journal.stage = ElasticStage::NeedsAttention;
                journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                    mode: ElasticServiceMode::Hold,
                    operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
                });
                journal.transfer = Some(TransferJournal {
                    operation_id: "66666666-6666-4666-8666-666666666666".into(),
                    resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                    started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                    rules: MoverRules::default(), coupled_sync: true, coupled_sync_result: None, sync_attempt: 0,
                    phase: ElasticMoverPhase::Syncing, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                    evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
                });
                seed(&root, &journal);
                let body = scan_text(false) + "msg:status: Nothing to do\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n";
                let sync_log = log_text("sync", &body);
                parsed_log(&sync_log, "Nothing to do\n").expect("valid sync fixture").apply(&mut run_record(ElasticSnapraidKind::Sync), &snap_spec(), false).expect("valid sync log");
                let mut run = run_record(ElasticSnapraidKind::Sync);
                run.operation_id = "66666666-6666-4666-8666-666666666666".into();
                let result = perform_maintenance(
                    &root, &mut journal, run, &snap_spec(),
                    || Ok((code, parsed_log(&sync_log, "Nothing to do\n"))),
                    || Ok(false), true,
                ).expect("typed result");
                drop(root);
                let reopened = Root::open(&dir.0, uid).expect("reopen");
                let stored = reopened.load(&journal.spec.array_id).expect("load");
                assert_eq!(stored.private.as_ref().unwrap().service.as_ref().unwrap().mode, ElasticServiceMode::Hold);
                assert_ne!(stored.stage, ElasticStage::Ready);
                assert_eq!(stored.transfer.as_ref().unwrap().coupled_sync_result.as_ref().unwrap(), &result);
                match code {
                    Some(0) => {
                        assert_eq!(result.outcome, ElasticSnapraidOutcome::Succeeded);
                        assert!(stored.pending.is_none());
                        assert_eq!(stored.stale_parity_bytes, None);
                    }
                    Some(1) | None => {
                        let expected = if code.is_some() { ElasticSnapraidOutcome::Failed } else { ElasticSnapraidOutcome::NeedsAttention };
                        assert_eq!(result.outcome, expected);
                        // Kept as history with parity out of date, not as a pending.
                        assert!(stored.pending.is_none());
                        assert!(stored.stale_parity_bytes.is_some());
                        assert_eq!(stored.transfer.as_ref().unwrap().phase, ElasticMoverPhase::NeedsAttention);
                    }
                    _ => unreachable!(),
                }
            }
        }

        #[test]
        fn mover_sync_post_guard_failure_keeps_hold_and_marks_parity_stale() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
            });
            journal.transfer = Some(TransferJournal {
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                rules: MoverRules::default(), coupled_sync: true, coupled_sync_result: None, sync_attempt: 0,
                phase: ElasticMoverPhase::Syncing, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            let result = perform_maintenance(
                &root, &mut journal, run_record(ElasticSnapraidKind::Sync), &snap_spec(),
                || Ok((Some(0), parsed_log(&log_text("sync", &(scan_text(false) + "msg:status: Nothing to do\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n")), "Nothing to do\n"))),
                || Err("zmieniony filesystem".into()), true,
            ).expect("wynik odmowy");
            assert_ne!(result.outcome, ElasticSnapraidOutcome::Succeeded);
            assert!(journal.pending.is_none());
            assert!(journal.stale_parity_bytes.is_some());
            assert_eq!(journal.private.as_ref().unwrap().service.as_ref().unwrap().mode, ElasticServiceMode::Hold);
            assert_eq!(journal.stage, ElasticStage::NeedsAttention);
        }

        #[test]
        fn coupled_sync_attempt_is_monotonic_after_reopen() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let mut root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
            });
            journal.transfer = Some(TransferJournal {
                operation_id: "66666666-6666-4666-8666-666666666666".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                rules: MoverRules::default(), coupled_sync: true, coupled_sync_result: None, sync_attempt: 0,
                phase: ElasticMoverPhase::Syncing, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            for expected in [1, 2] {
                let mut run = run_record(ElasticSnapraidKind::Sync);
                run.operation_id = "66666666-6666-4666-8666-666666666666".into();
                assert_eq!(begin_coupled_sync(&root, &mut journal, &run).expect("attempt"), expected);
                drop(root);
                root = Root::open(&dir.0, uid).expect("reopen");
                journal = root.load(&spec().array_id).expect("load");
                assert_eq!(journal.transfer.as_ref().unwrap().sync_attempt, expected);
            }
        }

        #[test]
        fn helper_timestamp_validation_rejects_invalid_calendar_values() {
            for value in [
                "2026-02-29T00:00:00Z", "2026-04-31T00:00:00Z", "2026-01-01T00:00:60Z",
                "2026-1-01T00:00:00Z", "", "2026-01-01T00:00:00+00:00",
            ] { assert!(!valid_timestamp(value), "{value}"); }
            assert!(valid_timestamp("2024-02-29T23:59:59Z"));
        }

        #[test]
        fn begin_coupled_sync_overflow_preserves_journal_and_skips_process() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
            });
            journal.transfer = Some(TransferJournal {
                operation_id: "66666666-6666-4666-8666-666666666666".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                rules: MoverRules::default(), coupled_sync: true, coupled_sync_result: None, sync_attempt: u64::MAX,
                phase: ElasticMoverPhase::NeedsAttention, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            let path = root.path.join(format!("{}.json", journal.spec.array_id));
            let before = std::fs::read(&path).expect("bytes");
            let mut run = run_record(ElasticSnapraidKind::Sync);
            run.operation_id = "66666666-6666-4666-8666-666666666666".into();
            assert!(begin_coupled_sync(&root, &mut journal, &run).is_err());
            assert_eq!(std::fs::read(path).expect("bytes"), before);
        }

        #[test]
        fn begin_coupled_sync_save_failure_preserves_memory_and_journal() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
            });
            journal.transfer = Some(TransferJournal {
                operation_id: "66666666-6666-4666-8666-666666666666".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                rules: MoverRules::default(), coupled_sync: true, coupled_sync_result: None, sync_attempt: 0,
                phase: ElasticMoverPhase::NeedsAttention, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            let path = root.path.join(format!("{}.json", journal.spec.array_id));
            let before = std::fs::read(&path).expect("bytes");
            let mut broken_root = root;
            broken_root.path = dir.0.join("not-a-directory");
            std::fs::write(&broken_root.path, b"obcy plik").expect("fixture");
            let mut run = run_record(ElasticSnapraidKind::Sync);
            run.operation_id = "66666666-6666-4666-8666-666666666666".into();
            assert!(begin_coupled_sync(&broken_root, &mut journal, &run).is_err());
            assert_eq!(std::fs::read(path).expect("bytes"), before);
            assert_eq!(journal.transfer.as_ref().unwrap().sync_attempt, 0);
        }

        #[test]
        fn manual_pending_write_failure_never_calls_tool() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
            let target = dir.0.join(format!("{}.json", spec().array_id));
            std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o400))
                .expect("mode");
            assert!(perform_maintenance(
                &root,
                &mut journal,
                run_record(ElasticSnapraidKind::Sync),
                &snap_spec(),
                || panic!("tool after failed pending write"),
                || panic!("postguard without tool"),
                false
            )
            .is_err());
        }

        #[test]
        fn manual_capture_uses_real_child_inherited_lock_and_reopened_log_fd() {
            use std::os::unix::fs::PermissionsExt;
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let array = root.array_lock(&spec().array_id).expect("array lock");
            let program = dir.0.join("tool.py");
            let log = log_text(
                "scrub",
                "block_count:9\ninfo_count:1\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n",
            );
            let code = format!(
                "#!/usr/bin/python3\nimport sys,os,fcntl\nassert sys.argv[1]=='-l'\nf=open({:?},'rb')\ntry:\n fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\n raise RuntimeError('missing lock')\nexcept BlockingIOError: pass\nwith open(sys.argv[2],'w') as target: target.write({log:?})\nprint('1%, 0 MB\\r100% completed, 0 MB accessed\\nEverything OK')\n",
                dir.0.join(".storage.lock").to_str().expect("path")
            );
            std::fs::write(&program, code).expect("script");
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                .expect("mode");
            let (code, parsed) = capture_snapraid(
                &root,
                &array,
                &dir.0,
                "run",
                program.to_str().expect("program"),
                &["-p".into(), "full".into(), "scrub".into()],
            )
            .expect("child");
            assert_eq!(code, Some(0));
            let mut run = run_record(ElasticSnapraidKind::Scrub);
            parsed
                .expect("parse")
                .apply(&mut run, &snap_spec(), false)
                .expect("strict result");
            assert_eq!(run.checked_blocks, Some(1));
            assert!(capture_snapraid(
                &root,
                &array,
                &dir.0,
                "run",
                program.to_str().expect("program"),
                &[]
            )
            .is_err());
        }

        #[test]
        fn manual_foreign_log_never_contributes_counters() {
            let text = log_text(
                "sync",
                "block_count:99\nsummary:error_file:1\nsummary:error_io:2\nsummary:error_data:3\n",
            );
            let mut run = run_record(ElasticSnapraidKind::Scrub);
            assert!(parsed_log(&text, "")
                .expect("parse")
                .apply(&mut run, &snap_spec(), false)
                .is_err());
            assert_eq!(
                (
                    run.total_blocks,
                    run.errors_file,
                    run.errors_io,
                    run.errors_data
                ),
                (None, None, None, None)
            );
            let own = text.replace("command:sync", "command:scrub");
            assert!(parsed_log(&own, "")
                .expect("parse")
                .apply(&mut run, &snap_spec(), false)
                .is_err());
            assert_eq!(
                (run.errors_file, run.errors_io, run.errors_data),
                (Some(1), Some(2), Some(3))
            );
        }

        #[test]
        fn manual_parity_zero_guard_refuses_before_inventory_and_tools() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = legacy_journal(&root);
            journal.stage = ElasticStage::Ready;
            root.save(&journal).expect("save");
            let before =
                std::fs::read(dir.0.join(format!("{}.json", spec().array_id))).expect("before");
            assert!(
                matches!(maintenance_guard(&root, &journal), Err(error) if error == "no_parity")
            );
            assert_eq!(
                std::fs::read(dir.0.join(format!("{}.json", spec().array_id))).expect("after"),
                before
            );
        }

        #[test]
        fn journal_reservation_survives_reopen_and_refuses_repeated_create() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            assert_eq!(root.load(&spec().array_id).expect("journal").spec, spec());
            assert!(root
                .reserve(&spec(), boot(), || panic!("guard po powtórzeniu"))
                .is_err());
        }

        #[test]
        fn rejected_preflight_writes_no_reservation() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let error = root
                .reserve(&spec(), boot(), || Err("obcy podpis FS".into()))
                .expect_err("guard");
            assert_eq!(error, "obcy podpis FS");
            assert!(root.journals().expect("list").is_empty());
        }

        #[test]
        fn pending_io_failure_prevents_operation_and_reopen_prevents_second_create() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for failure in ["write", "fsync"] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
                let path = dir.0.join("readonly");
                std::fs::write(&path, b"preserve").expect("fixture");
                let operations = std::cell::Cell::new(0);
                let persisted = std::cell::Cell::new(false);
                let error = pending_operation(
                    &mut journal,
                    Pending::Format(ElasticRole::Data(1)),
                    ElasticStage::Formatting,
                    |pending| {
                        assert_eq!(pending.pending, Some(Pending::Format(ElasticRole::Data(1))));
                        persisted.set(true);
                        if failure == "write" {
                            File::open(&path)
                                .expect("readonly fd")
                                .write_all(b"overwrite")
                                .map_err(|error| error.to_string())
                        } else {
                            let mut descriptors = [-1; 2];
                            assert_eq!(
                                unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
                                0
                            );
                            let _reader = unsafe { File::from_raw_fd(descriptors[0]) };
                            let writer = unsafe { File::from_raw_fd(descriptors[1]) };
                            writer.sync_all().map_err(|error| error.to_string())
                        }
                    },
                    || {
                        operations.set(operations.get() + 1);
                        Ok(())
                    },
                )
                .expect_err("rzeczywisty błąd I/O");
                assert!(!error.is_empty());
                assert!(persisted.get());
                assert_eq!(operations.get(), 0);
                assert_eq!(std::fs::read(&path).expect("fixture"), b"preserve");
                drop(root);
                let root = Root::open(&dir.0, uid).expect("reopen");
                assert_eq!(
                    root.load(&spec().array_id).expect("stara rezerwacja").stage,
                    ElasticStage::Prepared
                );
                assert!(root
                    .reserve(&spec(), boot(), || panic!(
                        "drugi create nie może dojść do urządzeń"
                    ))
                    .is_err());
            }
        }

        #[test]
        fn persisted_format_pending_survives_failed_operation_and_refuses_second_create() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
            let calls = std::cell::Cell::new(0);
            let error = pending_operation(
                &mut journal,
                Pending::Format(ElasticRole::Data(1)),
                ElasticStage::Formatting,
                |journal| root.save(journal),
                || {
                    let stored = root
                        .load(&spec().array_id)
                        .expect("trwały pending przed operacją");
                    assert_eq!(stored.pending, Some(Pending::Format(ElasticRole::Data(1))));
                    calls.set(calls.get() + 1);
                    Err("przerwane narzędzie".into())
                },
            )
            .expect_err("błąd wykonania");
            assert_eq!(error, "przerwane narzędzie");
            assert_eq!(calls.get(), 1);
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            let stored = root.load(&spec().array_id).expect("pending po reopen");
            assert_eq!(stored.pending, Some(Pending::Format(ElasticRole::Data(1))));
            assert!(stored.formatted.is_empty());
            assert!(root
                .reserve(&spec(), boot(), || panic!("zakaz drugiego mkfs"))
                .is_err());
        }

        #[test]
        fn journals_refuse_symlinks_hardlinks_modes_and_corrupt_json() {
            for kind in ["symlink", "hardlink", "mode", "json"] {
                let dir = Temp::new();
                let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
                root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
                let path = dir.0.join(format!("{}.json", spec().array_id));
                match kind {
                    "symlink" => {
                        std::fs::rename(&path, dir.0.join("original")).expect("move");
                        symlink(dir.0.join("original"), &path).expect("symlink");
                    }
                    "hardlink" => std::fs::hard_link(&path, dir.0.join("other")).expect("hardlink"),
                    "mode" => {
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                            .expect("mode")
                    }
                    _ => std::fs::write(&path, b"{").expect("corrupt"),
                }
                assert!(root.journals().is_err(), "{kind}");
            }
        }

        #[test]
        fn node_lock_excludes_an_independent_file_descriptor() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("first");
            assert!(Root::open(&dir.0, uid).is_err());
            drop(root);
            assert!(Root::open(&dir.0, uid).is_ok());
        }

        #[test]
        fn child_keeps_mutation_lock_after_parent_closes_but_daemon_does_not() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for inherit in [true, false] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let locks = if inherit {
                    vec![root.node_lock.as_raw_fd()]
                } else {
                    vec![]
                };
                let mut child = process_command(
                    Path::new("/bin/sh"),
                    &["-c".into(), "printf ready; read value".into()],
                    true,
                    &locks,
                )
                .spawn()
                .expect("dziecko");
                let mut ready = [0u8; 5];
                child
                    .stdout
                    .as_mut()
                    .expect("stdout")
                    .read_exact(&mut ready)
                    .expect("ready");
                assert_eq!(&ready, b"ready");
                drop(root);
                let probe = Root::open(&dir.0, uid);
                assert_eq!(probe.is_err(), inherit, "dziedziczenie={inherit}");
                drop(probe);
                drop(child.stdin.take());
                child.wait_with_output().expect("koniec dziecka");
                assert!(Root::open(&dir.0, uid).is_ok());
            }
        }

        #[test]
        fn legacy_journal_load_is_byte_stable_and_topology_cannot_change() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = legacy_journal(&root);
            let path = dir.0.join(format!("{}.json", journal.spec.array_id));
            let before = std::fs::read(&path).expect("schema 1");
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            let stored = root
                .load(&journal.spec.array_id)
                .expect("publiczny journal");
            assert_eq!(stored.schema, 1);
            assert!(stored.private.is_none());
            assert_eq!(std::fs::read(&path).expect("po odczycie"), before);
            journal.schema = 2;
            journal.private = Some(PrivateTopology {
                anchor: None,
                published: false,
                service: None,
            });
            assert_eq!(
                root.save(&journal).expect_err("bez adopcji"),
                "zmiana trwałej topologii Elastic"
            );
            assert_eq!(std::fs::read(&path).expect("po odmowie"), before);
            assert!(root
                .reserve(&spec(), boot(), || panic!("ponowny Create"))
                .is_err());
            let mut other = spec();
            other.array_id = "88888888-8888-4888-8888-888888888888".into();
            other.name = "other".into();
            assert!(claims_guard(&root.journals().expect("obie topologie"), &other).is_err());
        }

        #[test]
        fn private_journal_refuses_missing_null_unknown_and_duplicate_fields() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = root
                .reserve(&spec(), boot(), || Ok(()))
                .expect("prywatna rezerwacja");
            let text = serde_json::to_string(&journal).expect("journal JSON");
            let anchor = serde_json::to_string(&anchor_fixture()).expect("anchor JSON");
            let with_anchor = text.replace("\"anchor\":null", &format!("\"anchor\":{anchor}"));
            for invalid in [
                text.replace("\"schema\":2", "\"schema\":1"),
                text.replace("\"schema\":2", "\"schema\":3"),
                text.replace(
                    "\"private\":{\"anchor\":null,\"published\":false}",
                    "\"private\":null",
                ),
                text.replace(",\"private\":{\"anchor\":null,\"published\":false}", ""),
                text.replace("\"anchor\":null,", ""),
                text.replace(",\"published\":false", ""),
                text.replace("\"schema\":2", "\"schema\":2,\"schema\":2"),
                text.replace("\"private\":", "\"private\":null,\"private\":"),
                with_anchor.replace("\"anchor\":", "\"anchor\":null,\"anchor\":"),
                with_anchor.replace("\"pid\":42", "\"pid\":42,\"pid\":42"),
                with_anchor.replace("\"pid\":42", "\"pid\":42,\"unknown\":true"),
                text.replace(
                    "\"published\":false",
                    "\"published\":false,\"unknown\":true",
                ),
                text.replace("\"stage\":\"prepared\"", "\"stage\":\"ready\""),
            ] {
                assert_ne!(invalid, text);
                assert!(decode_journal(invalid.as_bytes()).is_err(), "{invalid}");
            }
            let public = text
                .replace("\"schema\":2", "\"schema\":1")
                .replace(",\"private\":{\"anchor\":null,\"published\":false}", "");
            decode_journal(public.as_bytes()).expect("historyczny brak private");
            assert!(decode_journal(
                public
                    .replace("\"schema\":1", "\"schema\":1,\"private\":null")
                    .as_bytes()
            )
            .is_err());
            let path = dir.0.join(format!("{}.json", journal.spec.array_id));
            assert_eq!(
                std::fs::read(path).expect("oryginalny journal"),
                text.as_bytes()
            );
        }

        #[test]
        fn private_publication_persists_before_callback_and_preserves_failure_intent() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for failure in ["none", "before", "publish", "after"] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
                journal.private.as_mut().expect("private").anchor = Some(anchor_fixture());
                journal.pending = Some(Pending::Union);
                root.save(&journal).expect("kotwica przed publikacją");
                let path = dir.0.join(format!("{}.json", journal.spec.array_id));
                if failure == "before" {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
                        .expect("odmowa zapisu");
                }
                let calls = std::cell::Cell::new(0);
                let result = publish_private(&root, &mut journal, |anchor| {
                    calls.set(calls.get() + 1);
                    let saved = root
                        .load(&spec().array_id)
                        .expect("trwały zamiar przed granicą syscall");
                    assert_eq!(saved.pending, Some(Pending::Union));
                    let private = saved.private.expect("private");
                    assert_eq!(private.anchor.as_ref(), Some(anchor));
                    assert!(!private.published);
                    if failure == "publish" {
                        return Err("move_mount odmówione".into());
                    }
                    if failure == "after" {
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
                            .expect("odmowa zapisu po ACK");
                    }
                    Ok(())
                });
                assert_eq!(calls.get(), usize::from(failure != "before"));
                assert_eq!(result.is_ok(), failure == "none", "{failure}");
                assert_eq!(journal.pending.is_none(), failure == "none");
                assert_eq!(journal.private.as_ref().expect("lokalny private").published, failure == "none");
                let raw = std::fs::read(&path).expect("trwałe bajty nawet po odmowie private_file");
                let saved = decode_journal(&raw).expect("trwały format");
                assert_eq!(
                    saved.private.as_ref().expect("private").anchor,
                    Some(anchor_fixture())
                );
                assert_eq!(
                    saved.private.as_ref().expect("private").published,
                    failure == "none"
                );
                assert_eq!(saved.pending.is_none(), failure == "none");
                assert_ne!(saved.stage, ElasticStage::Ready);
                drop(root);
                let root = Root::open(&dir.0, uid).expect("reopen");
                if matches!(failure, "before" | "after") {
                    assert!(root.load(&spec().array_id).is_err());
                } else {
                    let reopened = root.load(&spec().array_id).expect("reopen journal");
                    assert_eq!(reopened.pending, saved.pending);
                }
                assert_eq!(std::fs::read(&path).expect("bez retry/zapisu rodzica"), raw);
            }
        }

        #[test]
        fn publication_authorization_reads_exact_durable_anchor_without_writes() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = root
                .reserve(&spec(), boot_id().expect("boot hosta"), || Ok(()))
                .expect("reserve");
            let mut anchor = anchor_fixture();
            anchor.boot_id = journal.boot_id.clone();
            journal.private.as_mut().expect("private").anchor = Some(anchor.clone());
            journal.pending = Some(Pending::Union);
            root.save(&journal).expect("trwała kotwica");
            let path = dir.0.join(format!("{}.json", journal.spec.array_id));
            let bytes = std::fs::read(&path).expect("przed autoryzacją");
            authorize_publication(&root, &spec(), &anchor).expect("wyłącznie potwierdzenie journala");
            let mut other = anchor.clone();
            other.pid += 1;
            assert!(authorize_publication(&root, &spec(), &other).is_err());
            assert_eq!(std::fs::read(&path).expect("po autoryzacji"), bytes);
            journal.pending = None;
            root.save(&journal).expect("brak zamiaru");
            assert!(authorize_publication(&root, &spec(), &anchor).is_err());
            journal.pending = Some(Pending::Union);
            journal.private.as_mut().expect("private").published = true;
            root.save(&journal).expect("stare potwierdzenie");
            assert!(authorize_publication(&root, &spec(), &anchor).is_err());
            let mut public = journal.clone();
            public.schema = 1;
            public.private = None;
            assert!(root.save(&public).is_err());
        }

        #[test]
        fn private_directory_traverses_only_new_overlay_descendants() {
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let before = std::fs::metadata(&dir.0).expect("parent").mode();
            let target = dir.0.join("media/data/d1");
            private_directory(&dir.0, &target, uid).expect("prywatne przodki");
            for path in [
                dir.0.join("media"),
                dir.0.join("media/data"),
                target.clone(),
            ] {
                assert_eq!(
                    std::fs::metadata(path).expect("traversable").mode() & 0o777,
                    0o711
                );
            }
            assert_eq!(std::fs::metadata(&dir.0).expect("parent po").mode(), before);
            private_directory(&dir.0, &target, uid).expect("bez zmiany istniejących praw");
            let hidden = dir.0.join("hidden");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&hidden)
                .expect("zamknięty katalog");
            assert!(private_directory(&dir.0, &hidden.join("data"), uid).is_err());
            assert_eq!(
                std::fs::metadata(&hidden).expect("bez chmod").mode() & 0o777,
                0o700
            );
            symlink(&hidden, dir.0.join("alias")).expect("symlink");
            assert!(private_directory(&dir.0, &dir.0.join("alias/data"), uid).is_err());
            assert!(private_directory(&dir.0, Path::new("/obca-sciezka"), uid).is_err());
        }

        #[test]
        fn private_restore_checkpoint_refuses_before_namespace_entry() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = root
                .reserve(&spec(), boot_id().expect("boot hosta"), || Ok(()))
                .expect("reserve");
            let command = crate::HelperCommand::ElasticRestore {
                array_id: journal.spec.array_id.clone(),
                owner: journal.spec.owner.clone(),
            };
            journal.pending = Some(Pending::Sync);
            root.save(&journal).expect("niepewny sync");
            assert_eq!(
                private_operation(&root, journal.clone(), &command).expect_err("H1 przed namespace"),
                "sync niepotwierdzony; restore nie wykonuje sync"
            );
            journal.pending = None;
            root.save(&journal).expect("brak kotwicy");
            assert_eq!(
                private_operation(&root, journal, &command).expect_err("bez fork fallback"),
                "brak kotwicy w tym samym boot; wymagany restart"
            );
        }

        #[test]
        fn restore_union_boot_matrix_survives_journal_reopen_without_writes() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for same_boot in [true, false] {
                for mounted in [true, false] {
                    let dir = Temp::new();
                    let uid = unsafe { libc::geteuid() };
                    let root = Root::open(&dir.0, uid).expect("root");
                    let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
                    journal.formatted = roles(&journal.spec);
                    journal.pending = Some(Pending::Union);
                    root.save(&journal).expect("zapis pending");
                    let path = dir.0.join(format!("{}.json", journal.spec.array_id));
                    let before = std::fs::read(&path).expect("bajty przed odczytem");
                    drop(root);
                    let root = Root::open(&dir.0, uid).expect("reopen");
                    let stored = root.load(&journal.spec.array_id).expect("trwały journal");
                    restore_checkpoint_guard(&stored).expect("bez parity i sync");
                    let current_boot = if same_boot {
                        boot()
                    } else {
                        "66666666-6666-4666-8666-666666666666".into()
                    };
                    let calls = std::cell::Cell::new(0);
                    let result = restore_mount_guard(&stored, &current_boot, || {
                        calls.set(calls.get() + 1);
                        Ok(mounted)
                    });
                    if same_boot && !mounted {
                        assert_eq!(
                            result.expect_err("niepewny mount w tym samym boot"),
                            "niepewne zakończenie mount w tym samym boot; wymagany restart"
                        );
                    } else {
                        result.expect("dopuszczenie dalszej walidacji restore");
                    }
                    assert_eq!(calls.get(), usize::from(same_boot));
                    assert_eq!(stored.boot_id, boot());
                    assert_eq!(stored.pending, Some(Pending::Union));
                    assert_eq!(std::fs::read(&path).expect("bajty po guardzie"), before);
                }
            }
        }

        #[test]
        fn restore_checkpoint_rejections_survive_reopen_before_mount_probe() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for case in [
                "zero",
                "sync",
                "maintenance_sync",
                "maintenance_scrub",
                "no_sync",
                "synced",
            ] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut request = spec();
                if matches!(case, "no_sync" | "synced") {
                    let mut parity = request.data[0].clone();
                    parity.disk_id = "serial:parity".into();
                    parity.serial = Some("parity".into());
                    parity.expected_uuid = "77777777-7777-4777-8777-777777777777".into();
                    request.parity.push(parity);
                }
                let mut journal = root.reserve(&request, boot(), || Ok(())).expect("reserve");
                journal.formatted = roles(&journal.spec);
                journal.pending = Some(Pending::Union);
                if !matches!(case, "zero" | "no_sync") {
                    journal.sync_completed_at = Some("2026-09-08T12:00:00Z".into());
                }
                if case == "sync" {
                    journal.pending = Some(Pending::Sync);
                } else if matches!(case, "maintenance_sync" | "maintenance_scrub") {
                    let kind = if case == "maintenance_sync" {
                        ElasticSnapraidKind::Sync
                    } else {
                        ElasticSnapraidKind::Scrub
                    };
                    let run = run_record(kind);
                    journal.pending = Some(Pending::Maintenance {
                        operation_id: run.operation_id.clone(),
                        kind,
                    });
                    journal.last_run = Some(run);
                }
                root.save(&journal).expect("zapis checkpointu");
                let path = dir.0.join(format!("{}.json", request.array_id));
                let before = std::fs::read(&path).expect("bajty przed odczytem");
                drop(root);
                let root = Root::open(&dir.0, uid).expect("reopen");
                let stored = root.load(&request.array_id).expect("trwały checkpoint");
                let calls = std::cell::Cell::new(0);
                let result = restore_checkpoint_guard(&stored).and_then(|()| {
                    restore_mount_guard(&stored, &boot(), || {
                        calls.set(calls.get() + 1);
                        Ok(true)
                    })
                });
                if matches!(case, "zero" | "synced") {
                    result.expect("potwierdzony checkpoint lub brak parity");
                    assert_eq!(calls.get(), 1, "{case}");
                } else {
                    assert_eq!(
                        result.expect_err("odmowa przed sondą mount"),
                        "sync niepotwierdzony; restore nie wykonuje sync",
                        "{case}"
                    );
                    assert_eq!(calls.get(), 0, "{case}");
                }
                assert_eq!(std::fs::read(&path).expect("bajty po guardach"), before);
            }
        }

        #[test]
        fn restore_mount_guard_keeps_lazy_probe_errors_and_format_gate() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for (pending, complete) in [
                (Some(Pending::Union), true),
                (Some(Pending::Union), false),
                (Some(Pending::Format(ElasticRole::Data(1))), false),
                (Some(Pending::Mount(ElasticRole::Data(1))), true),
                (Some(Pending::Config), true),
                (None, false),
            ] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
                journal.pending = pending.clone();
                if complete {
                    journal.formatted = roles(&journal.spec);
                }
                root.save(&journal).expect("zapis stanu formatowania");
                let path = dir.0.join(format!("{}.json", journal.spec.array_id));
                let before = std::fs::read(&path).expect("bajty przed odczytem");
                drop(root);
                let root = Root::open(&dir.0, uid).expect("reopen");
                let stored = root.load(&journal.spec.array_id).expect("trwały journal");
                let calls = std::cell::Cell::new(0);
                let result = restore_mount_guard(&stored, &boot(), || {
                    calls.set(calls.get() + 1);
                    Err("odczyt mountinfo odmówiony".into())
                });
                if pending == Some(Pending::Union) {
                    assert_eq!(
                        result.expect_err("błąd sondy"),
                        "odczyt mountinfo odmówiony"
                    );
                    assert_eq!(calls.get(), 1);
                    let after_reboot =
                        restore_mount_guard(&stored, "66666666-6666-4666-8666-666666666666", || {
                            panic!("nowy boot nie odpytuje union w tym guardzie")
                        });
                    if complete {
                        after_reboot.expect("pełne formatowanie po zmianie boot");
                    } else {
                        assert_eq!(
                            after_reboot.expect_err("niepełne formatowanie po zmianie boot"),
                            "nieukończone formatowanie; restore nie formatuje"
                        );
                    }
                } else {
                    assert_eq!(calls.get(), 0);
                    if complete {
                        result.expect("pozostały pending nie wymaga sondy union");
                    } else {
                        assert_eq!(
                            result.expect_err("niepełne formatowanie"),
                            "nieukończone formatowanie; restore nie formatuje"
                        );
                    }
                }
                assert_eq!(std::fs::read(&path).expect("bajty po guardzie"), before);
            }
        }

        #[test]
        fn restore_journal_reopen_refuses_unknown_boot_without_rewriting_bytes() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for unknown_boot in ["", "unknown"] {
                let dir = Temp::new();
                let uid = unsafe { libc::geteuid() };
                let root = Root::open(&dir.0, uid).expect("root");
                let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
                journal.boot_id = unknown_boot.into();
                root.save(&journal).expect("zapis bajtów z nieznanym boot");
                let path = dir.0.join(format!("{}.json", journal.spec.array_id));
                let before = std::fs::read(&path).expect("bajty przed odczytem");
                drop(root);
                let root = Root::open(&dir.0, uid).expect("reopen");
                assert!(
                    root.load(&journal.spec.array_id).is_err(),
                    "{unknown_boot:?}"
                );
                assert_eq!(std::fs::read(&path).expect("bajty po odmowie"), before);
            }
        }

        #[test]
        fn restore_executor_refuses_mkfs_before_external_io() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
            let lock = root.array_lock(&journal.spec.array_id).expect("array lock");
            let plan = ElasticSpec {
                name: "media".into(),
                filesystem: "ext4".into(),
                data: vec![Branch {
                    disk: "d1".into(),
                    device: "/dev/vdb".into(),
                }],
                cache: vec![],
                parity: vec![],
                mergerfs: MergerfsOptions::default(),
                snapraid: SnapraidOptions::default(),
            };
            let before = std::fs::read(dir.0.join(format!("{}.json", journal.spec.array_id)))
                .expect("before");
            let error = execute_steps(
                &root,
                &lock,
                &mut journal,
                &plan,
                vec![ElasticStep::Mkfs {
                    program: "/must-not-execute".into(),
                    device: "/dev/vdb".into(),
                    filesystem: "ext4".into(),
                    label: "test".into(),
                }],
                StepMode::Restore,
                &mut LiveSteps { worker: None },
            )
            .expect_err("restore odmawia mkfs");
            assert_eq!(error, "restore nie może formatować");
            assert_eq!(
                std::fs::read(dir.0.join(format!("{}.json", journal.spec.array_id)))
                    .expect("after"),
                before
            );
        }

        #[test]
        fn immutable_plan_role_is_not_derived_from_renumbered_kernel_paths() {
            let plan = ElasticSpec {
                name: "media".into(),
                filesystem: "ext4".into(),
                data: vec![
                    Branch {
                        disk: "d1".into(),
                        device: "/dev/vdb".into(),
                    },
                    Branch {
                        disk: "d2".into(),
                        device: "/dev/vdc".into(),
                    },
                ],
                cache: vec![],
                parity: vec![],
                mergerfs: MergerfsOptions::default(),
                snapraid: SnapraidOptions::default(),
            };
            let steps = plan_create(&plan, &Tools::for_preview()).expect("plan");
            let mut desired = spec();
            let mut second = desired.data[0].clone();
            second.serial = Some("second".into());
            second.disk_id = "serial:second".into();
            second.expected_uuid = "66666666-6666-4666-8666-666666666666".into();
            desired.data.push(second);
            let renamed = vec![
                Device {
                    path: "/dev/vdc".into(),
                    kernel: "vdc".into(),
                    bytes: 32 << 30,
                    wwn: None,
                    serial: Some("test".into()),
                    major_minor: "252:32".into(),
                    occupied: false,
                },
                Device {
                    path: "/dev/vdb".into(),
                    kernel: "vdb".into(),
                    bytes: 32 << 30,
                    wwn: None,
                    serial: Some("second".into()),
                    major_minor: "252:16".into(),
                    occupied: false,
                },
            ];
            for step in steps {
                match step {
                    ElasticStep::Mkfs { device, label, .. } => {
                        let role = plan_role(&plan, &device).expect("rola");
                        assert_eq!(
                            role,
                            if label.contains("d1") {
                                ElasticRole::Data(1)
                            } else {
                                ElasticRole::Data(2)
                            }
                        );
                        let disk = role_disk(&desired, role).expect("spec roli");
                        let current = identity_device(disk, &renamed).expect("renumeracja");
                        assert_ne!(current.path, device);
                        assert_eq!(current.serial, disk.serial);
                    }
                    ElasticStep::Mount {
                        source, mountpoint, ..
                    } => {
                        let role = plan_role(&plan, &source).expect("rola");
                        assert_eq!(mountpoint, mount_path(&spec(), role));
                    }
                    _ => (),
                }
            }
        }

        #[test]
        fn immutable_identity_and_cross_owner_aliases_are_enforced() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
            journal.spec.owner.org_id = "other".into();
            assert!(root.save(&journal).is_err());
            journal.spec.array_id = "55555555-5555-4555-8555-555555555555".into();
            journal.spec.name = "other".into();
            assert!(root
                .reserve(&journal.spec, boot(), || panic!("alias przed guardem"))
                .is_err());
        }

        #[test]
        fn typed_spec_rejects_missing_identity_aliases_and_untrusted_fields() {
            let mut value = serde_json::to_value(spec()).expect("json");
            value["command"] = serde_json::json!("mkfs");
            assert!(serde_json::from_value::<ElasticCreateSpec>(value).is_err());
            let mut s = spec();
            s.data[0].serial = None;
            assert!(s.validate().is_err());
            s.data[0].wwn = Some("wwn-1".into());
            assert!(s.validate().is_ok());
            s.parity.push(s.data[0].clone());
            assert!(s.validate().is_err());
        }

        #[test]
        fn create_refuses_only_the_two_fixed_mount_roots() {
            for name in ["tentanas", "tentanas-branches"] {
                let mut spec = spec();
                spec.name = name.into();
                assert!(spec.validate().is_err(), "{name}");
            }
            for name in ["tentanas-data", "tentanas-branches2", "media"] {
                let mut spec = spec();
                spec.name = name.into();
                spec.validate().expect("niekolizyjna nazwa");
            }
        }

        #[test]
        fn namespace_overlap_compares_components_in_both_directions() {
            assert!(overlaps(Path::new("/mnt"), Path::new("/mnt/media")));
            assert!(overlaps(
                Path::new("/mnt/media/sub"),
                Path::new("/mnt/media")
            ));
            assert!(!overlaps(Path::new("/mnt/media2"), Path::new("/mnt/media")));
        }

        #[test]
        fn runtime_union_requires_matching_normalized_options_not_only_branches() {
            let mut spec = ElasticSpec {
                name: "media".into(),
                filesystem: "ext4".into(),
                data: vec![Branch {
                    disk: "d1".into(),
                    device: "/dev/vdb".into(),
                }],
                cache: vec![],
                parity: vec![],
                mergerfs: MergerfsOptions::default(),
                snapraid: SnapraidOptions::default(),
            };
            let values: BTreeMap<String, String> = [
                ("branches", spec.branch_specs().join(":")),
                ("category.create", "mfs".into()),
                ("cache.files", "off".into()),
                ("minfreespace", "21474836480".into()),
                ("moveonenospc", "mfs".into()),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v))
            .collect();
            validate_union_options(&spec, &values).expect("zgodne opcje runtime");
            for (key, value) in [
                ("branches", "/mnt/other=RW"),
                ("category.create", "ff"),
                ("cache.files", "libfuse"),
                ("minfreespace", "20G"),
                ("moveonenospc", "false"),
                ("moveonenospc", "ff"),
                ("moveonenospc", "true"),
            ] {
                let mut changed = values.clone();
                changed.insert(key.into(), value.into());
                assert!(validate_union_options(&spec, &changed).is_err(), "{key}");
            }
            spec.mergerfs.move_on_enospc = false;
            assert!(validate_union_options(&spec, &values).is_err());
            let mut disabled = values;
            disabled.insert("moveonenospc".into(), "false".into());
            validate_union_options(&spec, &disabled).expect("wyłączone moveonenospc");
        }

        #[test]
        fn imported_namespace_is_checked_as_a_whole_before_first_mount() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journals = vec![root.reserve(&spec(), boot(), || Ok(())).expect("reserve")];
            let mut datasets = vec![
                ZfsDataset {
                    name: "tank".into(),
                    mountpoint: "/mnt/tank".into(),
                    canmount: "on".into(),
                },
                ZfsDataset {
                    name: "tank/data".into(),
                    mountpoint: "/mnt/media".into(),
                    canmount: "on".into(),
                },
            ];
            let mut calls = Vec::new();
            let error = mount_imported("tank", &journals, &datasets, |name| {
                calls.push(name.to_string());
                Ok(())
            })
            .expect_err("konflikt przed mount");
            assert!(error.contains("pozostaje zaimportowana po import -N"));
            assert!(calls.is_empty());
            datasets[1].mountpoint = "/mnt/tank/data".into();
            mount_imported("tank", &journals, &datasets, |name| {
                calls.push(name.to_string());
                Ok(())
            })
            .expect("import");
            assert_eq!(calls, ["tank", "tank/data"]);
            for path in ["/mnt", "/mnt/tentanas-branches", "/mnt/media/child"] {
                assert!(zfs_path_guard(&journals, path).is_err(), "{path}");
            }
            assert!(zfs_path_guard(&journals, "/mnt/media2").is_ok());
        }

        fn ready_service_journal(root: &Root) -> Journal {
            let mut journal = root.reserve(&spec(), boot(), || Ok(())).expect("reserve");
            journal.formatted = vec![ElasticRole::Data(1)];
            journal.stage = ElasticStage::Ready;
            journal.private.as_mut().unwrap().anchor = Some(anchor_fixture());
            journal.private.as_mut().unwrap().published = true;
            journal.transfer = None;
            root.save(&journal).expect("ready");
            journal
        }

        #[test]
        fn service_hold_pending_false_survives_reopen() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: false,
            });
            root.save(&journal).expect("hold");
            drop(root);
            let reopened = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("reopen");
            assert!(!reopened.load(&journal.spec.array_id).unwrap().private.unwrap().service.unwrap().pending);
        }

        #[test]
        fn enter_service_callback_observes_durable_hold_pending() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = ready_service_journal(&root);
            let operation = "55555555-5555-4555-8555-555555555555";
            let array_lock = root.array_lock(&journal.spec.array_id).expect("lock");
            enter_service_with(&root, journal, operation, &array_lock, || {
                let stored = root.load(&spec().array_id).expect("load");
                let service = stored.private.unwrap().service.unwrap();
                assert_eq!(service.mode, ElasticServiceMode::Hold);
                assert!(service.pending);
                Ok(())
            }).expect("hold");
        }

        #[test]
        fn enter_service_callback_error_preserves_hold() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = ready_service_journal(&root);
            let operation = "55555555-5555-4555-8555-555555555555";
            let array_lock = root.array_lock(&journal.spec.array_id).expect("lock");
            assert!(enter_service_with(&root, journal, operation, &array_lock, || Err("ro failed".into())).is_err());
            let stored = root.load(&spec().array_id).expect("load");
            assert!(stored.private.unwrap().service.unwrap().pending);
        }

        #[test]
        fn service_guard_rejects_foreign_pending_without_write() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.pending = Some(Pending::Maintenance {
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                kind: ElasticSnapraidKind::Sync,
            });
            assert!(service_guard(&journal, "66666666-6666-4666-8666-666666666666").is_err());
        }

        #[test]
        fn service_guard_allows_online_ready_for_new_hold() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Online,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: false,
            });
            assert!(service_guard(&journal, "66666666-6666-4666-8666-666666666666").is_ok());
        }

        #[test]
        fn authorize_resume_persists_online_with_new_operation_for_both_hold_pending_states() {
            for pending in [false, true] {
                let dir = Temp::new();
                let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
                let mut journal = ready_service_journal(&root);
                journal.stage = ElasticStage::NeedsAttention;
                journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                    mode: ElasticServiceMode::Hold,
                    operation_id: "55555555-5555-4555-8555-555555555555".into(),
                    pending,
                });
                root.save(&journal).expect("hold");
                authorize_resume(&root, &mut journal, "66666666-6666-4666-8666-666666666666")
                    .expect("resume authorize");
                let stored = root.load(&journal.spec.array_id).expect("load");
                let service = stored.private.unwrap().service.unwrap();
                assert_eq!(service.mode, ElasticServiceMode::Online);
                assert!(service.pending);
                assert_eq!(service.operation_id, "66666666-6666-4666-8666-666666666666");
            }
        }

        #[test]
        fn authorize_resume_rejects_ordinary_pending_without_mutating_journal() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: false,
            });
            journal.pending = Some(Pending::Union);
            root.save(&journal).expect("pending fixture");
            let before = std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes");
            assert!(authorize_resume(&root, &mut journal, "66666666-6666-4666-8666-666666666666").is_err());
            assert_eq!(std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes"), before);
        }

        #[test]
        fn unfinished_transfer_rejects_foreign_operation_after_reopen() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: false,
            });
            journal.transfer = Some(TransferJournal {
                operation_id: "66666666-6666-4666-8666-666666666666".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                rules: MoverRules::default(), coupled_sync: false, coupled_sync_result: None, sync_attempt: 0,
                phase: ElasticMoverPhase::Moving, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            let stored = root.load(&journal.spec.array_id).expect("load");
            let before = std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes");
            let mut stored = stored;
            assert!(authorize_resume(&root, &mut stored, "88888888-8888-4888-8888-888888888888").is_err());
            assert_eq!(std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes"), before);
        }

        #[test]
        fn restore_guard_rejects_unfinished_transfer_after_reopen() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
            });
            journal.transfer = Some(TransferJournal {
                operation_id: "66666666-6666-4666-8666-666666666666".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: None,
                rules: MoverRules::default(), coupled_sync: false, coupled_sync_result: None, sync_attempt: 0,
                phase: ElasticMoverPhase::Moving, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            let stored = root.load(&journal.spec.array_id).expect("load");
            assert_eq!(restore_checkpoint_guard(&stored).expect_err("hold"), "Restore nie konsumuje trwałego service Hold");
        }

        #[test]
        fn completed_mover_allows_new_hold_resume_without_rewriting_history() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(), pending: false,
            });
            let history = "2026-01-01T00:00:00Z";
            journal.transfer = Some(TransferJournal {
                operation_id: "66666666-6666-4666-8666-666666666666".into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(), finished_at: Some(history.into()),
                rules: MoverRules::default(), coupled_sync: false, coupled_sync_result: None, sync_attempt: 1,
                phase: ElasticMoverPhase::Complete, target: None, sequence: 0, current: None, current_failed: None, moved_files: 0, moved_bytes: 0,
                evicted: 0,
                skipped_files: 0, skipped_bytes: 0, refused_files: 0, issues: Vec::new(), walked: false, detail: None,
            });
            seed(&root, &journal);
            drop(journal);
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            let mut reopened = root.load(&spec().array_id).expect("load");
            authorize_resume(&root, &mut reopened, "88888888-8888-4888-8888-888888888888").expect("new resume");
            let stored = root.load(&spec().array_id).expect("history");
            assert_eq!(stored.transfer.unwrap().finished_at.as_deref(), Some(history));
            assert!(stored.private.unwrap().service.unwrap().pending);
        }

        fn transfer_fixture(operation_id: &str, phase: ElasticMoverPhase, coupled_sync: bool) -> TransferJournal {
            TransferJournal {
                operation_id: operation_id.into(),
                resume_operation_id: "77777777-7777-4777-8777-777777777777".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
                finished_at: None,
                rules: MoverRules::default(),
                coupled_sync,
                coupled_sync_result: None,
                sync_attempt: 0,
                phase,
                target: None,
                sequence: 0,
                current: None,
                current_failed: None,
                moved_files: 0,
                moved_bytes: 0,
                skipped_files: 0,
                skipped_bytes: 0,
                refused_files: 0,
                issues: Vec::new(),
                walked: false,
                detail: None,
                evicted: 0,
            }
        }

        /// Writes a crafted journal state directly, the way a crash leaves one:
        /// the load-time validation applies, the save-time transition check not.
        fn seed(root: &Root, journal: &Journal) {
            validate_topology(journal).expect("spójny stan testowy");
            atomic_write(
                &root.path.join(format!("{}.json", journal.spec.array_id)),
                &serde_json::to_vec(journal).expect("json"),
                root.uid,
            )
            .expect("zapis testowy");
            root.load(&journal.spec.array_id).expect("stan testowy czytelny");
        }

        const MOVER_OPERATION: &str = "a1a1a1a1-a1a1-4a1a-8a1a-a1a1a1a1a1a1";
        const MOVER_RESUME: &str = "b2b2b2b2-b2b2-4b2b-8b2b-b2b2b2b2b2b2";
        const FOREIGN_RESUME: &str = "c3c3c3c3-c3c3-4c3c-8c3c-c3c3c3c3c3c3";
        const NEXT_OPERATION: &str = "d4d4d4d4-d4d4-4d4d-8d4d-d4d4d4d4d4d4";
        const NEXT_RESUME: &str = "e5e5e5e5-e5e5-4e5e-8e5e-e5e5e5e5e5e5";
        const NEW_BOOT: &str = "f6f6f6f6-f6f6-4f6f-8f6f-f6f6f6f6f6f6";
        const NEXT_BOOT: &str = "f7f7f7f7-f7f7-4f7f-8f7f-f7f7f7f7f7f7";

        /// Private cache and data directories standing in for mounted branches,
        /// next to a real `Root` whose array has a cache and, usually, parity.
        struct MoverBench {
            dir: Temp,
            cache: PathBuf,
            data: Vec<PathBuf>,
            spec: ElasticCreateSpec,
        }

        impl MoverBench {
            fn root_path(&self) -> PathBuf {
                self.dir.0.join("root")
            }

            fn reopen(&self) -> Root {
                Root::open(&self.root_path(), unsafe { libc::geteuid() }).expect("root")
            }

            fn journal_path(&self) -> PathBuf {
                self.root_path().join(format!("{}.json", self.spec.array_id))
            }

            fn journal_bytes(&self) -> Vec<u8> {
                std::fs::read(self.journal_path()).expect("journal")
            }

            /// The journal exactly as the disk held it when the power went.
            fn power_loss(&self, bytes: &[u8]) {
                atomic_write(&self.journal_path(), bytes, unsafe { libc::geteuid() }).expect("stan po awarii");
            }

            fn env<'a>(
                &self,
                space: &'a dyn Fn(&Path) -> Result<(u64, u64), String>,
                open: &'a dyn Fn(u32) -> Result<BTreeSet<(u64, u64)>, String>,
            ) -> MoverEnv<'a> {
                self.env_at(space, open, &boot())
            }

            fn env_at<'a>(
                &self,
                space: &'a dyn Fn(&Path) -> Result<(u64, u64), String>,
                open: &'a dyn Fn(u32) -> Result<BTreeSet<(u64, u64)>, String>,
                boot_id: &str,
            ) -> MoverEnv<'a> {
                MoverEnv {
                    cache: self.cache.clone(),
                    data: self
                        .data
                        .iter()
                        .zip(&self.spec.data)
                        .enumerate()
                        .map(|(index, (path, disk))| MoverTarget {
                            disk: format!("d{}", index + 1),
                            disk_id: disk.disk_id.clone(),
                            path: path.clone(),
                        })
                        .collect(),
                    min_free_bytes: 20 << 30,
                    space,
                    open_files: open,
                    now_ns: now_ns().expect("czas"),
                    boot_id: boot_id.into(),
                }
            }
        }

        fn mover_bench(data_disks: usize) -> (MoverBench, Root, Journal) {
            mover_bench_with(data_disks, true)
        }

        fn mover_bench_with(data_disks: usize, parity: bool) -> (MoverBench, Root, Journal) {
            let dir = Temp::new();
            let mut spec = spec();
            spec.cache = Some(cache_disk());
            if data_disks > 1 {
                spec.data.push(ElasticDiskSpec {
                    disk_id: "serial:data2".into(),
                    serial: Some("data2".into()),
                    wwn: None,
                    bytes: 32 << 30,
                    expected_uuid: "dededede-dede-4ede-8ede-dededededede".into(),
                });
            }
            if parity {
                spec.parity.push(ElasticDiskSpec {
                    disk_id: "serial:parity".into(),
                    serial: Some("parity".into()),
                    wwn: None,
                    bytes: 32 << 30,
                    expected_uuid: "99999999-9999-4999-8999-999999999999".into(),
                });
            }
            let root = Root::open(&dir.0.join("root"), unsafe { libc::geteuid() }).expect("root");
            let mut journal = root.reserve(&spec, boot(), || Ok(())).expect("reserve");
            journal.formatted = roles(&spec);
            journal.stage = ElasticStage::Ready;
            if parity {
                journal.sync_completed_at = Some("2026-09-08T11:00:00Z".into());
            }
            let private = journal.private.as_mut().expect("private");
            private.anchor = Some(anchor_fixture());
            private.published = true;
            root.save(&journal).expect("ready");
            let cache = dir.0.join("cache");
            std::fs::create_dir(&cache).expect("cache");
            let data = (1..=data_disks)
                .map(|index| {
                    let path = dir.0.join(format!("d{index}"));
                    std::fs::create_dir(&path).expect("data");
                    path
                })
                .collect();
            (MoverBench { dir, cache, data, spec }, root, journal)
        }

        fn write_aged(path: &Path, bytes: &[u8], age_secs: u64) {
            std::fs::create_dir_all(path.parent().expect("rodzic")).expect("katalog");
            std::fs::write(path, bytes).expect("plik");
            File::options()
                .write(true)
                .open(path)
                .expect("plik")
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs))
                .expect("mtime");
        }

        fn set_user_xattr(path: &Path, name: &str, value: &[u8]) {
            let path = CString::new(path.as_os_str().as_encoded_bytes()).expect("ścieżka");
            let name = CString::new(name).expect("nazwa");
            assert_eq!(
                unsafe {
                    libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0)
                },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
        }

        /// POSIX ACL xattr v2: user::rwx, user:65534:rwx, group::r-x, mask::rwx, other::r-x.
        fn posix_default_acl() -> Vec<u8> {
            let mut value = 2u32.to_le_bytes().to_vec();
            for (tag, perm, id) in [
                (0x01u16, 7u16, u32::MAX),
                (0x02, 7, 65534),
                (0x04, 5, u32::MAX),
                (0x10, 7, u32::MAX),
                (0x20, 5, u32::MAX),
            ] {
                value.extend_from_slice(&tag.to_le_bytes());
                value.extend_from_slice(&perm.to_le_bytes());
                value.extend_from_slice(&id.to_le_bytes());
            }
            value
        }

        fn posix_acl_names(path: &Path) -> Vec<String> {
            let path = CString::new(path.as_os_str().as_encoded_bytes()).expect("ścieżka");
            let mut names = vec![0u8; 4096];
            let size = unsafe { libc::listxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
            assert!(size >= 0, "{}", std::io::Error::last_os_error());
            names[..size as usize]
                .split(|byte| *byte == 0)
                .filter(|name| name.starts_with(b"system.posix_acl"))
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect()
        }

        /// Relative paths of every non-directory entry below `root`, sorted.
        fn tree(root: &Path) -> Vec<String> {
            let mut files = Vec::new();
            let mut pending = vec![(root.to_path_buf(), String::new())];
            while let Some((directory, prefix)) = pending.pop() {
                for entry in std::fs::read_dir(&directory).expect("katalog") {
                    let entry = entry.expect("wpis");
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                    if entry.file_type().expect("typ").is_dir() {
                        pending.push((entry.path(), relative));
                    } else {
                        files.push(relative);
                    }
                }
            }
            files.sort();
            files
        }

        fn roomy(_: &Path) -> Result<(u64, u64), String> {
            Ok((100 << 30, 60 << 30))
        }

        fn nothing_open(_: u32) -> Result<BTreeSet<(u64, u64)>, String> {
            Ok(BTreeSet::new())
        }

        fn move_everything_aged() -> MoverRules {
            MoverRules {
                min_age_secs: 3600,
                min_free_pct: 100,
                pinned_folders: Vec::new(),
                eager_folders: Vec::new(),
                skip_open_files: true,
            }
        }

        fn mover_request<'a>(rules: &'a MoverRules, array_lock: &'a File) -> MoverRequest<'a> {
            MoverRequest {
                operation_id: MOVER_OPERATION,
                resume_operation_id: MOVER_RESUME,
                rules,
                coupled_sync: true,
                array_lock,
            }
        }

        /// What `authorize_resume` and a successful `finish` persist once the
        /// private union is published RW again.
        fn release(root: &Root, journal: &mut Journal, resume: &str) {
            authorize_resume(root, journal, resume).expect("zapowiedziany Resume");
            let evicted = close_transfer(journal, resume).expect("zamknięcie transferu");
            journal.private.as_mut().expect("private").service.as_mut().expect("service").pending = false;
            journal.stage = ElasticStage::Ready;
            journal.detail = None;
            root.save(journal).expect("Online");
            // The same order as `finish`: durable first, then said.
            if let Some(dropped) = evicted {
                log_eviction(&dropped);
            }
        }

        fn capture(snapshot: &std::cell::RefCell<Option<Vec<u8>>>, path: &Path) -> Result<(), String> {
            *snapshot.borrow_mut() = Some(std::fs::read(path).expect("journal"));
            Err("utrata zasilania".into())
        }

        /// One shared stand-in `snapraid` for the whole test binary, written
        /// once: rewriting an executable per test would race exec with other
        /// test threads' forks (ETXTBSY). Each test steers it through its own
        /// control files, passed as the first argument after `-l <log>`.
        const FAKE_SNAPRAID: &str = "#!/bin/sh\n\
            control=\"$3\"\n\
            n=$(cat \"$control.attempts\" 2>/dev/null || echo 0)\n\
            n=$((n + 1))\n\
            echo \"$n\" > \"$control.attempts\"\n\
            if [ -f \"$control.before\" ]; then . \"$control.before\"; fi\n\
            code=$(sed -n \"${n}p\" \"$control.codes\")\n\
            cat \"$control.log\" > \"$2\"\n\
            echo 'Nothing to do'\n\
            exit \"${code:-1}\"\n";

        fn fake_snapraid_program() -> &'static Path {
            static PROGRAM: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
            PROGRAM.get_or_init(|| {
                let dir = std::env::temp_dir().join(format!("tentanas-fake-snapraid-{}", std::process::id()));
                std::fs::create_dir_all(&dir).expect("katalog");
                let program = dir.join("snapraid");
                std::fs::write(&program, FAKE_SNAPRAID).expect("skrypt");
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).expect("mode");
                program
            })
        }

        /// The control files of one test's SnapRAID: attempt N exits with
        /// `codes[N - 1]` after writing a log `capture_snapraid` parses.
        #[derive(Clone)]
        struct FakeSnapraid {
            control: PathBuf,
        }

        impl FakeSnapraid {
            fn new(dir: &Path, codes: &[i32]) -> Self {
                let snapraid = Self {
                    control: dir.join(format!("snapraid-{}", NEXT_FILE.fetch_add(1, Ordering::Relaxed))),
                };
                let codes: String = codes.iter().map(|code| format!("{code}\n")).collect();
                std::fs::write(snapraid.part("codes"), codes).expect("kody");
                let body = scan_text(false)
                    + "msg:status: Nothing to do\nsummary:error_file:0\nsummary:error_io:0\nsummary:error_data:0\nsummary:exit:ok\n";
                std::fs::write(snapraid.part("log"), log_text("sync", &body)).expect("log");
                snapraid
            }

            fn part(&self, name: &str) -> PathBuf {
                PathBuf::from(format!("{}.{name}", self.control.display()))
            }

            /// Shell run by the process before it exits, with `$n` the attempt.
            fn before(&self, snippet: &str) {
                std::fs::write(self.part("before"), snippet).expect("hook");
            }

            fn attempts(&self) -> usize {
                std::fs::read_to_string(self.part("attempts"))
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(0)
            }
        }

        /// The lowest level below `execute_steps`: devices, mount table, mount
        /// calls and the private mergerfs of one namespace. Every pending
        /// intent, save and mode decision above it runs for real.
        struct FakeSteps<'a> {
            devices: Vec<Device>,
            mounted: BTreeSet<String>,
            union: bool,
            readonly: bool,
            boot: String,
            fail_mount: Option<String>,
            /// Whether a private worker hosts the union.
            worker: bool,
            log: Vec<String>,
            published: usize,
            on_readonly: Box<dyn FnMut() -> Result<(), String> + 'a>,
        }

        impl FakeSteps<'_> {
            /// A namespace after a boot: the array's disks, nothing mounted.
            fn fresh(spec: &ElasticCreateSpec, boot: &str) -> Self {
                let devices = spec
                    .data
                    .iter()
                    .chain(spec.cache.iter())
                    .chain(&spec.parity)
                    .enumerate()
                    .map(|(index, disk)| {
                        let letter = (b'b' + index as u8) as char;
                        Device {
                            path: format!("/dev/vd{letter}"),
                            kernel: format!("vd{letter}"),
                            bytes: disk.bytes,
                            wwn: disk.wwn.clone(),
                            serial: disk.serial.clone(),
                            major_minor: format!("252:{}", index * 16),
                            occupied: false,
                        }
                    })
                    .collect();
                FakeSteps {
                    devices,
                    mounted: BTreeSet::new(),
                    union: false,
                    readonly: false,
                    boot: boot.into(),
                    fail_mount: None,
                    worker: true,
                    log: Vec::new(),
                    published: 0,
                    on_readonly: Box::new(|| Ok(())),
                }
            }

            /// The running array's namespace: every branch and the union mounted.
            fn live(spec: &ElasticCreateSpec, boot: &str) -> Self {
                let mut steps = Self::fresh(spec, boot);
                steps.mounted = roles(spec).into_iter().map(|role| mount_path(spec, role)).collect();
                steps.union = true;
                steps.readonly = true;
                steps
            }
        }

        impl StepSystem for FakeSteps<'_> {
            fn device(&mut self, disk: &ElasticDiskSpec) -> Result<Device, String> {
                self.devices
                    .iter()
                    .find(|device| device.serial == disk.serial)
                    .cloned()
                    .ok_or_else(|| "brak urządzenia macierzy".into())
            }

            fn filesystem_matches(&mut self, _: &ElasticCreateSpec, _: ElasticRole, _: &Device) -> Result<(), String> {
                Ok(())
            }

            fn branch_mounted(&mut self, spec: &ElasticCreateSpec, role: ElasticRole, _: &Device) -> Result<bool, String> {
                Ok(self.mounted.contains(&mount_path(spec, role)))
            }

            fn union_mounted(&mut self, _: &ElasticSpec) -> Result<bool, String> {
                Ok(self.union)
            }

            fn directory_empty(&mut self, _: &str) -> Result<bool, String> {
                Ok(true)
            }

            fn make_directory(&mut self, path: &str, _: u32) -> Result<(), String> {
                self.log.push(format!("mkdir {path}"));
                Ok(())
            }

            fn clean_device(&mut self, _: &Device) -> Result<(), String> {
                Err("test nie formatuje".into())
            }

            fn mount(&mut self, _: &str, _: &[String], _: &Device, mountpoint: &str, _: &[RawFd]) -> Result<(), String> {
                self.log.push(format!("mount {mountpoint}"));
                if self.fail_mount.as_deref() == Some(mountpoint) {
                    return Err("mount: Input/output error".into());
                }
                self.mounted.insert(mountpoint.into());
                Ok(())
            }

            fn run_tool(&mut self, program: &Path, _: &[String], _: &[RawFd]) -> Result<(), String> {
                Err(format!("nieoczekiwane narzędzie {}", program.display()))
            }

            fn write_file(&mut self, path: &str, _: &str, _: u32) -> Result<(), String> {
                self.log.push(format!("write {path}"));
                Ok(())
            }

            fn has_worker(&self) -> bool {
                self.worker
            }

            fn start_mergerfs(&mut self, _: &Path, _: &[String], _: &File) -> Result<Anchor, String> {
                self.log.push("mergerfs".into());
                self.union = true;
                self.readonly = false;
                Ok(Anchor { boot_id: self.boot.clone(), pid: 4242, ..anchor_fixture() })
            }

            fn publish(&mut self, _: &Anchor) -> Result<(), String> {
                self.published += 1;
                Ok(())
            }

            fn set_union_readonly(&mut self, readonly: bool) -> Result<(), String> {
                if readonly {
                    (self.on_readonly)()?;
                }
                self.log.push(format!("readonly {readonly}"));
                self.readonly = readonly;
                Ok(())
            }

            fn union_readonly(&mut self) -> Result<bool, String> {
                Ok(self.readonly)
            }

            fn config_matches(&mut self, _: &ElasticSpec, _: u32) -> Result<(), String> {
                Ok(())
            }

            fn tools(&mut self, _: &ElasticSpec) -> Result<Tools, String> {
                Ok(Tools::for_preview())
            }
        }

        /// The mover's host over fakes: the step system above, the branch
        /// check on its mount table and SnapRAID as a real process.
        struct TestHost<'a> {
            steps: FakeSteps<'a>,
            snapraid: FakeSnapraid,
            prepare_error: Option<String>,
            guard_error: Option<String>,
            on_checkpoint: Box<dyn FnMut(&TransferFile) -> Result<(), String> + 'a>,
        }

        impl<'a> TestHost<'a> {
            fn new(bench: &MoverBench, snapraid: &FakeSnapraid) -> Self {
                TestHost {
                    steps: FakeSteps::live(&bench.spec, &boot()),
                    snapraid: snapraid.clone(),
                    prepare_error: None,
                    guard_error: None,
                    on_checkpoint: Box::new(|_| Ok(())),
                }
            }

            fn after_boot(mut self, bench: &MoverBench, boot: &str) -> Self {
                self.steps = FakeSteps::fresh(&bench.spec, boot);
                self
            }

            fn with_enter(mut self, hook: impl FnMut() -> Result<(), String> + 'a) -> Self {
                self.steps.on_readonly = Box::new(hook);
                self
            }

            fn with_checkpoint(mut self, hook: impl FnMut(&TransferFile) -> Result<(), String> + 'a) -> Self {
                self.on_checkpoint = Box::new(hook);
                self
            }

            fn failing_prepare(mut self, error: &str) -> Self {
                self.prepare_error = Some(error.into());
                self
            }

            fn failing_guard(mut self, error: String) -> Self {
                self.guard_error = Some(error);
                self
            }
        }

        impl MoverHost for TestHost<'_> {
            fn steps(&mut self) -> &mut dyn StepSystem {
                &mut self.steps
            }

            fn verify_branches(&mut self, journal: &Journal) -> Result<(), String> {
                for role in roles(&journal.spec) {
                    if matches!(role, ElasticRole::Parity(_)) {
                        continue;
                    }
                    if !self.steps.mounted.contains(&mount_path(&journal.spec, role)) {
                        return Err("mover wymaga zamontowanych branchy macierzy".into());
                    }
                }
                Ok(())
            }

            fn prepare_sync(&mut self, _: &Root, _: &Journal) -> Result<(ElasticSpec, String, Vec<String>), String> {
                if let Some(error) = &self.prepare_error {
                    return Err(error.clone());
                }
                let spec = snap_spec();
                let mut args = vec![self.snapraid.control.display().to_string()];
                args.extend(snapraid_args(&spec, &SnapraidAction::Sync).map_err(|e| e.to_string())?);
                Ok((spec, fake_snapraid_program().display().to_string(), args))
            }

            fn sync_guard(&mut self, _: &Root, _: &Journal) -> Result<bool, String> {
                match &self.guard_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(false),
                }
            }

            fn checkpoint(&mut self, record: &TransferFile) -> Result<(), String> {
                (self.on_checkpoint)(record)
            }
        }

        /// Runs until the first file record of `source` reaches `phase`, then
        /// stops the run there (a failed checkpoint, not a failed syscall).
        fn stop_at(source: &'static str, phase: TransferFilePhase) -> impl FnMut(&TransferFile) -> Result<(), String> {
            let mut stopped = false;
            move |record| {
                if record.source == source && record.phase == phase && !stopped {
                    stopped = true;
                    return Err("przerwanie testowe".into());
                }
                Ok(())
            }
        }

        #[test]
        fn coupled_sync_runs_only_with_parity_and_only_until_confirmed() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            journal.stage = ElasticStage::NeedsAttention;
            journal.stale_parity_bytes = Some(5);
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            journal.transfer = Some(TransferJournal {
                resume_operation_id: MOVER_RESUME.into(),
                target: Some("d1".into()),
                sequence: 1,
                moved_files: 1,
                moved_bytes: 5,
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Syncing, true)
            });
            seed(&root, &journal);
            let mut no_parity = snap_spec();
            no_parity.parity.clear();
            run_coupled_sync(
                &root,
                &mut journal,
                &no_parity,
                MOVER_OPERATION,
                |_, _| panic!("Sync bez parity"),
                |_| panic!("guard bez Sync"),
            )
            .expect("brak parity");
            assert!(transfer_sync_owed(&journal, journal.transfer.as_ref().unwrap()));
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let rules = MoverRules::default();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            run_owed_sync(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("wymagany Sync");
            assert_eq!(snapraid.attempts(), 1, "parity bez potwierdzonego Sync musi uruchomić proces");
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut stored = root.load(&bench.spec.array_id).expect("load");
            let transfer = stored.transfer.clone().unwrap();
            assert!(!transfer_sync_owed(&stored, &transfer));
            assert_eq!(transfer.sync_attempt, 1);
            assert!(stored.pending.is_none());
            assert_eq!(stored.stale_parity_bytes, None, "udany Sync zamyka nieaktualną parity");
            run_coupled_sync(
                &root,
                &mut stored,
                &snap_spec(),
                MOVER_OPERATION,
                |_, _| panic!("powtórzony Sync"),
                |_| panic!("guard powtórzonego Sync"),
            )
            .expect("potwierdzony Sync");
        }

        #[test]
        fn mover_moves_by_rules_syncs_and_releases_hold_only_to_its_resume() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("old.bin"), b"old payload", 7200);
            write_aged(&bench.cache.join("nested/deep/older.bin"), b"older payload", 9000);
            write_aged(&bench.cache.join("young.bin"), b"young payload", 0);
            write_aged(&bench.cache.join("inbox/new.bin"), b"eager payload", 0);
            write_aged(&bench.cache.join("foto/keep.bin"), b"pinned payload", 9000);
            write_aged(&bench.cache.join("foto/inbox/keep.bin"), b"pinned wins", 9000);
            write_aged(&bench.cache.join("fotoalbum.bin"), b"prefix is no folder", 9000);
            write_aged(&bench.cache.join("lost+found/#12"), b"fsck leftover", 9000);
            std::fs::set_permissions(bench.cache.join("nested"), std::fs::Permissions::from_mode(0o750))
                .expect("mode");
            let rules = MoverRules {
                pinned_folders: vec!["foto".into()],
                eager_folders: vec!["inbox".into(), "foto/inbox".into()],
                ..move_everything_aged()
            };
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let (root_ref, array_id) = (&root, bench.spec.array_id.clone());
            let mut host = TestHost::new(&bench, &snapraid).with_enter(move || {
                let stored = root_ref.load(&array_id).expect("zamiar Hold");
                assert_eq!(stored.transfer.expect("zapowiedziany transfer").phase, ElasticMoverPhase::Holding);
                assert!(stored.private.expect("private").service.expect("service").pending);
                Ok(())
            });
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut host,
            )
            .expect("mover");
            assert_eq!(host.steps.log, vec!["readonly true".to_string()]);
            drop(host);
            assert_eq!(snapraid.attempts(), 1);
            assert_eq!(
                tree(&bench.data[0]),
                vec!["fotoalbum.bin", "inbox/new.bin", "nested/deep/older.bin", "old.bin"]
            );
            assert_eq!(
                tree(&bench.cache),
                vec!["foto/inbox/keep.bin", "foto/keep.bin", "lost+found/#12", "young.bin"]
            );
            assert_eq!(
                std::fs::read(bench.data[0].join("nested/deep/older.bin")).expect("cel"),
                b"older payload"
            );
            assert_eq!(
                std::fs::metadata(bench.data[0].join("nested")).expect("katalog").permissions().mode() & 0o7777,
                0o750
            );
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut stored = root.load(&bench.spec.array_id).expect("reopen");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!(transfer.phase, ElasticMoverPhase::Complete);
            assert!(transfer.finished_at.is_some());
            assert_eq!(transfer.target.as_deref(), Some("d1"));
            assert!(transfer.current.is_none());
            assert!(coupled_sync_success(transfer.coupled_sync_result.as_ref().expect("sync"), MOVER_OPERATION));
            assert_eq!(stored.stale_parity_bytes, None);
            let run = mover_run(&transfer, &[], 0, 0);
            assert_eq!((run.moved_files, run.skipped_files, run.refused_files), (4, 0, 0));
            assert_eq!(run.moved_bytes, (11 + 13 + 13 + 19) as u64);
            assert!(run.counts_known);
            let service = stored.private.as_ref().unwrap().service.clone().unwrap();
            assert_eq!((service.mode, service.operation_id.as_str()), (ElasticServiceMode::Hold, MOVER_OPERATION));
            let before = bench.journal_bytes();
            assert!(authorize_resume(&root, &mut stored.clone(), FOREIGN_RESUME).is_err());
            assert_eq!(bench.journal_bytes(), before);
            authorize_resume(&root, &mut stored, MOVER_RESUME).expect("zapowiedziany Resume");
            let online = root.load(&bench.spec.array_id).expect("online");
            let service = online.private.as_ref().unwrap().service.clone().unwrap();
            assert_eq!(
                (service.mode, service.pending, service.operation_id.as_str()),
                (ElasticServiceMode::Online, true, MOVER_RESUME)
            );
            restore_checkpoint_guard(&online).expect("Restore dokończy autoryzowany Resume");
        }

        #[test]
        fn mover_resumes_an_interrupted_file_without_a_second_copy() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for stop in [
                TransferFilePhase::CopyIntent,
                TransferFilePhase::CopyConfirmed,
                TransferFilePhase::RenameConfirmed,
                TransferFilePhase::UnlinkIntent,
                TransferFilePhase::UnlinkConfirmed,
            ] {
                let (bench, root, mut journal) = mover_bench(1);
                write_aged(&bench.cache.join("a.bin"), b"first", 9000);
                write_aged(&bench.cache.join("b/b.bin"), b"second", 8000);
                let rules = move_everything_aged();
                let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
                let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
                let error = run_mover(
                    &root,
                    &mut journal,
                    &mover_request(&rules, &array_lock),
                    &bench.env(&roomy, &nothing_open),
                    &mut TestHost::new(&bench, &snapraid).with_checkpoint(stop_at("b/b.bin", stop)),
                )
                .expect_err("przerwany mover");
                assert_eq!(error, "przerwanie testowe", "{stop:?}");
                assert_eq!(snapraid.attempts(), 0, "Sync nie może poprzedzać plików");
                drop(array_lock);
                drop(root);
                let root = bench.reopen();
                let mut stored = root.load(&bench.spec.array_id).expect("reopen");
                let transfer = stored.transfer.clone().expect("transfer");
                assert_eq!(transfer.phase, ElasticMoverPhase::NeedsAttention, "{stop:?}");
                assert_eq!(transfer.detail.as_deref(), Some("przerwanie testowe"));
                assert_eq!(transfer.current.as_ref().expect("plik w toku").phase, stop);
                assert!(transfer.current_failed.is_none(), "przerwanie nie jest błędem pliku");
                assert_eq!(transfer.moved_files, 1);
                assert!(!mover_run(&transfer, &[], 0, 0).counts_known);
                let before = bench.journal_bytes();
                assert!(authorize_resume(&root, &mut stored.clone(), MOVER_RESUME).is_err());
                assert!(restore_checkpoint_guard(&stored).is_err());
                let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
                let foreign = MoverRequest {
                    operation_id: FOREIGN_RESUME,
                    ..mover_request(&rules, &array_lock)
                };
                assert!(run_mover(
                    &root,
                    &mut stored.clone(),
                    &foreign,
                    &bench.env(&roomy, &nothing_open),
                    &mut TestHost::new(&bench, &snapraid),
                )
                .is_err());
                assert_eq!(bench.journal_bytes(), before, "obca operacja nie zmienia journala");
                let mut host = TestHost::new(&bench, &snapraid);
                run_mover(
                    &root,
                    &mut stored,
                    &mover_request(&rules, &array_lock),
                    &bench.env(&roomy, &nothing_open),
                    &mut host,
                )
                .unwrap_or_else(|error| panic!("wznowienie po {stop:?}: {error}"));
                assert!(host.steps.log.is_empty(), "wznowienie nie wchodzi ponownie w Hold");
                assert_eq!(tree(&bench.data[0]), vec!["a.bin", "b/b.bin"], "{stop:?}");
                assert!(tree(&bench.cache).is_empty(), "{stop:?}");
                assert_eq!(std::fs::read(bench.data[0].join("b/b.bin")).expect("cel"), b"second");
                assert_eq!(snapraid.attempts(), 1);
                let transfer = stored.transfer.clone().expect("transfer");
                assert_eq!(
                    (transfer.phase, transfer.moved_files, transfer.moved_bytes, transfer.sequence),
                    (ElasticMoverPhase::Complete, 2, 11, 2)
                );
            }
        }

        #[test]
        fn mover_retries_its_failed_coupled_sync_within_the_same_operation() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[1, 0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect_err("nieudany Sync");
            assert_eq!(snapraid.attempts(), 1);
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut stored = root.load(&bench.spec.array_id).expect("reopen");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!(transfer.phase, ElasticMoverPhase::NeedsAttention);
            let failed = transfer.coupled_sync_result.clone().expect("wynik");
            assert_eq!((failed.outcome, failed.exit_code), (ElasticSnapraidOutcome::Failed, Some(1)));
            assert!(stored.pending.is_none(), "nieudany Sync movera nie zostawia pending");
            assert_eq!(stored.stale_parity_bytes, Some(7));
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            run_mover(
                &root,
                &mut stored,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("ponowiony Sync");
            assert_eq!(snapraid.attempts(), 2);
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!((transfer.phase, transfer.sync_attempt), (ElasticMoverPhase::Complete, 2));
            assert!(stored.pending.is_none());
            assert_eq!(stored.stale_parity_bytes, None);
            authorize_resume(&root, &mut stored, MOVER_RESUME).expect("Resume po Sync");
        }

        #[test]
        fn failed_coupled_sync_releases_with_stale_parity_and_the_next_run_syncs() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[1, 0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect_err("nieudany Sync");
            let mut stored = root.load(&bench.spec.array_id).expect("load");
            assert_eq!(stored.last_run.as_ref().expect("historia Sync").outcome, ElasticSnapraidOutcome::Failed);
            assert_eq!(stored.stale_parity_bytes, Some(7));
            assert!(authorize_resume(&root, &mut stored.clone(), FOREIGN_RESUME).is_err());
            release(&root, &mut stored, MOVER_RESUME);
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut online = root.load(&bench.spec.array_id).expect("reopen");
            let closed = online.transfer.clone().expect("historia");
            assert_eq!(closed.phase, ElasticMoverPhase::NeedsAttention);
            assert!(closed.finished_at.is_some());
            assert_eq!(online.stale_parity_bytes, Some(7), "parity pozostaje nieaktualna po zwolnieniu");
            let observed = observe(&online, None);
            assert_eq!((observed.parity_stale, observed.stale_parity_bytes), (true, Some(7)));
            // A new run owes the Sync before parity is current, even with nothing to move.
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let next = MoverRequest {
                operation_id: NEXT_OPERATION,
                resume_operation_id: NEXT_RESUME,
                ..mover_request(&rules, &array_lock)
            };
            run_mover(
                &root,
                &mut online,
                &next,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("następny mover");
            assert_eq!(snapraid.attempts(), 2);
            let transfer = online.transfer.clone().expect("transfer");
            assert_eq!((transfer.phase, transfer.moved_files), (ElasticMoverPhase::Complete, 0));
            assert!(coupled_sync_success(transfer.coupled_sync_result.as_ref().expect("sync"), NEXT_OPERATION));
            assert_eq!(online.stale_parity_bytes, None);
            assert!(!observe(&online, None).parity_stale);
        }

        /// Runs a two-file mover that stops right after `a.bin` is done, then
        /// returns the reopened root and journal: one file moved, one waiting.
        fn after_first_file(bench: &MoverBench, root: Root, mut journal: Journal, snapraid: &FakeSnapraid) -> (Root, Journal) {
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            write_aged(&bench.cache.join("b.bin"), b"second", 8000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(bench, snapraid).with_checkpoint(stop_at("b.bin", TransferFilePhase::CopyIntent)),
            )
            .expect_err("przerwanie");
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let journal = root.load(&bench.spec.array_id).expect("reopen");
            (root, journal)
        }

        /// Every failure after files moved leaves the run releasable by its
        /// declared Resume, with parity marked stale.
        fn assert_released_with_stale_parity(root: &Root, journal: &mut Journal, why: &str) {
            let transfer = journal.transfer.clone().expect("transfer");
            assert!(transfer.current.is_none(), "{why}");
            assert!(transfer.moved_files > 0, "{why}");
            assert!(journal.stale_parity_bytes.is_some(), "{why}");
            assert!(authorize_resume(root, &mut journal.clone(), FOREIGN_RESUME).is_err(), "{why}");
            release(root, journal, MOVER_RESUME);
            assert!(journal.stale_parity_bytes.is_some(), "{why}");
        }

        #[test]
        fn failures_before_snapraid_starts_still_release_with_stale_parity() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let error = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).failing_prepare("brak pliku content parity"),
            )
            .expect_err("guard przed SnapRAID");
            assert_eq!(error, "brak pliku content parity");
            assert_eq!(snapraid.attempts(), 0);
            assert!(journal.transfer.as_ref().unwrap().coupled_sync_result.is_none(), "bez próby Sync");
            assert_released_with_stale_parity(&root, &mut journal, "guard");
            // The next run owes the Sync and makes parity current.
            let next = MoverRequest {
                operation_id: NEXT_OPERATION,
                resume_operation_id: NEXT_RESUME,
                ..mover_request(&rules, &array_lock)
            };
            run_mover(
                &root,
                &mut journal,
                &next,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("następny mover");
            assert_eq!(snapraid.attempts(), 1);
            assert_eq!(journal.stale_parity_bytes, None);
        }

        #[test]
        fn walk_failures_after_files_moved_still_run_the_owed_sync() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let broken_space = |_: &Path| -> Result<(u64, u64), String> { Err("statvfs: Input/output error".into()) };
            let refusing_open = |_: u32| -> Result<BTreeSet<(u64, u64)>, String> {
                Err("/proc/4242/fd: Permission denied".into())
            };
            // Mode 0 stops a user, never root, so each says what it expects.
            let as_user = unsafe { libc::geteuid() } != 0;
            // One unreadable directory is a refused entry and the walk goes on; a
            // failed descriptor scan or free-space probe stops the walk. Either
            // way the Sync the moved files owe runs before the run ends.
            for case in ["unreadable_directory", "open_files", "statvfs", "walk_and_sync"] {
                let (bench, root, journal) = mover_bench(1);
                // Only the case that fails its Sync spends a non-zero exit.
                let codes: &[i32] = if case == "walk_and_sync" { &[1] } else { &[0] };
                let snapraid = FakeSnapraid::new(&bench.dir.0, codes);
                let (root, mut journal) = after_first_file(&bench, root, journal, &snapraid);
                write_aged(&bench.cache.join("locked/c.bin"), b"third", 7000);
                if case == "unreadable_directory" {
                    std::fs::set_permissions(bench.cache.join("locked"), std::fs::Permissions::from_mode(0o000))
                        .expect("mode");
                }
                let rules = move_everything_aged();
                let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
                let space: &dyn Fn(&Path) -> Result<(u64, u64), String> =
                    if case == "statvfs" { &broken_space } else { &roomy };
                let open: &dyn Fn(u32) -> Result<BTreeSet<(u64, u64)>, String> =
                    if matches!(case, "open_files" | "walk_and_sync") { &refusing_open } else { &nothing_open };
                let result = run_mover(
                    &root,
                    &mut journal,
                    &mover_request(&rules, &array_lock),
                    &bench.env(space, open),
                    &mut TestHost::new(&bench, &snapraid),
                );
                std::fs::set_permissions(bench.cache.join("locked"), std::fs::Permissions::from_mode(0o755))
                    .expect("mode");
                let data = tree(&bench.data[0]);
                assert_eq!(data[..2], ["a.bin", "b.bin"], "{case}: plik w toku dokończony");
                assert_eq!(snapraid.attempts(), 1, "{case}: należny Sync");
                let transfer = journal.transfer.clone().expect("transfer");
                if case == "unreadable_directory" {
                    result.expect(case);
                    assert_eq!(journal.stale_parity_bytes, None, "{case}");
                    assert_eq!(transfer.phase, ElasticMoverPhase::Complete);
                    if as_user {
                        assert_eq!(data.len(), 2, "katalog bez dostępu zostaje na cache");
                        assert!(transfer.issues.iter().any(|issue| issue.path == "locked"
                            && issue.kind == ElasticMoverIssueKind::Refused
                            && issue.reason.starts_with("katalog niedostępny")), "{:?}", transfer.issues);
                    } else {
                        // Root reads the directory, so its file moves like any other.
                        assert_eq!(data, ["a.bin", "b.bin", "locked/c.bin"]);
                        assert!(!transfer.issues.iter().any(|issue| issue.path == "locked"), "{:?}", transfer.issues);
                    }
                    continue;
                }
                let error = result.expect_err(case);
                assert!(error.contains(if case == "statvfs" { "statvfs" } else { "/proc/4242/fd" }), "{error}");
                assert_eq!(data.len(), 2, "{case}");
                assert_eq!(transfer.phase, ElasticMoverPhase::NeedsAttention, "{case}");
                assert!(!mover_run(&transfer, &[], 0, 0).counts_known, "{case}");
                let issue = transfer.issues.iter().find(|issue| issue.path == ".").expect("zgłoszenie przeglądu");
                assert!(issue.reason.starts_with("przegląd cache przerwany"), "{}", issue.reason);
                assert!(authorize_resume(&root, &mut journal.clone(), FOREIGN_RESUME).is_err(), "{case}");
                if case == "walk_and_sync" {
                    // Both failures are reported, and the Sync that did not
                    // confirm parity is named as the one that left it stale.
                    assert!(error.contains("SnapRAID"), "{error}");
                    assert!(error.find("/proc/4242/fd") < error.find("SnapRAID"), "{error}");
                    let stored = root.load(&bench.spec.array_id).expect("stan");
                    assert_eq!(stored.stale_sync_operation.as_deref(), Some(MOVER_OPERATION));
                    assert!(stored.stale_parity_bytes.is_some());
                    assert!(stored.pending.is_none(), "nieudany Sync movera nie przypina pending");
                    release(&root, &mut journal, MOVER_RESUME);
                    assert!(journal.stale_parity_bytes.is_some(), "parity wciąż nieaktualna");
                } else {
                    assert_eq!(journal.stale_parity_bytes, None, "{case}");
                    assert!(coupled_sync_success(transfer.coupled_sync_result.as_ref().expect("sync"), MOVER_OPERATION), "{case}");
                    release(&root, &mut journal, MOVER_RESUME);
                    assert_eq!(journal.stale_parity_bytes, None, "{case}: parity aktualna po Resume");
                }
            }
        }

        #[test]
        fn a_sync_left_running_by_a_failed_save_is_closed_by_the_declared_resume() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            // SnapRAID succeeds, but every later journal write fails.
            snapraid.before(&format!("chmod 0500 '{}'\n", bench.root_path().display()));
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect_err("zapis po SnapRAID");
            std::fs::set_permissions(bench.root_path(), std::fs::Permissions::from_mode(0o700)).expect("mode");
            let mut stored = root.load(&bench.spec.array_id).expect("load");
            assert_eq!(stored.last_run.as_ref().unwrap().outcome, ElasticSnapraidOutcome::Running);
            assert!(stored.pending.is_some(), "stan trwały z uruchomionym Sync");
            assert!(authorize_resume(&root, &mut stored.clone(), FOREIGN_RESUME).is_err());
            release(&root, &mut stored, MOVER_RESUME);
            let online = root.load(&bench.spec.array_id).expect("online");
            assert!(online.pending.is_none());
            assert_eq!(online.last_run.as_ref().unwrap().outcome, ElasticSnapraidOutcome::NeedsAttention);
            assert!(online.stale_parity_bytes.is_some());
        }

        #[test]
        fn begin_coupled_sync_never_overwrites_another_intent() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            journal.stage = ElasticStage::SyncPending;
            journal.stale_parity_bytes = Some(5);
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            journal.transfer = Some(TransferJournal {
                resume_operation_id: MOVER_RESUME.into(),
                target: Some("d1".into()),
                sequence: 1,
                moved_files: 1,
                moved_bytes: 5,
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Syncing, true)
            });
            let mut foreign = run_record(ElasticSnapraidKind::Sync);
            foreign.operation_id = FOREIGN_RESUME.into();
            journal.last_run = Some(foreign);
            journal.pending = Some(Pending::Maintenance {
                operation_id: FOREIGN_RESUME.into(),
                kind: ElasticSnapraidKind::Sync,
            });
            seed(&root, &journal);
            let before = bench.journal_bytes();
            let mut own = run_record(ElasticSnapraidKind::Sync);
            own.operation_id = MOVER_OPERATION.into();
            assert!(begin_coupled_sync(&root, &mut journal, &own).is_err());
            assert_eq!(bench.journal_bytes(), before);
        }

        #[test]
        fn stale_parity_clears_only_with_the_sync_that_records_it() {
            let (_bench, root, mut journal) = mover_bench(1);
            let mut earlier = run_record(ElasticSnapraidKind::Sync);
            earlier.outcome = ElasticSnapraidOutcome::Succeeded;
            earlier.finished_at = Some("2026-09-08T12:00:05Z".into());
            journal.last_run = Some(earlier);
            journal.sync_completed_at = Some("2026-09-08T12:00:05Z".into());
            journal.stale_parity_bytes = Some(7);
            seed(&root, &journal);
            let mut cleared = journal.clone();
            cleared.stale_parity_bytes = None;
            assert!(root.save(&cleared).is_err(), "stary udany Sync nie zamyka nowej nieaktualności");
            let mut synced = cleared.clone();
            let mut newer = run_record(ElasticSnapraidKind::Sync);
            newer.operation_id = NEXT_OPERATION.into();
            newer.outcome = ElasticSnapraidOutcome::Succeeded;
            newer.finished_at = Some("2026-09-08T13:00:00Z".into());
            synced.last_run = Some(newer);
            synced.sync_completed_at = Some("2026-09-08T13:00:00Z".into());
            root.save(&synced).expect("zapis udanego Sync");
        }

        #[test]
        fn room_is_reserved_only_by_files_that_move() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("held.bin"), b"1", 9000);
            write_aged(&bench.cache.join("next.bin"), b"2", 8000);
            let unit = std::fs::metadata(bench.cache.join("held.bin")).expect("plik").blocks() * 512;
            let data = bench.data[0].clone();
            // Room for exactly one file above minfreespace.
            let space = move |path: &Path| -> Result<(u64, u64), String> {
                Ok((100 << 30, if path == data { (20 << 30) + unit } else { 60 << 30 }))
            };
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .stdin(File::open(bench.cache.join("held.bin")).expect("held"))
                .spawn()
                .expect("sleep");
            let pid = child.id();
            let open = move |_: u32| crate::elastic_transfer::open_file_identities_of(&[pid], pid);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let result = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&space, &open),
                &mut TestHost::new(&bench, &snapraid),
            );
            result.expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["next.bin"], "otwarty plik nie rezerwuje miejsca");
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!(transfer.issues.len(), 1);
            assert_eq!(transfer.issues[0].reason, "plik otwarty przez inny proces");
            drop(array_lock);
            drop(root);
            // Nothing that moves: no target is chosen for the operation.
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("only.bin"), b"1", 9000);
            let only = std::fs::metadata(bench.cache.join("only.bin")).expect("plik");
            let all_open = move |_: u32| -> Result<BTreeSet<(u64, u64)>, String> {
                Ok([(only.dev(), only.ino())].into_iter().collect())
            };
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &all_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover bez ruchu");
            child.kill().expect("kill");
            child.wait().expect("wait");
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!((transfer.target.clone(), transfer.sequence), (None, 0));
        }

        #[test]
        fn mover_journal_stays_bounded_for_hundreds_of_files() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            for index in 0..400 {
                let path = bench.cache.join(format!("share/{:02}/file-{index:03}.bin", index % 20));
                write_aged(&path, format!("payload {index}").as_bytes(), 9000);
                set_user_xattr(&path, "user.DOSATTRIB", &[b'd'; 120]);
            }
            // A Samba NTACL-sized attribute fits the bound; a 24 KiB one does not.
            write_aged(&bench.cache.join("ntacl.bin"), b"ntacl", 9000);
            set_user_xattr(&bench.cache.join("ntacl.bin"), "user.NTACL", &vec![b'n'; 4096]);
            write_aged(&bench.cache.join("huge.bin"), b"huge", 9000);
            set_user_xattr(&bench.cache.join("huge.bin"), "user.blob", &vec![b'h'; 24 * 1024]);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let journal_path = bench.journal_path();
            let largest = std::cell::Cell::new(0u64);
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(|_| {
                    let size = std::fs::metadata(&journal_path).expect("journal").len();
                    largest.set(largest.get().max(size));
                    Ok(())
                }),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]).len(), 401);
            assert_eq!(tree(&bench.cache), vec!["huge.bin"]);
            let stored = root.load(&bench.spec.array_id).expect("journal nadal czytelny");
            let transfer = stored.transfer.expect("transfer");
            assert_eq!((transfer.moved_files, transfer.refused_files), (401, 1));
            assert_eq!(
                transfer.issues,
                vec![ElasticMoverIssue {
                    path: "huge.bin".into(),
                    kind: ElasticMoverIssueKind::Refused,
                    reason: "rekord pliku przekroczyłby limit dziennika".into(),
                }]
            );
            // Four hundred moved files never make the durable state larger than one record.
            assert!(largest.get() > 0);
            assert!(largest.get() < 32 * 1024, "{}", largest.get());
            assert!(std::fs::metadata(&journal_path).expect("journal").len() < 16 * 1024);
        }

        #[test]
        fn mover_keeps_its_first_target_skips_what_no_longer_fits_and_still_syncs() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(2);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            write_aged(&bench.cache.join("b.bin"), b"second", 8000);
            let (d1, d2) = (bench.data[0].clone(), bench.data[1].clone());
            let space = |d1_free: u64, d2_free: u64| {
                let (d1, d2) = (d1.clone(), d2.clone());
                move |path: &Path| -> Result<(u64, u64), String> {
                    Ok((100 << 30, if path == d1 { d1_free } else if path == d2 { d2_free } else { 60 << 30 }))
                }
            };
            let d2_freest = space(30 << 30, 40 << 30);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0, 0]);
            let error = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&d2_freest, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(stop_at("a.bin", TransferFilePhase::Done)),
            )
            .expect_err("przerwanie");
            assert_eq!(error, "przerwanie testowe");
            assert_eq!(tree(&d2), vec!["a.bin"]);
            assert!(tree(&d1).is_empty());
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut stored = root.load(&bench.spec.array_id).expect("reopen");
            assert_eq!(stored.transfer.as_ref().unwrap().target.as_deref(), Some("d2"));
            // d1 is now far freer, but the operation keeps d2, which is at its floor.
            let d2_full = space(90 << 30, 20 << 30);
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            run_mover(
                &root,
                &mut stored,
                &mover_request(&rules, &array_lock),
                &bench.env(&d2_full, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("brak miejsca nie blokuje wymaganego Sync");
            assert!(tree(&d1).is_empty(), "mover nie zmienia celu w trakcie operacji");
            assert_eq!(tree(&d2), vec!["a.bin"]);
            assert_eq!(tree(&bench.cache), vec!["b.bin"]);
            assert_eq!(snapraid.attempts(), 1, "Sync dla przeniesionego a.bin");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!((transfer.phase, transfer.skipped_files), (ElasticMoverPhase::Complete, 1));
            assert_eq!(transfer.issues[0].path, "b.bin");
            assert_eq!(transfer.issues[0].kind, ElasticMoverIssueKind::Skipped);
            assert_eq!(stored.stale_parity_bytes, None);
            // A later operation chooses its own target: d1, now the freest.
            release(&root, &mut stored, MOVER_RESUME);
            let next = MoverRequest {
                operation_id: NEXT_OPERATION,
                resume_operation_id: NEXT_RESUME,
                ..mover_request(&rules, &array_lock)
            };
            run_mover(
                &root,
                &mut stored,
                &next,
                &bench.env(&d2_full, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("następna operacja");
            assert_eq!(tree(&d1), vec!["b.bin"]);
            assert_eq!(stored.transfer.as_ref().unwrap().target.as_deref(), Some("d1"));
        }

        #[test]
        fn mover_breaks_free_space_ties_by_disk_id() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(2);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let equal = |_: &Path| -> Result<(u64, u64), String> { Ok((100 << 30, 50 << 30)) };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&equal, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover");
            assert!(bench.spec.data[1].disk_id < bench.spec.data[0].disk_id);
            assert_eq!(tree(&bench.data[1]), vec!["a.bin"]);
            assert!(tree(&bench.data[0]).is_empty());
            assert_eq!(journal.transfer.as_ref().unwrap().target.as_deref(), Some("d2"));
        }

        #[test]
        fn mover_without_room_skips_every_file_and_still_runs_the_owed_sync() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            // An earlier run left parity out of date.
            journal.stale_parity_bytes = Some(4096);
            seed(&root, &journal);
            let cache = bench.cache.clone();
            let tight = move |path: &Path| -> Result<(u64, u64), String> {
                Ok((100 << 30, if path == cache { 60 << 30 } else { (20 << 30) + 1024 }))
            };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&tight, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(|_| panic!("żaden plik bez miejsca")),
            )
            .expect("brak miejsca nie jest błędem");
            assert!(tree(&bench.data[0]).is_empty());
            assert_eq!(std::fs::read(bench.cache.join("a.bin")).expect("źródło"), b"payload");
            assert_eq!(snapraid.attempts(), 1, "wymagany Sync mimo braku miejsca");
            let transfer = journal.transfer.clone().unwrap();
            assert_eq!(
                (transfer.phase, transfer.target.clone(), transfer.sequence, transfer.skipped_files),
                (ElasticMoverPhase::Complete, None, 0, 1)
            );
            assert_eq!(transfer.issues[0].kind, ElasticMoverIssueKind::Skipped);
            assert_eq!(journal.stale_parity_bytes, None);
            authorize_resume(&root, &mut journal, MOVER_RESUME).expect("Resume");
        }

        #[test]
        fn mover_fills_the_target_oldest_first_up_to_minfreespace() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("oldest.bin"), b"1", 5000);
            write_aged(&bench.cache.join("older.bin"), b"2", 4800);
            write_aged(&bench.cache.join("old.bin"), b"3", 4600);
            let unit = std::fs::metadata(bench.cache.join("oldest.bin")).expect("plik").blocks() * 512;
            assert!(unit > 0);
            for name in ["older.bin", "old.bin"] {
                assert_eq!(std::fs::metadata(bench.cache.join(name)).expect("plik").blocks() * 512, unit);
            }
            let data = bench.data[0].clone();
            // Room for exactly two of the three above minfreespace.
            let space = move |path: &Path| -> Result<(u64, u64), String> {
                Ok((100 << 30, if path == data { (20 << 30) + 2 * unit } else { 60 << 30 }))
            };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&space, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["older.bin", "oldest.bin"]);
            assert_eq!(tree(&bench.cache), vec!["old.bin"]);
            assert_eq!(journal.transfer.as_ref().unwrap().skipped_files, 1);
        }

        #[test]
        fn mover_moves_every_aged_file_whatever_min_free_pct_says() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for pct in [0u8, 50, 100] {
                let (bench, root, mut journal) = mover_bench(1);
                write_aged(&bench.cache.join("old.bin"), b"old", 9000);
                write_aged(&bench.cache.join("older.bin"), b"older", 9500);
                write_aged(&bench.cache.join("young.bin"), b"young", 0);
                write_aged(&bench.cache.join("inbox/new.bin"), b"new", 0);
                let rules = MoverRules {
                    min_free_pct: pct,
                    eager_folders: vec!["inbox".into()],
                    ..move_everything_aged()
                };
                let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
                let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
                run_mover(
                    &root,
                    &mut journal,
                    &mover_request(&rules, &array_lock),
                    &bench.env(&roomy, &nothing_open),
                    &mut TestHost::new(&bench, &snapraid),
                )
                .expect("mover");
                assert_eq!(tree(&bench.data[0]), vec!["inbox/new.bin", "old.bin", "older.bin"], "{pct}%");
                assert_eq!(tree(&bench.cache), vec!["young.bin"], "{pct}%");
            }
        }

        #[test]
        fn mover_never_moves_a_pinned_folder_whatever_the_other_rules_say() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("foto/a.bin"), b"a", 9000);
            write_aged(&bench.cache.join("foto/sub/b.bin"), b"b", 9000);
            write_aged(&bench.cache.join("fotoalbum/c.bin"), b"c", 9000);
            write_aged(&bench.cache.join("d.bin"), b"d", 9000);
            let rules = MoverRules {
                min_age_secs: 0,
                min_free_pct: 100,
                pinned_folders: vec!["foto".into()],
                eager_folders: vec!["foto/sub".into()],
                skip_open_files: true,
            };
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(|record| {
                    assert!(!under_folder(&record.source, "foto"), "{}", record.source);
                    Ok(())
                }),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["d.bin", "fotoalbum/c.bin"]);
            assert_eq!(tree(&bench.cache), vec!["foto/a.bin", "foto/sub/b.bin"]);
            assert!(journal.transfer.as_ref().unwrap().issues.is_empty());
        }

        #[test]
        fn mover_skips_open_files_and_refuses_unsupported_entries_one_by_one() {
            use std::os::unix::ffi::OsStrExt;
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("held.bin"), b"held payload", 9000);
            write_aged(&bench.cache.join("ok.bin"), b"ok", 9000);
            write_aged(&bench.cache.join("hard.bin"), b"hard", 9000);
            std::fs::hard_link(bench.cache.join("hard.bin"), bench.cache.join("alias.bin")).expect("hardlink");
            symlink("ok.bin", bench.cache.join("link.bin")).expect("symlink");
            File::create(bench.cache.join("sparse.bin")).expect("sparse").set_len(8192).expect("len");
            let fifo = CString::new(bench.cache.join("pipe").as_os_str().as_bytes()).expect("fifo");
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            std::fs::write(bench.cache.join(std::ffi::OsStr::from_bytes(b"bad\xff.bin")), b"x").expect("nazwa");
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .stdin(File::open(bench.cache.join("held.bin")).expect("held"))
                .spawn()
                .expect("sleep");
            let pid = child.id();
            let open = move |daemon: u32| {
                assert_eq!(daemon, 42, "deskryptory mergerfs z kotwicy macierzy");
                crate::elastic_transfer::open_file_identities_of(&[pid], pid)
            };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let result = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &open),
                &mut TestHost::new(&bench, &snapraid),
            );
            child.kill().expect("kill");
            child.wait().expect("wait");
            result.expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["ok.bin"]);
            assert_eq!(std::fs::read(bench.cache.join("held.bin")).expect("held"), b"held payload");
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!(transfer.phase, ElasticMoverPhase::Complete);
            assert_eq!(
                (transfer.skipped_files, transfer.skipped_bytes, transfer.refused_files),
                (1, 12, 6)
            );
            let mut issues: Vec<(String, ElasticMoverIssueKind)> = transfer
                .issues
                .iter()
                .map(|issue| (issue.path.clone(), issue.kind))
                .collect();
            issues.sort_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(
                issues,
                vec![
                    ("alias.bin".to_string(), ElasticMoverIssueKind::Refused),
                    ("bad\u{fffd}.bin".to_string(), ElasticMoverIssueKind::Refused),
                    ("hard.bin".to_string(), ElasticMoverIssueKind::Refused),
                    ("held.bin".to_string(), ElasticMoverIssueKind::Skipped),
                    ("link.bin".to_string(), ElasticMoverIssueKind::Refused),
                    ("pipe".to_string(), ElasticMoverIssueKind::Refused),
                    ("sparse.bin".to_string(), ElasticMoverIssueKind::Refused),
                ]
            );
            assert!(mover_run(&transfer, &[], 0, 0).counts_known);
        }

        #[test]
        fn mover_refuses_a_file_whose_target_parent_is_a_symlink() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("nested/x.bin"), b"nested", 9000);
            write_aged(&bench.cache.join("plain.bin"), b"plain", 9000);
            let outside = bench.dir.0.join("outside");
            std::fs::create_dir(&outside).expect("outside");
            symlink(&outside, bench.data[0].join("nested")).expect("symlink");
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["nested", "plain.bin"]);
            assert!(std::fs::symlink_metadata(bench.data[0].join("nested"))
                .expect("symlink")
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read_dir(&outside).expect("outside").count(), 0);
            assert_eq!(std::fs::read(bench.cache.join("nested/x.bin")).expect("źródło"), b"nested");
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!(transfer.refused_files, 1);
            assert_eq!(transfer.issues[0].path, "nested/x.bin");
            assert!(
                transfer.issues[0].reason.starts_with("katalog docelowy nested"),
                "{}",
                transfer.issues[0].reason
            );
            assert_eq!(transfer.sequence, 1, "odmowa zapada przed rekordem w journalu");
        }

        #[test]
        fn mover_withdraws_a_file_whose_rename_fails_and_moves_the_rest() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("nested/a.bin"), b"first", 9000);
            write_aged(&bench.cache.join("b.bin"), b"second", 8000);
            let data = bench.data[0].clone();
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            // The copy is confirmed, then its target directory stops accepting
            // entries: the rename fails with EACCES while the source is untouched.
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(move |record| {
                    if record.source == "nested/a.bin" && record.phase == TransferFilePhase::CopyConfirmed {
                        std::fs::set_permissions(data.join("nested"), std::fs::Permissions::from_mode(0o555))
                            .expect("mode");
                    }
                    Ok(())
                }),
            )
            .expect("błąd jednego pliku nie zatrzymuje movera");
            std::fs::set_permissions(bench.data[0].join("nested"), std::fs::Permissions::from_mode(0o755))
                .expect("mode");
            assert_eq!(tree(&bench.data[0]), vec!["b.bin"], "kopia wycofana, bez pliku tymczasowego");
            assert_eq!(std::fs::read(bench.cache.join("nested/a.bin")).expect("źródło"), b"first");
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!(
                (transfer.phase, transfer.moved_files, transfer.refused_files),
                (ElasticMoverPhase::Complete, 1, 1)
            );
            assert!(transfer.current.is_none());
            assert_eq!(transfer.issues[0].path, "nested/a.bin");
            assert!(transfer.issues[0].reason.starts_with("wycofano kopię"), "{}", transfer.issues[0].reason);
            authorize_resume(&root, &mut journal, MOVER_RESUME).expect("Resume po wycofaniu");
        }

        #[test]
        fn unreadable_source_after_the_rename_is_finished_by_identity() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            write_aged(&bench.cache.join("b/b.bin"), b"second", 8000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid)
                    .with_checkpoint(stop_at("b/b.bin", TransferFilePhase::RenameConfirmed)),
            )
            .expect_err("utrata zasilania po zmianie nazwy");
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut stored = root.load(&bench.spec.array_id).expect("reopen");
            // The cache disk now fails every read of that file.
            let source = std::fs::metadata(bench.cache.join("b/b.bin")).expect("źródło");
            crate::elastic_transfer::fail_content_reads(Some((source.dev(), source.ino())));
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let result = run_mover(
                &root,
                &mut stored,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            );
            crate::elastic_transfer::fail_content_reads(None);
            result.expect("dokończenie po tożsamości");
            assert!(tree(&bench.cache).is_empty(), "źródło usunięte");
            assert_eq!(std::fs::read(bench.data[0].join("b/b.bin")).expect("cel"), b"second");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!((transfer.phase, transfer.moved_files), (ElasticMoverPhase::Complete, 2));
            let issue = transfer.issues.iter().find(|issue| issue.path == "b/b.bin").expect("zgłoszenie");
            assert_eq!(issue.kind, ElasticMoverIssueKind::Attention);
            assert!(issue.reason.starts_with("źródło usunięte po tożsamości"), "{}", issue.reason);
            authorize_resume(&root, &mut stored, MOVER_RESUME).expect("Resume");
        }

        #[test]
        fn a_destination_that_fails_its_pin_leaves_a_stuck_record_the_resume_releases() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid)
                    .with_checkpoint(stop_at("a.bin", TransferFilePhase::RenameConfirmed)),
            )
            .expect_err("utrata zasilania po zmianie nazwy");
            // The copy on the data disk no longer matches, and the source is unreadable.
            std::fs::write(bench.data[0].join("a.bin"), b"FIRST").expect("zmieniony cel");
            let source = std::fs::metadata(bench.cache.join("a.bin")).expect("źródło");
            crate::elastic_transfer::fail_content_reads(Some((source.dev(), source.ino())));
            let result = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            );
            crate::elastic_transfer::fail_content_reads(None);
            let error = result.expect_err("nierozwiązany rekord");
            assert!(error.starts_with("rekord nierozwiązany"), "{error}");
            // Nothing is deleted: both copies stay for the admin.
            assert_eq!(std::fs::read(bench.cache.join("a.bin")).expect("źródło"), b"first");
            assert_eq!(std::fs::read(bench.data[0].join("a.bin")).expect("cel"), b"FIRST");
            drop(array_lock);
            drop(root);
            let root = bench.reopen();
            let mut stored = root.load(&bench.spec.array_id).expect("reopen");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!(transfer.current.as_ref().expect("rekord").phase, TransferFilePhase::RenameConfirmed);
            assert!(transfer.current_failed.as_deref().is_some_and(|reason| reason.starts_with("rekord nierozwiązany")));
            assert!(stored.stale_parity_bytes.is_some());
            assert!(transfer.issues.iter().any(|issue| issue.kind == ElasticMoverIssueKind::Attention));
            assert!(mover_run(&transfer, &[], 0, 0).detail.is_some_and(|detail| detail.contains("rekord nierozwiązany")));
            release(&root, &mut stored, MOVER_RESUME);
            let history = root.load(&bench.spec.array_id).expect("historia").transfer.expect("transfer");
            assert!(history.finished_at.is_some() && history.current.is_some() && history.current_failed.is_some());
        }

        /// Drives one file to a confirmed copy, then replaces the source so the
        /// withdrawal cannot verify it: the record sticks with its temporary
        /// still on a WRITABLE branch, which is where cleanup has to happen.
        fn stick_with_orphan(
            bench: &MoverBench,
            root: &Root,
            journal: &mut Journal,
            rules: &MoverRules,
            array_lock: &File,
            before_retry: impl FnOnce(&str),
        ) -> String {
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                root,
                journal,
                &mover_request(rules, array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(bench, &snapraid)
                    .with_checkpoint(stop_at("a.bin", TransferFilePhase::CopyConfirmed)),
            )
            .expect_err("przerwanie po potwierdzonej kopii");
            let left = tree(&bench.data[0]);
            assert_eq!(left.len(), 1, "kopia tymczasowa leży na gałęzi: {left:?}");
            before_retry(&left[0]);
            // A different inode under the same path: the withdrawal refuses it
            // before it ever reaches the copy, so the record sticks.
            std::fs::remove_file(bench.cache.join("a.bin")).expect("usuń źródło");
            write_aged(&bench.cache.join("a.bin"), b"second", 9000);
            run_mover(
                root,
                journal,
                &mover_request(rules, array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(bench, &snapraid),
            )
            .expect_err("rekord nierozwiązany")
        }

        #[test]
        fn a_verified_orphan_is_deleted_when_its_record_sticks() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let error = stick_with_orphan(&bench, &root, &mut journal, &rules, &array_lock, |_| {});
            assert!(error.contains("usunięto porzuconą kopię tymczasową"), "{error}");
            assert!(
                tree(&bench.data[0]).is_empty(),
                "sierota zniknęła z gałęzi: {:?}",
                tree(&bench.data[0])
            );
            let stored = root.load(&bench.spec.array_id).expect("stan");
            let shown = stuck_records(&stored);
            assert_eq!(shown.len(), 1, "{shown:?}");
            assert_eq!(shown[0].temporary, None, "rekord nie może wskazywać usuniętej kopii");
            assert_eq!(shown[0].temporary_copy, None);
            // The syslog line says what was removed, where no bound drops it.
            assert!(
                EVICTION_LOG.with(|log| log
                    .borrow()
                    .as_ref()
                    .is_some_and(|lines| lines.iter().any(|line| line.contains("usunięto porzuconą kopię")))),
                "log systemowy musi nazwać usuniętą kopię"
            );
        }

        /// MAJ-1: the unlink decides the outcome. A directory fsync that fails
        /// afterwards is a notice — the file is gone — and must never turn into
        /// a report of an orphan that no longer exists.
        #[test]
        fn a_failed_directory_fsync_after_the_unlink_is_a_notice_not_a_reversal() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let error = stick_with_orphan(&bench, &root, &mut journal, &rules, &array_lock, |_| {
                crate::elastic_transfer::fail_directory_fsync(true);
            });
            crate::elastic_transfer::fail_directory_fsync(false);
            assert!(error.contains("usunięto porzuconą kopię tymczasową"), "{error}");
            assert!(
                !error.contains("nie została usunięta"),
                "usunięty plik nie może być zgłoszony jako nieusunięty: {error}"
            );
            assert!(error.contains("wpis katalogu niepotwierdzony"), "{error}");
            assert!(
                tree(&bench.data[0]).is_empty(),
                "plik naprawdę zniknął: {:?}",
                tree(&bench.data[0])
            );
            let shown = stuck_records(&root.load(&bench.spec.array_id).expect("stan"));
            assert_eq!(
                shown[0].temporary, None,
                "rekord nie może wskazywać pliku, którego już nie ma"
            );
            assert_eq!(shown[0].temporary_copy, None);
        }

        #[test]
        fn an_orphan_that_is_not_ours_is_left_alone_and_still_named() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let branch = bench.data[0].clone();
            let mut name = String::new();
            let taken = &mut name;
            let error = stick_with_orphan(&bench, &root, &mut journal, &rules, &array_lock, |temporary| {
                taken.push_str(temporary);
                // Same name, different inode: not ours, so not ours to delete.
                std::fs::remove_file(branch.join(temporary)).expect("usuń kopię");
                // The SAME SIZE as the real copy (b"first"), so the size pin
                // cannot be what refuses it: the inode pin has to.
                std::fs::write(branch.join(temporary), b"OBCY!").expect("obcy plik");
            });
            assert!(error.contains("nie została usunięta"), "{error}");
            assert_eq!(
                std::fs::read(bench.data[0].join(&name)).expect("obcy plik"),
                b"OBCY!",
                "cudzy plik zostaje nietknięty"
            );
            let shown = stuck_records(&root.load(&bench.spec.array_id).expect("stan"));
            assert_eq!(
                shown[0].temporary.as_deref(),
                Some(name.as_str()),
                "rekord dalej wskazuje plik, którego nie usunęliśmy"
            );
        }

        #[test]
        fn an_orphan_already_gone_is_a_clean_outcome() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let branch = bench.data[0].clone();
            let error = stick_with_orphan(&bench, &root, &mut journal, &rules, &array_lock, |temporary| {
                // The crash window: the unlink landed, the record did not.
                std::fs::remove_file(branch.join(temporary)).expect("ktoś już posprzątał");
            });
            assert!(!error.contains("nie została usunięta"), "{error}");
            assert!(error.starts_with("rekord nierozwiązany"), "{error}");
            let shown = stuck_records(&root.load(&bench.spec.array_id).expect("stan"));
            // The record keeps its name: this run deleted nothing, and a name
            // is also how a reader recognises a copy that was renamed into
            // place rather than one that is missing.
            assert!(
                shown[0].temporary.is_some(),
                "brak pliku nie jest dowodem, że to my go usunęliśmy"
            );
        }

        /// The crash window, in the direction the ordering actually creates: the
        /// unlink is durable before the record is. A later run must read that as
        /// CLEAN — it recreates the copy and finishes the move.
        #[test]
        fn a_temporary_removed_before_its_record_lets_a_later_run_finish_the_move() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid)
                    .with_checkpoint(stop_at("a.bin", TransferFilePhase::CopyIntent)),
            )
            .expect_err("przerwanie na zamiarze kopii");
            for entry in tree(&bench.data[0]) {
                std::fs::remove_file(bench.data[0].join(entry)).expect("kopia znika przed rekordem");
            }
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("brakująca kopia to stan czysty, nie uszkodzony dziennik");
            assert_eq!(
                std::fs::read(bench.data[0].join("a.bin")).expect("plik przeniesiony"),
                b"first"
            );
            assert!(!bench.cache.join("a.bin").exists(), "źródło sprzątnięte");
            let stored = root.load(&bench.spec.array_id).expect("stan");
            assert!(stuck_records(&stored).is_empty(), "nic nie utknęło");
        }

        #[test]
        fn a_withdrawal_that_fails_leaves_a_stuck_record_the_resume_releases() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let data = bench.data[0].clone();
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            // The target disk stops accepting changes after the copy: the rename
            // fails, and so does removing the copy.
            let error = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(move |record| {
                    if record.phase == TransferFilePhase::CopyConfirmed {
                        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).expect("mode");
                    }
                    Ok(())
                }),
            )
            .expect_err("nieudane wycofanie");
            std::fs::set_permissions(&bench.data[0], std::fs::Permissions::from_mode(0o755)).expect("mode");
            assert!(error.contains("wycofanie nieudane"), "{error}");
            assert_eq!(std::fs::read(bench.cache.join("a.bin")).expect("źródło"), b"first");
            assert_eq!(tree(&bench.data[0]).len(), 1, "kopia tymczasowa zostaje");
            let mut stored = root.load(&bench.spec.array_id).expect("load");
            let transfer = stored.transfer.clone().expect("transfer");
            assert!(transfer.current.is_some() && transfer.current_failed.is_some());
            assert!(stored.stale_parity_bytes.is_some());
            release(&root, &mut stored, MOVER_RESUME);
        }

        #[test]
        fn mover_strips_acls_the_target_branch_would_add() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("nested/x.bin"), b"payload", 9000);
            set_user_xattr(&bench.data[0], "system.posix_acl_default", &posix_default_acl());
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["nested/x.bin"]);
            assert!(posix_acl_names(&bench.data[0].join("nested")).is_empty());
            assert!(posix_acl_names(&bench.data[0].join("nested/x.bin")).is_empty());
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!((transfer.phase, transfer.refused_files), (ElasticMoverPhase::Complete, 0));
        }

        #[test]
        fn restarting_mover_refuses_a_union_that_is_not_readonly_or_unmounted_branches() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, journal) = mover_bench(1);
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let (root, mut journal) = after_first_file(&bench, root, journal, &snapraid);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let before = bench.journal_bytes();
            let mut rw = TestHost::new(&bench, &snapraid);
            rw.steps.readonly = false;
            let error = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut rw,
            )
            .expect_err("unia RW");
            assert!(error.contains("unii potwierdzonej jako RO"), "{error}");
            let mut unmounted = TestHost::new(&bench, &snapraid);
            unmounted.steps.mounted.remove(&cache_branch_path("media", "c1"));
            let error = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut unmounted,
            )
            .expect_err("niezamontowany cache");
            assert!(error.contains("zamontowanych branchy"), "{error}");
            assert_eq!(bench.journal_bytes(), before);
            assert_eq!(tree(&bench.data[0]), vec!["a.bin"]);
            assert_eq!(tree(&bench.cache), vec!["b.bin"]);
        }

        #[test]
        fn mover_refuses_a_path_already_present_on_another_data_branch() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(2);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            write_aged(&bench.cache.join("b.bin"), b"second", 8000);
            std::fs::write(bench.data[1].join("a.bin"), b"stale").expect("stara kopia");
            let (d1, d2) = (bench.data[0].clone(), bench.data[1].clone());
            let space = move |path: &Path| -> Result<(u64, u64), String> {
                Ok((100 << 30, if path == d1 { 50 << 30 } else if path == d2 { 30 << 30 } else { 60 << 30 }))
            };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&space, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["b.bin"]);
            assert_eq!(std::fs::read(bench.data[1].join("a.bin")).expect("stara kopia"), b"stale");
            assert_eq!(tree(&bench.cache), vec!["a.bin"]);
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!(transfer.issues[0].path, "a.bin");
            assert_eq!(transfer.issues[0].reason, "ścieżka istnieje już na branchu d2");
        }

        #[test]
        fn coupled_sync_failure_text_is_bounded_everywhere() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).failing_guard("x".repeat(10_000)),
            )
            .expect_err("nieudany Sync");
            let stored = root.load(&bench.spec.array_id).expect("journal czytelny");
            let transfer = stored.transfer.clone().expect("transfer");
            for (field, text) in [
                ("detail", stored.detail.as_deref()),
                ("last_run", stored.last_run.as_ref().and_then(|run| run.detail.as_deref())),
                ("coupled_sync", transfer.coupled_sync_result.as_ref().and_then(|run| run.detail.as_deref())),
                ("transfer", transfer.detail.as_deref()),
            ] {
                assert!(text.is_some_and(|text| !text.is_empty() && text.len() <= 1024), "{field}");
            }
        }

        #[test]
        fn save_refuses_contradictory_transfer_transitions() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(2);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            write_aged(&bench.cache.join("b.bin"), b"second", 8000);
            let (d1, d2) = (bench.data[0].clone(), bench.data[1].clone());
            let space = move |path: &Path| -> Result<(u64, u64), String> {
                Ok((100 << 30, if path == d1 { 50 << 30 } else if path == d2 { 30 << 30 } else { 60 << 30 }))
            };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&space, &nothing_open),
                &mut TestHost::new(&bench, &snapraid)
                    .with_checkpoint(stop_at("b.bin", TransferFilePhase::RenameConfirmed)),
            )
            .expect_err("przerwanie");
            let stored = root.load(&bench.spec.array_id).expect("load");
            assert_eq!(stored.stale_parity_bytes, Some(5));
            let before = bench.journal_bytes();
            let mut variants: Vec<(&str, Journal)> = Vec::new();
            let mut variant = stored.clone();
            variant.transfer.as_mut().unwrap().current.as_mut().unwrap().phase = TransferFilePhase::CopyIntent;
            variants.push(("faza pliku wstecz", variant));
            let mut variant = stored.clone();
            variant.transfer.as_mut().unwrap().current = None;
            variants.push(("porzucony plik po zmianie nazwy", variant));
            let mut variant = stored.clone();
            variant.transfer.as_mut().unwrap().target = Some("d2".into());
            variants.push(("zmiana brancha docelowego", variant));
            let mut variant = stored.clone();
            variant.transfer.as_mut().unwrap().moved_files = 0;
            variants.push(("cofnięty licznik", variant));
            let mut variant = stored.clone();
            variant.transfer = None;
            variants.push(("usunięcie transferu", variant));
            let mut variant = stored.clone();
            variant.transfer.as_mut().unwrap().phase = ElasticMoverPhase::Holding;
            variants.push(("powrót do Holding", variant));
            let mut variant = stored.clone();
            variant.stale_parity_bytes = None;
            variants.push(("parity aktualna bez Sync", variant));
            for (why, variant) in variants {
                assert!(root.save(&variant).is_err(), "{why}");
                assert_eq!(bench.journal_bytes(), before, "{why}");
            }
            // Forward steps are still accepted.
            let mut stored = stored;
            run_mover(
                &root,
                &mut stored,
                &mover_request(&rules, &array_lock),
                &bench.env(&space, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("dokończenie");
            assert_eq!(stored.transfer.as_ref().unwrap().phase, ElasticMoverPhase::Complete);
        }

        #[test]
        fn record_bound_refuses_a_file_before_any_write_and_accepts_one_just_below() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            // Measure the record of an 8-byte name with a one-byte attribute...
            write_aged(&bench.cache.join("prob.bin"), b"p", 9000);
            set_user_xattr(&bench.cache.join("prob.bin"), "user.blob", b"x");
            let probe = crate::elastic_transfer::scan_cache(&bench.cache, |_| false)
                .expect("skan")
                .into_iter()
                .find_map(|entry| match entry {
                    ScanEntry::File(file) => Some(file),
                    ScanEntry::Refused { .. } => None,
                })
                .expect("próbka");
            let record = crate::elastic_transfer::plan_file(
                &bench.cache,
                &bench.data[0],
                "prob.bin",
                (probe.device, probe.inode),
                &format!(".tentanas-transfer-{MOVER_OPERATION}-0"),
            )
            .expect("rekord próbki");
            // ...each further attribute byte costs two hex characters.
            let base = crate::elastic_transfer::worst_case_record_size(&record).expect("rozmiar") - 2;
            std::fs::remove_file(bench.cache.join("prob.bin")).expect("usunięcie próbki");
            let fitting = (TRANSFER_RECORD_LIMIT - base) / 2;
            write_aged(&bench.cache.join("fits.bin"), b"p", 9000);
            set_user_xattr(&bench.cache.join("fits.bin"), "user.blob", &vec![b'x'; fitting - 16]);
            write_aged(&bench.cache.join("over.bin"), b"p", 8000);
            set_user_xattr(&bench.cache.join("over.bin"), "user.blob", &vec![b'x'; fitting + 16]);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover");
            assert_eq!(tree(&bench.data[0]), vec!["fits.bin"], "odrzucony plik nie zostawia zapisu");
            assert_eq!(tree(&bench.cache), vec!["over.bin"]);
            let transfer = journal.transfer.clone().expect("transfer");
            assert_eq!((transfer.moved_files, transfer.refused_files, transfer.sequence), (1, 1, 1));
            assert_eq!(transfer.issues[0].path, "over.bin");
            assert_eq!(transfer.issues[0].reason, "rekord pliku przekroczyłby limit dziennika");
        }

        /// Recovery on a fresh boot through the real `execute_steps(Recover)`:
        /// the branches and a new mergerfs come back RO, nothing is published,
        /// no mount intent is left behind and the declared Resume releases.
        fn recover_and_release(bench: &MoverBench, request_coupled: bool, snapraid: &FakeSnapraid, boot_id: &'static str) {
            let root = bench.reopen();
            let mut crashed = root.load(&bench.spec.array_id).expect("stan po awarii czytelny");
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let request = MoverRequest { coupled_sync: request_coupled, ..mover_request(&rules, &array_lock) };
            let (root_ref, array_id) = (&root, bench.spec.array_id.clone());
            let mut host = TestHost::new(bench, snapraid).after_boot(bench, boot_id).with_checkpoint(move |_| {
                let stored = root_ref.load(&array_id).map_err(|error| error.to_string())?;
                assert!(!stored.private.expect("private").published, "odtworzenie niczego nie publikuje");
                assert_eq!(stored.boot_id, boot_id);
                Ok(())
            });
            run_mover(&root, &mut crashed, &request, &bench.env_at(&roomy, &nothing_open, boot_id), &mut host)
                .unwrap_or_else(|error| panic!("odtworzenie: {error}"));
            assert_eq!(host.steps.published, 0, "tryb Recover nie publikuje");
            assert!(host.steps.log.contains(&format!("mount {}", cache_branch_path("media", "c1"))));
            let mergerfs = host.steps.log.iter().position(|step| step == "mergerfs").expect("nowy mergerfs");
            assert!(host.steps.log[mergerfs..].contains(&"readonly true".to_string()), "unia wraca jako RO");
            drop(host);
            assert!(crashed.pending.is_none(), "żadna intencja montowania nie zostaje");
            assert_eq!(crashed.transfer.as_ref().unwrap().phase, ElasticMoverPhase::Complete);
            assert_eq!(crashed.boot_id, boot_id);
            let private = crashed.private.clone().expect("private");
            assert!(!private.published);
            // Publication stays refused under the Hold; only the declared Resume releases it.
            assert!(authorize_publication(&root, &crashed.spec, private.anchor.as_ref().expect("kotwica")).is_err());
            assert!(authorize_resume(&root, &mut crashed.clone(), FOREIGN_RESUME).is_err());
            authorize_resume(&root, &mut crashed, MOVER_RESUME)
                .unwrap_or_else(|error| panic!("Resume po odtworzeniu: {error}"));
        }

        #[test]
        fn mover_recovers_after_a_fresh_boot_in_every_phase() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for stop in ["holding", "copy_confirmed", "rename_confirmed", "unlink_confirmed", "syncing"] {
                let (bench, root, mut journal) = mover_bench(1);
                write_aged(&bench.cache.join("a.bin"), b"first", 9000);
                write_aged(&bench.cache.join("b/b.bin"), b"second", 8000);
                let rules = move_everything_aged();
                let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
                // Only the syncing crash spends a SnapRAID attempt before the power loss.
                let codes: &[i32] = if stop == "syncing" { &[1, 0] } else { &[0] };
                let snapraid = FakeSnapraid::new(&bench.dir.0, codes);
                // The journal as the disk holds it at the chosen moment; the run
                // then stops, and that moment is what the power loss leaves.
                let snapshot = std::cell::RefCell::new(None);
                let journal_path = bench.journal_path();
                let (snapshot_ref, path_ref) = (&snapshot, journal_path.as_path());
                let mut host = TestHost::new(&bench, &snapraid);
                match stop {
                    "holding" => host = host.with_enter(move || capture(snapshot_ref, path_ref)),
                    "syncing" => snapraid.before(&format!(
                        "[ \"$n\" = 1 ] && cp '{}' '{}.snapshot'\n",
                        journal_path.display(),
                        journal_path.display()
                    )),
                    phase => {
                        let wanted = match phase {
                            "copy_confirmed" => TransferFilePhase::CopyConfirmed,
                            "rename_confirmed" => TransferFilePhase::RenameConfirmed,
                            _ => TransferFilePhase::UnlinkConfirmed,
                        };
                        host = host.with_checkpoint(move |record| {
                            if record.source == "b/b.bin" && record.phase == wanted {
                                return capture(snapshot_ref, path_ref);
                            }
                            Ok(())
                        });
                    }
                }
                run_mover(
                    &root,
                    &mut journal,
                    &mover_request(&rules, &array_lock),
                    &bench.env(&roomy, &nothing_open),
                    &mut host,
                )
                .expect_err("utrata zasilania");
                drop(host);
                let lost = match stop {
                    "syncing" => std::fs::read(format!("{}.snapshot", journal_path.display())).expect("migawka"),
                    _ => snapshot.take().expect("stan w chwili awarii"),
                };
                bench.power_loss(&lost);
                drop(array_lock);
                drop(root);
                recover_and_release(&bench, true, &snapraid, NEW_BOOT);
                assert_eq!(tree(&bench.data[0]), vec!["a.bin", "b/b.bin"], "{stop}");
                assert!(tree(&bench.cache).is_empty(), "{stop}");
                let stored = bench.reopen().load(&bench.spec.array_id).expect("stan");
                assert!(coupled_sync_success(
                    stored.transfer.as_ref().unwrap().coupled_sync_result.as_ref().expect("sync"),
                    MOVER_OPERATION
                ), "{stop}");
                assert_eq!(stored.stale_parity_bytes, None, "{stop}");
            }
        }

        #[test]
        fn recovery_releases_without_parity_without_coupled_sync_and_with_nothing_left() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for case in ["no_parity", "no_coupled_sync", "nothing_left"] {
                let (bench, root, mut journal) = mover_bench_with(1, case != "no_parity");
                if case != "nothing_left" {
                    write_aged(&bench.cache.join("a.bin"), b"first", 9000);
                }
                let rules = move_everything_aged();
                let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
                let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
                let request = MoverRequest {
                    coupled_sync: case != "no_coupled_sync",
                    ..mover_request(&rules, &array_lock)
                };
                let snapshot = std::cell::RefCell::new(None);
                let journal_path = bench.journal_path();
                let (snapshot_ref, path_ref) = (&snapshot, journal_path.as_path());
                let host = TestHost::new(&bench, &snapraid);
                let mut host = if case == "nothing_left" {
                    host.with_enter(move || capture(snapshot_ref, path_ref))
                } else {
                    host.with_checkpoint(move |record| {
                        if record.phase == TransferFilePhase::RenameConfirmed {
                            return capture(snapshot_ref, path_ref);
                        }
                        Ok(())
                    })
                };
                run_mover(&root, &mut journal, &request, &bench.env(&roomy, &nothing_open), &mut host)
                    .expect_err("utrata zasilania");
                drop(host);
                bench.power_loss(&snapshot.take().expect("stan w chwili awarii"));
                drop(array_lock);
                drop(root);
                recover_and_release(&bench, case != "no_coupled_sync", &snapraid, NEW_BOOT);
                assert_eq!(snapraid.attempts(), 0, "{case}: brak Sync do wykonania");
                if case != "nothing_left" {
                    assert_eq!(tree(&bench.data[0]), vec!["a.bin"], "{case}");
                }
            }
        }

        #[test]
        fn a_recovery_that_fails_after_a_mount_intent_recovers_on_the_next_boot() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let snapshot = std::cell::RefCell::new(None);
            let journal_path = bench.journal_path();
            let (snapshot_ref, path_ref) = (&snapshot, journal_path.as_path());
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(move |record| {
                    if record.phase == TransferFilePhase::CopyConfirmed {
                        return capture(snapshot_ref, path_ref);
                    }
                    Ok(())
                }),
            )
            .expect_err("utrata zasilania");
            bench.power_loss(&snapshot.take().expect("stan w chwili awarii"));
            drop(array_lock);
            drop(root);
            // First boot: the cache mount fails part-way through recovery.
            let root = bench.reopen();
            let mut crashed = root.load(&bench.spec.array_id).expect("stan po awarii");
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let mut failing = TestHost::new(&bench, &snapraid).after_boot(&bench, NEW_BOOT);
            failing.steps.fail_mount = Some(cache_branch_path("media", "c1"));
            run_mover(
                &root,
                &mut crashed,
                &mover_request(&rules, &array_lock),
                &bench.env_at(&roomy, &nothing_open, NEW_BOOT),
                &mut failing,
            )
            .expect_err("montowanie cache");
            let stored = root.load(&bench.spec.array_id).expect("stan");
            assert_eq!(stored.pending, Some(Pending::Mount(ElasticRole::Cache)));
            assert_eq!(stored.boot_id, NEW_BOOT);
            // The same boot cannot retry past the unresolved mount intent.
            let before = bench.journal_bytes();
            assert!(run_mover(
                &root,
                &mut stored.clone(),
                &mover_request(&rules, &array_lock),
                &bench.env_at(&roomy, &nothing_open, NEW_BOOT),
                &mut TestHost::new(&bench, &snapraid).after_boot(&bench, NEW_BOOT),
            )
            .is_err());
            assert_eq!(bench.journal_bytes(), before);
            drop(array_lock);
            drop(root);
            // The next boot clears the void intent and recovers.
            recover_and_release(&bench, true, &snapraid, NEXT_BOOT);
            assert_eq!(tree(&bench.data[0]), vec!["a.bin"]);
            assert!(tree(&bench.cache).is_empty());
        }

        #[test]
        fn a_walk_goes_on_past_unreadable_entries_and_the_owed_sync_runs() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            for path in ["a.bin", "locked/b.bin", "unlistable/c.bin", "d.bin", "e.bin"] {
                write_aged(&bench.cache.join(path), path.as_bytes(), 9000);
            }
            // Root ignores mode 0: the permission case is only real for a user.
            let as_user = unsafe { libc::geteuid() } != 0;
            std::fs::set_permissions(bench.cache.join("locked"), std::fs::Permissions::from_mode(0o000)).expect("mode");
            let unlistable = std::fs::metadata(bench.cache.join("unlistable")).expect("katalog");
            crate::elastic_transfer::fail_listing_of(Some((unlistable.dev(), unlistable.ino())));
            crate::elastic_transfer::fail_stat_of(Some("d.bin"));
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let result = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            );
            crate::elastic_transfer::fail_listing_of(None);
            crate::elastic_transfer::fail_stat_of(None);
            std::fs::set_permissions(bench.cache.join("locked"), std::fs::Permissions::from_mode(0o755)).expect("mode");
            result.expect("przegląd idzie dalej");
            let mut moved = vec!["a.bin", "e.bin"];
            if !as_user {
                moved.push("locked/b.bin");
            }
            assert_eq!(tree(&bench.data[0]), moved);
            assert_eq!(snapraid.attempts(), 1, "należny Sync za przeniesione pliki");
            let stored = root.load(&bench.spec.array_id).expect("stan");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!(transfer.phase, ElasticMoverPhase::Complete);
            assert!(transfer.walked);
            let refused: Vec<(&str, &str)> = transfer
                .issues
                .iter()
                .filter(|issue| issue.kind == ElasticMoverIssueKind::Refused)
                .map(|issue| (issue.path.as_str(), issue.reason.as_str()))
                .collect();
            assert!(refused.iter().any(|(path, reason)| *path == "unlistable" && reason.starts_with("listowanie katalogu")), "{refused:?}");
            assert!(refused.iter().any(|(path, reason)| *path == "d.bin" && reason.starts_with("stat:")), "{refused:?}");
            if as_user {
                assert!(refused.iter().any(|(path, reason)| *path == "locked" && reason.starts_with("katalog niedostępny")), "{refused:?}");
            }
            assert_eq!(transfer.refused_files, if as_user { 3 } else { 2 });
            assert!(coupled_sync_success(transfer.coupled_sync_result.as_ref().expect("sync"), MOVER_OPERATION));
            assert_eq!(stored.stale_parity_bytes, None);
        }

        #[test]
        fn a_restart_runs_under_the_rules_its_operation_announced() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("keep/k.bin"), b"kept", 9000);
            write_aged(&bench.cache.join("a.bin"), b"first", 8000);
            write_aged(&bench.cache.join("b.bin"), b"second", 7000);
            let announced = MoverRules { pinned_folders: vec!["keep".into()], ..move_everything_aged() };
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&announced, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(stop_at("a.bin", TransferFilePhase::CopyConfirmed)),
            )
            .expect_err("przerwanie");
            // The core lost its intent and restarts the operation with defaults.
            let forgotten = MoverRules::default();
            let restart = MoverRequest { coupled_sync: false, ..mover_request(&forgotten, &array_lock) };
            run_mover(
                &root,
                &mut journal,
                &restart,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("wznowienie tej samej operacji");
            assert_eq!(tree(&bench.cache), vec!["keep/k.bin"], "przypięty folder operacji");
            assert_eq!(tree(&bench.data[0]), vec!["a.bin", "b.bin"]);
            assert_eq!(snapraid.attempts(), 1, "sprzężenie zapowiedziane przez operację");
            let transfer = root.load(&bench.spec.array_id).expect("stan").transfer.expect("transfer");
            assert_eq!(transfer.rules, announced);
            assert!(transfer.coupled_sync);
            // Another Resume UUID is not a restart of this operation.
            let foreign = MoverRequest { resume_operation_id: FOREIGN_RESUME, ..mover_request(&announced, &array_lock) };
            assert!(run_mover(
                &root,
                &mut journal,
                &foreign,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .is_err());
        }

        #[test]
        fn recovery_steps_refuse_a_system_without_a_private_worker() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let before = bench.journal_bytes();
            let mut steps = FakeSteps::fresh(&bench.spec, NEW_BOOT);
            steps.worker = false;
            let error = execute_steps(
                &root,
                &array_lock,
                &mut journal,
                &snap_spec(),
                vec![ElasticStep::Mkdir { path: "/mnt/elastic-test".into() }],
                StepMode::Recover,
                &mut steps,
            )
            .expect_err("Recover bez wykonawcy");
            assert_eq!(error, "odtworzenie wymaga prywatnego wykonawcy");
            assert!(steps.log.is_empty(), "{:?}", steps.log);
            assert_eq!(bench.journal_bytes(), before);
        }

        #[test]
        fn save_and_load_share_the_operation_record_check() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, journal) = mover_bench(1);
            let before = bench.journal_bytes();
            let sync = |outcome, finished: bool| ElasticSnapraidRun {
                operation_id: MOVER_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T12:00:00Z".into(),
                finished_at: finished.then(|| "2026-09-08T12:01:00Z".to_string()),
                outcome,
                exit_code: Some(1),
                total_blocks: None,
                checked_blocks: None,
                accessed_mb: None,
                errors_file: None,
                errors_io: None,
                errors_data: None,
                detail: None,
            };
            // A Sync still running without the pending that owns it.
            let mut running = journal.clone();
            running.last_run = Some(sync(ElasticSnapraidOutcome::Running, false));
            assert_eq!(root.save(&running).expect_err("Running bez pending"), "niespójny wynik operacji i pending");
            // A failed Sync next to stale parity it did not leave behind.
            let mut foreign = journal.clone();
            foreign.stale_parity_bytes = Some(5);
            foreign.last_run = Some(sync(ElasticSnapraidOutcome::Failed, true));
            assert_eq!(root.save(&foreign).expect_err("obca nieaktualna parity"), "niespójny wynik operacji i pending");
            foreign.stale_sync_operation = Some(NEXT_OPERATION.into());
            assert_eq!(root.save(&foreign).expect_err("inna operacja"), "niespójny wynik operacji i pending");
            assert_eq!(bench.journal_bytes(), before);
            // The same crafted state is unreadable, whoever wrote it.
            atomic_write(&bench.journal_path(), &serde_json::to_vec(&running).expect("json"), root.uid).expect("zapis");
            assert_eq!(root.load(&bench.spec.array_id).err().as_deref(), Some("niespójny wynik operacji i pending"));
            atomic_write(&bench.journal_path(), &before, root.uid).expect("przywrócenie");
            // The mover's own failed Sync is the one record that stands without its pending.
            foreign.stale_sync_operation = Some(MOVER_OPERATION.into());
            root.save(&foreign).expect("własny nieudany Sync movera");
            root.load(&bench.spec.array_id).expect("czytelny po zapisie");
        }

        #[test]
        fn inspect_after_a_failed_recovery_reports_the_restart_and_the_next_boot_recovers() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let host_boot = boot_id().expect("boot hosta");
            assert_ne!(host_boot, boot(), "awaria w innym boot niż bieżący");
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let snapshot = std::cell::RefCell::new(None);
            let journal_path = bench.journal_path();
            let (snapshot_ref, path_ref) = (&snapshot, journal_path.as_path());
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(move |record| {
                    if record.phase == TransferFilePhase::CopyConfirmed {
                        return capture(snapshot_ref, path_ref);
                    }
                    Ok(())
                }),
            )
            .expect_err("utrata zasilania");
            bench.power_loss(&snapshot.take().expect("stan w chwili awarii"));
            drop(array_lock);
            drop(root);
            // This boot: the cache mount fails part-way through recovery.
            let root = bench.reopen();
            let mut crashed = root.load(&bench.spec.array_id).expect("stan po awarii");
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let mut failing = TestHost::new(&bench, &snapraid).after_boot(&bench, &host_boot);
            failing.steps.fail_mount = Some(cache_branch_path("media", "c1"));
            run_mover(
                &root,
                &mut crashed,
                &mover_request(&rules, &array_lock),
                &bench.env_at(&roomy, &nothing_open, &host_boot),
                &mut failing,
            )
            .expect_err("montowanie cache");
            drop(failing);
            let stored = root.load(&bench.spec.array_id).expect("stan");
            assert_eq!(stored.boot_id, host_boot);
            assert!(stored.private.as_ref().expect("private").anchor.is_none());
            let before = bench.journal_bytes();
            // Inspect in the same boot returns the array state with the restart it waits for.
            let inspect = crate::HelperCommand::ElasticInspect {
                array_id: bench.spec.array_id.clone(),
                owner: bench.spec.owner.clone(),
            };
            let value = private_operation(&root, stored.clone(), &inspect).expect("Inspect zwraca stan");
            let state: ElasticResult = serde_json::from_value(value).expect("stan macierzy");
            assert!(state.restart_required);
            assert_eq!(state.stage, ElasticStage::NeedsAttention);
            // The flag carries the state; the detail keeps the cause.
            assert!(state.detail.is_some(), "przyczyna zostaje w szczegółach");
            assert!(
                !state.detail.as_deref().is_some_and(|detail| detail.contains("wymagany restart")),
                "helper nie koduje stanu w zdaniu: {:?}",
                state.detail
            );
            assert_eq!(state.last_mover.expect("mover").phase, ElasticMoverPhase::NeedsAttention);
            assert_eq!(bench.journal_bytes(), before, "Inspect niczego nie zapisuje");
            // The same boot does not retry; the accepted exit is a reboot.
            assert!(run_mover(
                &root,
                &mut stored.clone(),
                &mover_request(&rules, &array_lock),
                &bench.env_at(&roomy, &nothing_open, &host_boot),
                &mut TestHost::new(&bench, &snapraid).after_boot(&bench, &host_boot),
            )
            .is_err());
            assert_eq!(bench.journal_bytes(), before);
            drop(array_lock);
            drop(root);
            recover_and_release(&bench, true, &snapraid, NEXT_BOOT);
            assert_eq!(tree(&bench.data[0]), vec!["a.bin"]);
            assert!(tree(&bench.cache).is_empty());
        }

        /// The longest temporary name the validators admit for `operation_id`:
        /// the prefix they require plus the most digits they allow. Fixtures
        /// that must reach the bound take it from here, never from a literal.
        fn bound_temporary(operation_id: &str) -> String {
            format!(
                ".tentanas-transfer-{operation_id}-{}",
                "9".repeat(TEMPORARY_SEQUENCE_DIGITS)
            )
        }

        /// `count` stuck records with the skip set that must accompany them.
        fn stuck_seed(count: usize) -> (Vec<ElasticStuckRecord>, Vec<StuckPath>) {
            let records: Vec<ElasticStuckRecord> = (0..count).map(stuck_fixture).collect();
            let paths = records
                .iter()
                .map(|record| StuckPath {
                    path_sha256: record.path_sha256.clone(),
                    operation_id: record.operation_id.clone(),
                    temporary: None,
                })
                .collect();
            (records, paths)
        }

        fn stuck_fixture(index: usize) -> ElasticStuckRecord {
            let operation_id = format!("{:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa", index + 1);
            let path = format!("stuck/{index}.bin");
            ElasticStuckRecord {
                temporary: Some(format!(".tentanas-transfer-{operation_id}-0")),
                operation_id,
                path_sha256: path_digest(&path),
                path,
                target: Some("d1".into()),
                source: ElasticFilePin { device: 1, inode: index as u64 + 1, size: Some(1), sha256: Some("b".repeat(64)) },
                temporary_copy: None,
                destination: None,
                reason: "rekord nierozwiązany: test".into(),
                skipped: false,
            }
        }

        #[test]
        fn a_stuck_record_outlives_its_operation_and_later_runs_leave_its_path_alone() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(stop_at("a.bin", TransferFilePhase::RenameConfirmed)),
            )
            .expect_err("utrata zasilania po zmianie nazwy");
            // The copy on the data disk no longer matches, and the source is unreadable.
            std::fs::write(bench.data[0].join("a.bin"), b"FIRST").expect("zmieniony cel");
            let source = std::fs::metadata(bench.cache.join("a.bin")).expect("źródło");
            crate::elastic_transfer::fail_content_reads(Some((source.dev(), source.ino())));
            let result = run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            );
            crate::elastic_transfer::fail_content_reads(None);
            assert!(result.expect_err("nierozwiązany rekord").starts_with("rekord nierozwiązany"));
            // Visible while its operation still holds the array.
            let state = observe(&journal, None);
            assert_eq!(state.stuck_records.len(), 1);
            assert_eq!(state.last_mover.expect("wynik").stuck_records, state.stuck_records);
            release(&root, &mut journal, MOVER_RESUME);
            assert_eq!(journal.stuck.len(), 1, "Resume przenosi rekord do historii");
            // A new operation keeps the history, moves the rest and leaves the stuck path.
            write_aged(&bench.cache.join("c.bin"), b"third", 9000);
            let next = MoverRequest {
                operation_id: NEXT_OPERATION,
                resume_operation_id: NEXT_RESUME,
                ..mover_request(&rules, &array_lock)
            };
            run_mover(
                &root,
                &mut journal,
                &next,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("nowa operacja");
            assert_eq!(tree(&bench.cache), vec!["a.bin"]);
            assert_eq!(tree(&bench.data[0]), vec!["a.bin", "c.bin"]);
            // Nothing was deleted for it: both copies stay as they were.
            assert_eq!(std::fs::read(bench.cache.join("a.bin")).expect("źródło"), b"first");
            assert_eq!(std::fs::read(bench.data[0].join("a.bin")).expect("cel"), b"FIRST");
            let stored = root.load(&bench.spec.array_id).expect("stan");
            assert_eq!(stored.stuck.len(), 1);
            let record = &stored.stuck[0];
            assert_eq!(record.operation_id, MOVER_OPERATION);
            assert_eq!(record.path, "a.bin");
            assert_eq!(record.path_sha256, path_digest("a.bin"));
            assert_eq!(record.target.as_deref(), Some("d1"));
            assert!(record
                .temporary
                .as_deref()
                .is_some_and(|name| name.starts_with(&format!(".tentanas-transfer-{MOVER_OPERATION}-"))));
            assert!(record.source.sha256.is_some());
            assert!(record.temporary_copy.as_ref().is_some_and(|pin| pin.sha256.is_some()));
            assert!(record.reason.starts_with("rekord nierozwiązany"), "{}", record.reason);
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!((transfer.operation_id.as_str(), transfer.phase), (NEXT_OPERATION, ElasticMoverPhase::Complete));
            let issue = transfer.issues.iter().find(|issue| issue.path == "a.bin").expect("pominięta ścieżka");
            assert_eq!(issue.kind, ElasticMoverIssueKind::Refused);
            assert_eq!(issue.reason, format!("rekord utknął w operacji {MOVER_OPERATION}; mover go nie rusza"));
            // The new operation's result shows the history, carrying the flag
            // the view derives rather than whatever the journal stored.
            let state = observe(&stored, None);
            let shown = state.stuck_records.clone();
            assert_eq!(shown, stuck_records(&stored));
            assert_eq!(shown.len(), stored.stuck.len());
            assert!(shown.iter().all(|record| record.skipped), "ta ścieżka jest nadal omijana");
            assert_eq!(state.last_mover.expect("wynik").stuck_records, shown);
            drop(array_lock);
            drop(root);
            assert_eq!(bench.reopen().load(&bench.spec.array_id).expect("reopen").stuck, stored.stuck);
        }

        /// A small, valid in-flight record for a crafted journal.
        fn stuck_file(operation_id: &str, source: &str) -> TransferFile {
            TransferFile {
                source: source.into(),
                destination: source.into(),
                temporary: Some(format!(".tentanas-transfer-{operation_id}-0")),
                temporary_identity: None,
                temporary_pin: None,
                source_identity: crate::elastic_transfer::TransferIdentity {
                    device: 1,
                    inode: 2,
                    size: 3,
                    sha256: "a".repeat(64),
                    uid: 0,
                    gid: 0,
                    mode: 0o644,
                    mtime_ns: 0,
                    acl: Vec::new(),
                    xattr: Vec::new(),
                },
                destination_identity: None,
                directory_intent: None,
                unread_source: None,
                temporary_removed: false,
                phase: TransferFilePhase::RenameConfirmed,
            }
        }

        #[test]
        fn the_stuck_history_caps_full_records_while_every_path_is_still_skipped() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            // A full display history, plus paths whose full record it no longer shows.
            let (records, mut paths) = stuck_seed(STUCK_LIMIT);
            let summarised = ["summarised/0.bin", "summarised/1.bin", "summarised/2.bin"];
            let summarised_operation =
                |index: usize| format!("{:08x}-bbbb-4bbb-8bbb-bbbbbbbbbbbb", index + 1);
            for (index, path) in summarised.iter().enumerate() {
                paths.push(StuckPath {
                    path_sha256: path_digest(path),
                    operation_id: summarised_operation(index),
                    temporary: None,
                });
            }
            journal.stuck = records;
            journal.stuck_paths = paths;
            seed(&root, &journal);
            write_aged(&bench.cache.join("fresh.bin"), b"fresh", 9000);
            for path in summarised {
                write_aged(&bench.cache.join(path), b"stuck", 9000);
            }
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            // A full history never refuses an operation.
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("nowa operacja mimo pełnej historii");
            assert_eq!(tree(&bench.data[0]), vec!["fresh.bin"]);
            assert_eq!(tree(&bench.cache), summarised.to_vec(), "każda utknięta ścieżka zostaje");
            let stored = root.load(&bench.spec.array_id).expect("stan");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!(transfer.phase, ElasticMoverPhase::Complete);
            for (index, path) in summarised.iter().enumerate() {
                let issue = transfer
                    .issues
                    .iter()
                    .find(|issue| issue.path == *path)
                    .unwrap_or_else(|| panic!("{path}: {:?}", transfer.issues));
                assert_eq!(issue.kind, ElasticMoverIssueKind::Refused);
                assert_eq!(
                    issue.reason,
                    format!("rekord utknął w operacji {}; mover go nie rusza", summarised_operation(index))
                );
            }
            // The result says how many stuck paths it does not show in full.
            let state = observe(&stored, None);
            assert_eq!(state.stuck_records.len(), STUCK_LIMIT);
            assert_eq!(state.stuck_hidden, summarised.len() as u64);
            assert_eq!(state.last_mover.expect("wynik").stuck_hidden, summarised.len() as u64);
            // Nothing removes a skipped path, not even one no full record
            // stands on any more.
            let mut dropped = stored.clone();
            dropped.stuck_paths.pop();
            assert_eq!(
                root.save(&dropped).expect_err("usunięcie pominiętej ścieżki"),
                "sprzeczne przejście transferu: zmiana listy pominiętych ścieżek"
            );
            // A path a shown record stands on is refused even earlier.
            let mut orphaned = stored.clone();
            orphaned.stuck_paths.remove(0);
            assert_eq!(
                root.save(&orphaned).expect_err("rekord bez swojej ścieżki"),
                "nieprawidłowa lista pominiętych ścieżek"
            );
            // Over its bound the skip set is not a readable journal.
            let mut over = stored.clone();
            over.stuck = Vec::new();
            over.stuck_paths = (0..=STUCK_PATH_LIMIT)
                .map(|index| StuckPath {
                    path_sha256: path_digest(&format!("over/{index}.bin")),
                    operation_id: MOVER_RESUME.into(),
                    temporary: None,
                })
                .collect();
            assert_eq!(
                validate_topology(&over).expect_err("ponad limit"),
                "nieprawidłowa lista pominiętych ścieżek"
            );
        }

        #[test]
        fn a_ninth_stuck_record_summarises_the_oldest_away_and_keeps_its_path() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            let (records, paths) = stuck_seed(STUCK_LIMIT);
            let oldest = records[0].clone();
            journal.stuck = records;
            journal.stuck_paths = paths;
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().expect("private").service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            journal.transfer = Some(TransferJournal {
                target: Some("d1".into()),
                sequence: 1,
                current: Some(stuck_file(MOVER_OPERATION, "ninth.bin")),
                current_failed: Some("rekord nierozwiązany: test".into()),
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, false)
            });
            seed(&root, &journal);
            close_transfer(&mut journal, "77777777-7777-4777-8777-777777777777").expect("Resume zamyka transfer");
            assert_eq!(journal.stuck.len(), STUCK_LIMIT, "historia nie rośnie ponad limit");
            assert!(
                !journal.stuck.iter().any(|record| record.path_sha256 == oldest.path_sha256),
                "najstarszy pełny rekord zwinięty"
            );
            assert!(
                journal.stuck_paths.iter().any(|path| path.path_sha256 == oldest.path_sha256),
                "jego ścieżka nadal pomijana"
            );
            assert_eq!(journal.stuck_paths.len(), STUCK_LIMIT + 1);
            assert_eq!(stuck_hidden(&journal), 1);
            assert_eq!(journal.stuck.last().expect("najnowszy").path, "ninth.bin");
            root.save(&journal).expect("zamknięcie z pełną historią");
            assert_eq!(root.load(&journal.spec.array_id).expect("reopen").stuck_paths.len(), STUCK_LIMIT + 1);
        }

        #[test]
        fn the_plan_time_check_keeps_every_later_write_inside_the_journal_limit() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            // The heaviest array the validation lets exist. EVERY field is
            // derived from the validator that bounds it — a fixture below any
            // bound measures a journal lighter than the one the validators
            // accept, which is how four rounds in a row under-measured.
            let quoted = |count: usize| "\"".repeat(count);
            // `identity_text` admits 128 B per identity, but 32 disks at that
            // length overrun `ElasticCreateSpec::validate`'s 15 KiB cap: the
            // BINDING bound is the spec, so the identity length is searched
            // DOWN from 128 until the validator accepts, and whatever is left
            // over goes to the owner one byte at a time. No number below is
            // picked by hand; the validators decide all of them.
            let identity = |prefix: &str, index: usize, fill: char, len: usize| {
                let head = format!("{prefix}{index:02}");
                format!("{head}{}", fill.to_string().repeat(len.saturating_sub(head.len())))
            };
            let build = |len: usize| {
                let disk = |index: usize| ElasticDiskSpec {
                    disk_id: identity("serial:", index, 'd', len),
                    wwn: Some(identity("wwn-", index, 'w', len)),
                    serial: Some(identity("serial-", index, 's', len)),
                    bytes: 32 << 40,
                    expected_uuid: format!("{index:08x}-cccc-4ccc-8ccc-cccccccccccc"),
                };
                let mut spec = spec();
                // `validate_array_name`: 64 bytes, first byte alphanumeric.
                spec.name = format!("a{}", "n".repeat(ARRAY_NAME_LIMIT - 1));
                spec.data = (1..=29).map(disk).collect();
                spec.cache = Some(disk(30));
                spec.parity = vec![disk(31), disk(32)];
                spec
            };
            let identity_len = (16..=IDENTITY_TEXT_LIMIT)
                .rev()
                .find(|len| build(*len).validate().is_ok())
                .expect("jakaś długość tożsamości mieści się w limicie specyfikacji");
            // The SEARCH decides it, never a literal: one byte longer is refused.
            assert!(
                identity_len == IDENTITY_TEXT_LIMIT || build(identity_len + 1).validate().is_err(),
                "tożsamości mają być najdłuższe, jakie przechodzą walidację ({identity_len} B)"
            );
            let mut spec = build(identity_len);
            assert_eq!(spec.name.len(), ARRAY_NAME_LIMIT, "nazwa macierzy stoi na granicy");
            let encoded = |spec: &ElasticCreateSpec| serde_json::to_vec(spec).expect("json").len();
            while encoded(&spec) < SPEC_ENCODED_LIMIT
                && spec.owner.addon_id.len() < IDENTITY_TEXT_LIMIT
            {
                spec.owner.addon_id.push('o');
            }
            if encoded(&spec) > SPEC_ENCODED_LIMIT {
                spec.owner.addon_id.pop();
            }
            spec.validate().expect("najcięższa dopuszczalna specyfikacja");
            let spec_size = encoded(&spec);
            // AT the bound means one more byte anywhere is refused.
            let mut grown = spec.clone();
            grown.owner.addon_id.push('o');
            assert!(
                grown.owner.addon_id.len() > IDENTITY_TEXT_LIMIT || grown.validate().is_err(),
                "specyfikacja ma stać na granicy, stoi na {spec_size} B z {SPEC_ENCODED_LIMIT} B"
            );
            eprintln!("MIARA | specyfikacja {spec_size} B z {SPEC_ENCODED_LIMIT} B");
            let mut journal = root.reserve(&spec, boot(), || Ok(())).expect("reserve");
            journal.formatted = roles(&spec);
            journal.stage = ElasticStage::NeedsAttention;
            journal.sync_completed_at = Some("2026-09-08T11:00:00Z".into());
            // The anchor carries what the helper itself writes for THIS name:
            // the branch list mergerfs was started with, cache first.
            let layout = layout_with(&spec, |disk| Ok(format!("/dev/disk/by-id/{}", disk.disk_id)))
                .expect("układ najcięższej macierzy");
            let branches = layout.branch_specs().join(":");
            let anchor_size = json_size(&branches);
            assert!(anchor_size <= ANCHOR_SOURCE_BYTES, "kotwica {anchor_size} B ponad limit");
            assert!(branches.contains(&spec.name), "kotwica opisuje tę macierz");
            assert_eq!(
                branches.split(':').count(),
                spec.data.len() + usize::from(spec.cache.is_some()),
                "kotwica wymienia każdy branch unii"
            );
            eprintln!("MIARA | kotwica {anchor_size} B z {ANCHOR_SOURCE_BYTES} B");
            let private = journal.private.as_mut().expect("private");
            private.anchor = Some(Anchor { union_source: branches, ..anchor_fixture() });
            private.published = false;
            private.service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            let failed_sync = ElasticSnapraidRun {
                operation_id: MOVER_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T12:00:00Z".into(),
                finished_at: Some("2026-09-08T12:01:00Z".into()),
                outcome: ElasticSnapraidOutcome::Failed,
                exit_code: Some(i32::MIN),
                total_blocks: Some(u64::MAX),
                checked_blocks: Some(u64::MAX),
                accessed_mb: Some(u64::MAX),
                errors_file: Some(u64::MAX),
                errors_io: Some(u64::MAX),
                errors_data: Some(u64::MAX),
                detail: Some(quoted(TRANSFER_DETAIL_LIMIT)),
            };
            journal.detail = Some(quoted(TRANSFER_DETAIL_LIMIT));
            journal.stale_parity_bytes = Some(u64::MAX);
            journal.stale_sync_operation = Some(MOVER_OPERATION.into());
            journal.last_run = Some(failed_sync.clone());
            // Folder rules at their bound, measured as JSON, which is what the
            // journal pays for them.
            let rules = MoverRules {
                pinned_folders: (0..128).map(|index| format!("{index:03}{}", quoted(8 * 1024 / 128 - 3))).collect(),
                ..move_everything_aged()
            };
            rules.validate().expect("reguły na granicy");
            let worst_pin = ElasticFilePin {
                device: u64::MAX,
                inode: u64::MAX,
                size: Some(u64::MAX),
                sha256: Some("f".repeat(64)),
            };
            let (records, paths) = stuck_seed(STUCK_LIMIT - 1);
            journal.stuck = records
                .iter()
                .map(|base| ElasticStuckRecord {
                    operation_id: base.operation_id.clone(),
                    path: quoted(TRANSFER_TEXT_LIMIT),
                    path_sha256: base.path_sha256.clone(),
                    target: Some("d1".into()),
                    temporary: Some(bound_temporary(&base.operation_id)),
                    source: worst_pin.clone(),
                    temporary_copy: Some(worst_pin.clone()),
                    destination: Some(worst_pin.clone()),
                    reason: "\\".repeat(TRANSFER_TEXT_LIMIT),
                    // `stuck_entry` always persists false; the view derives it.
                    skipped: false,
                })
                .collect();
            // AT the bound, derived from the validators: every entry of a FULL
            // skip set and a FULL ring carries the longest temporary name they
            // admit. `valid_transfer_record` forces the in-flight record's name
            // to be `Some`, so every path the close inserts really does carry
            // one — a fixture that leaves any of them `None` measures a journal
            // lighter than the one the validators would accept.
            journal.stuck_paths = (0..STUCK_PATH_LIMIT)
                .map(|index| {
                    let mut entry = paths.get(index).cloned().unwrap_or_else(|| StuckPath {
                        path_sha256: path_digest(&format!("more/{index}.bin")),
                        operation_id: MOVER_RESUME.into(),
                        temporary: None,
                    });
                    entry.temporary = Some(bound_temporary(&entry.operation_id));
                    entry
                })
                .collect();
            journal.evicted_paths = (0..EVICTED_RING)
                .map(|index| StuckPath {
                    path_sha256: path_digest(&format!("ring/{index}.bin")),
                    temporary: Some(bound_temporary(MOVER_RESUME)),
                    operation_id: MOVER_RESUME.into(),
                })
                .collect();
            journal.stuck_evicted = EVICTED_RING as u64;
            assert!(
                journal.stuck_paths.iter().chain(&journal.evicted_paths).all(|path| {
                    path.temporary.as_deref() == Some(bound_temporary(&path.operation_id).as_str())
                }),
                "każdy wpis stoi na granicy, inaczej mierzymy za lekki dziennik"
            );
            journal.transfer = Some(TransferJournal {
                resume_operation_id: MOVER_RESUME.into(),
                rules,
                coupled_sync_result: Some(failed_sync),
                sync_attempt: u64::MAX,
                target: Some("d1".into()),
                sequence: u64::MAX,
                moved_files: u64::MAX,
                moved_bytes: u64::MAX,
                skipped_files: u64::MAX,
                skipped_bytes: u64::MAX,
                refused_files: u64::MAX,
                issues: (0..TRANSFER_ISSUE_LIMIT)
                    .map(|_| ElasticMoverIssue {
                        path: quoted(TRANSFER_TEXT_LIMIT),
                        kind: ElasticMoverIssueKind::Attention,
                        reason: "\\".repeat(TRANSFER_TEXT_LIMIT),
                    })
                    .collect(),
                detail: Some(quoted(TRANSFER_DETAIL_LIMIT)),
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, true)
            });
            seed(&root, &journal);
            assert!(
                !serde_json::to_string(&journal).expect("json").contains("\"skipped\""),
                "wyprowadzona flaga nie zajmuje miejsca w dzienniku"
            );
            let carried = std::fs::read(root.path.join(format!("{}.json", spec.array_id))).expect("journal").len();
            assert!(carried <= JOURNAL_LIMIT as usize, "sam stan: {carried} B");
            // A record that would leave no room for what the operation must
            // still write is refused before the file is started.
            let mut candidate = stuck_file(MOVER_OPERATION, "folder/w.bin");
            candidate.source_identity.xattr = vec![crate::elastic_transfer::TransferAttribute {
                name: "user.blob".into(),
                value: Vec::new(),
            }];
            let base = worst_case_journal_size(&journal, &candidate).expect("projekcja");
            eprintln!(
                "MIARA | stan {carried} B | projekcja z pustym rekordem {base} B | zapas na rekord {} B | limit {JOURNAL_LIMIT} B",
                (JOURNAL_LIMIT as usize).saturating_sub(base)
            );
            assert!(base <= JOURNAL_LIMIT as usize, "pusty rekord: {base} B");
            // Grown until the projection sits just under the limit.
            let room = (JOURNAL_LIMIT as usize - base) / 2;
            candidate.source_identity.xattr[0].value = vec![0xff; room.saturating_sub(64)];
            let fitted = worst_case_journal_size(&journal, &candidate).expect("projekcja");
            assert!(fitted <= JOURNAL_LIMIT as usize, "dopasowany rekord: {fitted} B");
            let mut oversized = candidate.clone();
            oversized.source_identity.xattr[0].value = vec![0xff; room + 512];
            assert!(
                crate::elastic_transfer::worst_case_record_size(&oversized).expect("rozmiar") <= TRANSFER_RECORD_LIMIT,
                "sam rekord mieści się w swoim limicie"
            );
            assert!(
                worst_case_journal_size(&journal, &oversized).expect("projekcja") > JOURNAL_LIMIT as usize,
                "to dziennik, nie rekord, odmawia tego pliku"
            );
            // What has to hold on a maximal array: an ORDINARY file still
            // moves. A record carrying a 4 KiB Samba ACL costs about twice
            // that once hex-encoded, and it fits with room to spare.
            let mut ordinary = candidate.clone();
            ordinary.source_identity.xattr[0].value = vec![0xff; 4 * 1024];
            let ordinary_size = worst_case_journal_size(&journal, &ordinary).expect("projekcja");
            assert!(
                ordinary_size <= JOURNAL_LIMIT as usize,
                "zwykły plik nie mieści się na maksymalnej macierzy: {ordinary_size} B"
            );
            eprintln!("MIARA | zwykły plik (xattr 4 KiB) {ordinary_size} B z {JOURNAL_LIMIT} B");
            // What the plan accepted still fits at its worst: the record fully
            // pinned, the failure text at its bound and the closing Resume
            // appending its history entry and its skipped path.
            let mut worst = journal.clone();
            {
                let transfer = worst.transfer.as_mut().expect("transfer");
                let mut record = crate::elastic_transfer::worst_case_record(&candidate);
                // The projection over-states the directory intent on purpose;
                // a persisted record names the real parent.
                record.directory_intent = Some("folder".into());
                transfer.current = Some(record);
                transfer.current_failed = Some(quoted(TRANSFER_DETAIL_LIMIT));
                transfer.phase = ElasticMoverPhase::NeedsAttention;
            }
            seed(&root, &worst);
            close_transfer(&mut worst, MOVER_RESUME).expect("Resume zamyka transfer");
            root.save(&worst).expect("zamykający zapis mieści się w limicie");
            let size = std::fs::read(root.path.join(format!("{}.json", spec.array_id))).expect("journal").len();
            assert!(size as u64 <= JOURNAL_LIMIT, "{size} B");
            let reopened = root.load(&spec.array_id).expect("odczyt po zapisie");
            assert_eq!(serde_json::to_vec(&reopened).expect("json"), serde_json::to_vec(&worst).expect("json"));
            eprintln!(
                "MIARA | najcięższy zapis zamykający {size} B z {JOURNAL_LIMIT} B | margines {} B | skip {STUCK_PATH_LIMIT} | ring {EVICTED_RING}",
                JOURNAL_LIMIT.saturating_sub(size as u64)
            );
        }

        #[test]
        fn a_file_whose_journal_would_not_fit_is_refused_and_the_run_goes_on() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            let quoted = |count: usize| "\"".repeat(count);
            let worst_pin = ElasticFilePin {
                device: u64::MAX,
                inode: u64::MAX,
                size: Some(u64::MAX),
                sha256: Some("f".repeat(64)),
            };
            let (records, paths) = stuck_seed(STUCK_LIMIT);
            journal.stuck = records
                .iter()
                .map(|base| ElasticStuckRecord {
                    path: quoted(TRANSFER_TEXT_LIMIT),
                    temporary: Some(bound_temporary(&base.operation_id)),
                    source: worst_pin.clone(),
                    temporary_copy: Some(worst_pin.clone()),
                    destination: Some(worst_pin.clone()),
                    reason: "\\".repeat(TRANSFER_TEXT_LIMIT),
                    ..base.clone()
                })
                .collect();
            journal.stuck_paths = (0..STUCK_PATH_LIMIT)
                .map(|index| {
                    paths.get(index).cloned().unwrap_or_else(|| StuckPath {
                        path_sha256: path_digest(&format!("more/{index}.bin")),
                        operation_id: MOVER_RESUME.into(),
                        temporary: None,
                    })
                })
                .collect();
            journal.private.as_mut().expect("private").anchor = Some(Anchor {
                union_source: "b".repeat(ANCHOR_SOURCE_BYTES - 2),
                ..anchor_fixture()
            });
            // The heaviest history the array may also be carrying.
            journal.last_run = Some(ElasticSnapraidRun {
                operation_id: NEXT_OPERATION.into(),
                kind: ElasticSnapraidKind::Sync,
                started_at: "2026-09-08T12:00:00Z".into(),
                finished_at: Some("2026-09-08T12:01:00Z".into()),
                outcome: ElasticSnapraidOutcome::Failed,
                exit_code: Some(i32::MIN),
                total_blocks: Some(u64::MAX),
                checked_blocks: Some(u64::MAX),
                accessed_mb: Some(u64::MAX),
                errors_file: Some(u64::MAX),
                errors_io: Some(u64::MAX),
                errors_data: Some(u64::MAX),
                detail: Some(quoted(TRANSFER_DETAIL_LIMIT)),
            });
            journal.stale_parity_bytes = Some(1);
            journal.stale_sync_operation = Some(NEXT_OPERATION.into());
            seed(&root, &journal);
            write_aged(&bench.cache.join("small.bin"), b"small", 9000);
            write_aged(&bench.cache.join("huge.bin"), b"huge", 8000);
            let rules = MoverRules {
                pinned_folders: (0..128).map(|index| format!("{index:03}{}", quoted(8 * 1024 / 128 - 3))).collect(),
                ..move_everything_aged()
            };
            // Sized from the journal this operation would have to write: the
            // file's own record still fits its limit, the journal does not.
            let planned = |path: &'static str| {
                let file = crate::elastic_transfer::scan_cache(&bench.cache, |_| false)
                    .expect("skan")
                    .into_iter()
                    .find_map(|entry| match entry {
                        ScanEntry::File(file) if file.path == path => Some(file),
                        _ => None,
                    })
                    .expect("plik skanu");
                crate::elastic_transfer::plan_file(
                    &bench.cache,
                    &bench.data[0],
                    path,
                    (file.device, file.inode),
                    &format!(".tentanas-transfer-{MOVER_OPERATION}-0"),
                )
                .expect("rekord pliku")
            };
            write_aged(&bench.cache.join("probe.bin"), b"p", 9000);
            set_user_xattr(&bench.cache.join("probe.bin"), "user.blob", b"x");
            let mut projected = journal.clone();
            projected.transfer = Some(TransferJournal {
                rules: rules.clone(),
                target: Some("d1".into()),
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, true)
            });
            let base = worst_case_journal_size(&projected, &planned("probe.bin")).expect("projekcja");
            std::fs::remove_file(bench.cache.join("probe.bin")).expect("usunięcie próbki");
            assert!(base < JOURNAL_LIMIT as usize, "baza projekcji {base} B");
            let room = (JOURNAL_LIMIT as usize - base) / 2;
            set_user_xattr(&bench.cache.join("huge.bin"), "user.blob", &vec![b'x'; room + 512]);
            let huge_record = planned("huge.bin");
            let record_size = crate::elastic_transfer::worst_case_record_size(&huge_record).expect("rozmiar");
            assert!(record_size <= TRANSFER_RECORD_LIMIT, "sam rekord mieści się w swoim limicie: {record_size} B");
            assert!(
                worst_case_journal_size(&projected, &huge_record).expect("projekcja") > JOURNAL_LIMIT as usize,
                "to dziennik, nie rekord, odmawia tego pliku (baza {base} B)"
            );
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("mover kończy przegląd");
            assert_eq!(tree(&bench.data[0]), vec!["small.bin"], "plik, który się mieści, jedzie");
            assert_eq!(tree(&bench.cache), vec!["huge.bin"], "odmówiony plik zostaje nietknięty");
            let transfer = root.load(&bench.spec.array_id).expect("stan").transfer.expect("transfer");
            assert_eq!(transfer.phase, ElasticMoverPhase::Complete);
            let issue = transfer
                .issues
                .iter()
                .find(|issue| issue.path == "huge.bin")
                .unwrap_or_else(|| panic!("{:?}", transfer.issues));
            assert_eq!(issue.reason, "dziennik z tym plikiem przekroczyłby limit odczytu");
        }

        #[test]
        fn a_journal_write_the_limit_refuses_still_leaves_a_releasable_run() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let (cache, target) = record_dirs(&dir);
            let mut journal = near_limit_journal(&root, &cache, &target, 300);
            // MIN-6: an interrupted mover run left bytes outside parity. The
            // emergency write must not quietly drop that marker while it is
            // rescuing the run.
            journal.stale_parity_bytes = Some(4096);
            journal.stale_sync_operation = Some(MOVER_OPERATION.into());
            seed(&root, &journal);
            let in_flight = journal.transfer.clone().expect("transfer");
            assert!(in_flight.current.is_some() && in_flight.current_failed.is_none(), "rekord wygląda zdrowo");
            assert!(
                transfer_blocks_resume(&journal, &in_flight, MOVER_RESUME),
                "taki rekord blokowałby Resume"
            );
            // Every field is within its own bound and the journal still has no
            // room left, so the write that would carry the error is refused.
            let mut refused = journal.clone();
            refused.detail = Some("x".repeat(TRANSFER_DETAIL_LIMIT));
            assert_eq!(
                root.save(&refused).expect_err("zbyt duży journal"),
                "journal przekroczyłby limit odczytu"
            );
            let mut after = refused.clone();
            let error = stick_short_record(&root, &mut after, "journal przekroczyłby limit odczytu".into());
            assert!(error.ends_with(SHORT_STUCK_REASON), "{error}");
            let released = root.load(&journal.spec.array_id).expect("stan po awaryjnym zapisie");
            let transfer = released.transfer.clone().expect("transfer");
            assert_eq!(transfer.current_failed.as_deref(), Some(SHORT_STUCK_REASON));
            assert_eq!(transfer.phase, ElasticMoverPhase::NeedsAttention);
            assert_eq!(transfer.current, in_flight.current, "rekord zostaje dokładnie taki, jaki był");
            assert!(!transfer_blocks_resume(&released, &transfer, MOVER_RESUME), "Resume może zwolnić macierz");
            assert!(
                released.stale_parity_bytes.is_some(),
                "zapis awaryjny nie może skasować znacznika nieaktualnej parity"
            );
            // Nothing on the cache is touched by any of this: the source file
            // is still exactly what it was before the refused write.
            assert_eq!(
                std::fs::read(cache.join("w.bin")).expect("źródło"),
                b"w",
                "nic nie jest usuwane"
            );
        }

        #[test]
        fn a_leftover_journal_temporary_never_blocks_a_write() {
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("root");
            let journal = ready_service_journal(&root);
            // A hard reset left temporaries behind and this boot reuses the pid.
            let pid = std::process::id();
            let next = NEXT_FILE.load(Ordering::Relaxed);
            let leftovers: Vec<PathBuf> = (next..next + 4)
                .map(|sequence| dir.0.join(format!(".elastic-{pid}-{sequence}.new")))
                .collect();
            for path in &leftovers {
                std::fs::write(path, b"pozostalosc").expect("pozostałość");
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("mode");
            }
            let foreign = dir.0.join("obcy.txt");
            std::fs::write(&foreign, b"nie nasze").expect("obcy plik");
            root.save(&journal).expect("zapis mimo pozostałości");
            root.load(&journal.spec.array_id).expect("journal czytelny");
            // Reopening the root sweeps what it can attribute to itself, and
            // nothing else.
            drop(root);
            let root = Root::open(&dir.0, uid).expect("reopen");
            for path in &leftovers {
                assert!(!path.exists(), "{}", path.display());
            }
            assert!(foreign.exists(), "obcy plik nietknięty");
            root.load(&journal.spec.array_id).expect("journal po sprzątaniu");
        }

        #[test]
        fn inspect_flags_the_restart_when_a_recovery_failed_after_its_anchor() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let host_boot = boot_id().expect("boot hosta");
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let snapshot = std::cell::RefCell::new(None);
            let journal_path = bench.journal_path();
            let (snapshot_ref, path_ref) = (&snapshot, journal_path.as_path());
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(move |record| {
                    if record.phase == TransferFilePhase::CopyConfirmed {
                        return capture(snapshot_ref, path_ref);
                    }
                    Ok(())
                }),
            )
            .expect_err("utrata zasilania");
            bench.power_loss(&snapshot.take().expect("stan w chwili awarii"));
            drop(array_lock);
            drop(root);
            // This boot: the new union comes up, its anchor is saved, and only
            // then does the RO barrier fail.
            let root = bench.reopen();
            let mut crashed = root.load(&bench.spec.array_id).expect("stan po awarii");
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let mut failing = TestHost::new(&bench, &snapraid)
                .after_boot(&bench, &host_boot)
                .with_enter(|| Err("prywatna unia nie przechodzi w RO".into()));
            run_mover(
                &root,
                &mut crashed,
                &mover_request(&rules, &array_lock),
                &bench.env_at(&roomy, &nothing_open, &host_boot),
                &mut failing,
            )
            .expect_err("bariera RO po odtworzeniu");
            drop(failing);
            let stored = root.load(&bench.spec.array_id).expect("stan");
            assert_eq!(stored.boot_id, host_boot);
            assert!(stored.private.as_ref().expect("private").anchor.is_some(), "kotwica zapisana przed błędem");
            assert_eq!(stored.pending, Some(Pending::Union));
            let before = bench.journal_bytes();
            let inspect = crate::HelperCommand::ElasticInspect {
                array_id: bench.spec.array_id.clone(),
                owner: bench.spec.owner.clone(),
            };
            let value = private_operation(&root, stored.clone(), &inspect).expect("Inspect zwraca stan");
            let state: ElasticResult = serde_json::from_value(value).expect("stan macierzy");
            assert!(state.restart_required, "kotwica bez ukończonego odtworzenia też czeka na restart");
            // The flag carries the state; the detail keeps the cause.
            assert!(state.detail.is_some(), "przyczyna zostaje w szczegółach");
            assert!(
                !state.detail.as_deref().is_some_and(|detail| detail.contains("wymagany restart")),
                "helper nie koduje stanu w zdaniu: {:?}",
                state.detail
            );
            assert_eq!(bench.journal_bytes(), before, "Inspect niczego nie zapisuje");
            // The same boot does not retry; the accepted exit is a reboot.
            assert!(run_mover(
                &root,
                &mut stored.clone(),
                &mover_request(&rules, &array_lock),
                &bench.env_at(&roomy, &nothing_open, &host_boot),
                &mut TestHost::new(&bench, &snapraid).after_boot(&bench, &host_boot),
            )
            .is_err());
            assert_eq!(bench.journal_bytes(), before);
            drop(array_lock);
            drop(root);
            recover_and_release(&bench, true, &snapraid, NEXT_BOOT);
            assert_eq!(tree(&bench.data[0]), vec!["a.bin"]);
        }

        #[test]
        fn the_anchor_and_the_rules_are_bounded_as_the_journal_stores_them() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            let quoted = |count: usize| "\"".repeat(count);
            // A branch string whose raw length fits but whose JSON cost does not.
            let mut oversized = journal.clone();
            oversized.private.as_mut().expect("private").anchor = Some(Anchor {
                union_source: quoted(ANCHOR_SOURCE_BYTES - 1024),
                ..anchor_fixture()
            });
            assert_eq!(
                validate_topology(&oversized).expect_err("kotwica po zakodowaniu"),
                "nieprawidłowa tożsamość kotwicy"
            );
            assert!(root.save(&oversized).is_err());
            // The same length of plain bytes is what the bound allows.
            journal.private.as_mut().expect("private").anchor = Some(Anchor {
                union_source: "b".repeat(ANCHOR_SOURCE_BYTES - 1024),
                ..anchor_fixture()
            });
            root.save(&journal).expect("kotwica w granicach");
            // The folder rules are measured the same way.
            let folders = |fill: &str, length: usize| {
                (0..5).map(|index| format!("{index}{}", fill.repeat(length))).collect::<Vec<_>>()
            };
            MoverRules { pinned_folders: folders("a", 1700), ..MoverRules::default() }
                .validate()
                .expect("zwykłe nazwy mieszczą się");
            assert!(
                MoverRules { pinned_folders: folders("\"", 1700), ..MoverRules::default() }
                    .validate()
                    .is_err(),
                "reguły liczone po zakodowaniu"
            );
        }

        /// `count` skipped paths that no full record stands on.
        fn skip_seed(count: usize) -> Vec<StuckPath> {
            (0..count)
                .map(|index| {
                    let operation_id = format!("{:08x}-dddd-4ddd-8ddd-dddddddddddd", index + 1);
                    StuckPath {
                        path_sha256: path_digest(&format!("evicted/{index}.bin")),
                        temporary: Some(format!(".tentanas-transfer-{operation_id}-{index}")),
                        operation_id,
                    }
                })
                .collect()
        }

        #[test]
        fn a_full_skip_set_evicts_its_oldest_digest_and_still_protects_the_newest() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            EVICTION_LOG.with(|log| *log.borrow_mut() = Some(Vec::new()));
            let (bench, root, mut journal) = mover_bench(1);
            journal.stuck_paths = skip_seed(STUCK_PATH_LIMIT);
            let oldest = journal.stuck_paths[0].clone();
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().expect("private").service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            journal.transfer = Some(TransferJournal {
                resume_operation_id: MOVER_RESUME.into(),
                target: Some("d1".into()),
                sequence: 1,
                current: Some(stuck_file(MOVER_OPERATION, "newest.bin")),
                current_failed: Some("rekord nierozwiązany: test".into()),
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, false)
            });
            seed(&root, &journal);
            let evicted = close_transfer(&mut journal, MOVER_RESUME).expect("Resume zamyka transfer");
            // Nothing is said before the write is durable.
            assert!(
                EVICTION_LOG.with(|log| log.borrow().as_ref().expect("zlew").is_empty()),
                "zamknięcie samo w sobie niczego nie ogłasza"
            );
            // FIFO: the newest stuck path is protected, the oldest digest goes.
            assert_eq!(journal.stuck_paths.len(), STUCK_PATH_LIMIT);
            assert_eq!(journal.stuck_evicted, 1);
            assert!(!journal.stuck_paths.iter().any(|path| path.path_sha256 == oldest.path_sha256));
            assert_eq!(
                journal.stuck_paths.last().expect("najnowsza").path_sha256,
                path_digest("newest.bin")
            );
            // The eviction is reported where nothing can drop it.
            let detail = journal.transfer.as_ref().expect("transfer").detail.clone().expect("szczegóły");
            assert!(detail.contains("lista pominiętych ścieżek pełna"), "{detail}");
            assert!(detail.contains("łącznie wypartych: 1"), "{detail}");
            // The identity of what stopped being skipped survives on both
            // routes: the journal's ring and the system log.
            let kept = journal.evicted_paths.last().expect("pierścień");
            assert_eq!(kept, &oldest, "pierścień trzyma dokładnie wypartą tożsamość");
            // The entry the eviction made room for names its temporary as well,
            // or the next eviction would have nothing to hand the operator.
            let inserted = journal.stuck_paths.last().expect("najnowsza ścieżka");
            assert_eq!(
                inserted.temporary,
                journal.stuck.last().expect("rekord").temporary,
                "wstawiona ścieżka zna kopię tymczasową rekordu"
            );
            assert!(inserted.temporary.is_some(), "rekord miał kopię tymczasową");
            // The run reports what IT evicted; the array keeps the lifetime figure.
            assert_eq!(journal.transfer.as_ref().expect("transfer").evicted, 1);
            root.save(&journal).expect("zamknięcie z wyparciem");
            log_eviction(&evicted.expect("wyparta tożsamość"));
            // Said only now, and it names what stopped being skipped.
            let logged = EVICTION_LOG.with(|log| log.borrow().clone().expect("zainstalowany zlew"));
            let line = logged.last().expect("wpis w logu");
            assert!(line.contains(&oldest.path_sha256), "{line}");
            assert!(line.contains(&oldest.operation_id), "{line}");
            assert!(
                line.contains(oldest.temporary.as_deref().expect("nazwa tymczasowa")),
                "{line}"
            );
            let mut released = root.load(&bench.spec.array_id).expect("reopen");
            assert_eq!(released.stuck_evicted, 1);
            let state = observe(&released, None);
            assert_eq!(state.stuck_evicted, 1, "macierz pamięta wyparcie");
            assert_eq!(state.last_mover.expect("wynik").stuck_evicted, 1, "ten przebieg je wykonał");
            // The next run skips the protected path and no longer skips the evicted one.
            release(&root, &mut released, MOVER_RESUME);
            write_aged(&bench.cache.join("newest.bin"), b"newest", 9000);
            write_aged(&bench.cache.join("evicted/0.bin"), b"evicted", 9000);
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let next = MoverRequest {
                operation_id: NEXT_OPERATION,
                resume_operation_id: NEXT_RESUME,
                ..mover_request(&rules, &array_lock)
            };
            run_mover(
                &root,
                &mut released,
                &next,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("nowa operacja");
            assert_eq!(tree(&bench.cache), vec!["newest.bin"], "najnowsza utknięta ścieżka nadal omijana");
            assert_eq!(tree(&bench.data[0]), vec!["evicted/0.bin"], "wyparta ścieżka nie jest już omijana");
            // A later clean run says nothing about an eviction it did not make.
            let stored = root.load(&bench.spec.array_id).expect("stan");
            let transfer = stored.transfer.clone().expect("transfer");
            assert_eq!(transfer.evicted, 0, "licznik wyparć jest licznikiem przebiegu");
            let state = observe(&stored, None);
            assert_eq!(state.stuck_evicted, 1, "macierz nadal pamięta");
            assert_eq!(state.last_mover.expect("wynik").stuck_evicted, 0, "ten przebieg nic nie wyparł");
        }

        #[test]
        fn the_eviction_notice_survives_a_detail_that_fills_the_bound() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stuck_paths = skip_seed(STUCK_PATH_LIMIT);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().expect("private").service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            journal.transfer = Some(TransferJournal {
                resume_operation_id: MOVER_RESUME.into(),
                target: Some("d1".into()),
                sequence: 1,
                current: Some(stuck_file(MOVER_OPERATION, "newest.bin")),
                current_failed: Some("rekord nierozwiązany: test".into()),
                // A detail already at its bound: an appended notice would be
                // the first thing the bound cut away.
                detail: Some("x".repeat(TRANSFER_DETAIL_LIMIT)),
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, false)
            });
            seed(&root, &journal);
            close_transfer(&mut journal, MOVER_RESUME).expect("Resume zamyka transfer");
            let detail = journal.transfer.expect("transfer").detail.expect("szczegóły");
            assert!(detail.len() <= TRANSFER_DETAIL_LIMIT, "{}", detail.len());
            assert!(detail.starts_with("lista pominiętych ścieżek pełna"), "{detail}");
        }

        #[test]
        fn a_record_stuck_before_its_rename_is_left_alone_by_later_runs() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"first", 9000);
            let data = bench.data[0].clone();
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            // The target stops accepting changes after the copy: the rename
            // fails and so does withdrawing the copy, so the record sticks
            // BEFORE its rename — there is no destination to refuse a later run.
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid).with_checkpoint(move |record| {
                    if record.phase == TransferFilePhase::CopyConfirmed {
                        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).expect("mode");
                    }
                    Ok(())
                }),
            )
            .expect_err("nieudane wycofanie");
            std::fs::set_permissions(&bench.data[0], std::fs::Permissions::from_mode(0o755)).expect("mode");
            let mut stored = root.load(&bench.spec.array_id).expect("load");
            // Stuck before the rename, whichever side of it the machine
            // reached: there is no destination to refuse a later run.
            let phase = stored.transfer.as_ref().expect("transfer").current.as_ref().expect("rekord").phase;
            assert!(
                file_phase_rank(phase) <= file_phase_rank(TransferFilePhase::RenameIntent),
                "rekord utknął przed zmianą nazwy: {phase:?}"
            );
            assert!(
                !bench.data[0].join("a.bin").exists(),
                "brak celu, który odmówiłby późniejszemu przebiegowi"
            );
            assert_eq!(tree(&bench.data[0]).len(), 1, "kopia tymczasowa zostaje");
            release(&root, &mut stored, MOVER_RESUME);
            assert_eq!(stored.stuck_paths.len(), 1, "ścieżka trafia na listę pominięć");
            write_aged(&bench.cache.join("b.bin"), b"second", 9000);
            let next = MoverRequest {
                operation_id: NEXT_OPERATION,
                resume_operation_id: NEXT_RESUME,
                ..mover_request(&rules, &array_lock)
            };
            run_mover(
                &root,
                &mut stored,
                &next,
                &bench.env(&roomy, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect("nowa operacja");
            assert_eq!(tree(&bench.cache), vec!["a.bin"], "utknięty plik zostaje na cache");
            let after = tree(&bench.data[0]);
            assert!(after.contains(&"b.bin".to_string()), "{after:?}");
            assert_eq!(
                after.iter().filter(|name| name.starts_with(".tentanas-transfer-")).count(),
                1,
                "żadnej drugiej kopii tymczasowej: {after:?}"
            );
            let transfer = root.load(&bench.spec.array_id).expect("stan").transfer.expect("transfer");
            let issue = transfer.issues.iter().find(|issue| issue.path == "a.bin").expect("pominięta ścieżka");
            assert_eq!(issue.reason, format!("rekord utknął w operacji {MOVER_OPERATION}; mover go nie rusza"));
        }

        #[test]
        fn save_refuses_a_skip_set_change_that_is_not_an_eviction() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stuck_paths = skip_seed(STUCK_PATH_LIMIT);
            seed(&root, &journal);
            // A removal on its own is never legal.
            let mut dropped = journal.clone();
            dropped.stuck_paths.remove(0);
            assert_eq!(
                root.save(&dropped).expect_err("samo usunięcie"),
                "sprzeczne przejście transferu: zmiana listy pominiętych ścieżek"
            );
            // A removal paired with an insert is legal only as the eviction of
            // a write that closes the operation that left the record.
            let mut rotated = journal.clone();
            let evicted = rotated.stuck_paths.remove(0);
            rotated.stuck_paths.push(StuckPath {
                path_sha256: path_digest("forged.bin"),
                operation_id: MOVER_OPERATION.into(),
                temporary: None,
            });
            rotated.stuck_evicted = 1;
            assert_eq!(
                root.save(&rotated).expect_err("wyparcie bez pierścienia"),
                "sprzeczne przejście transferu: pierścień wypartych ścieżek nie zgadza się z wyparciem"
            );
            // The ring must take the identity that actually left the set.
            let mut wrong_identity = rotated.clone();
            wrong_identity.evicted_paths.push(StuckPath {
                path_sha256: path_digest("someone-else.bin"),
                operation_id: MOVER_OPERATION.into(),
                temporary: None,
            });
            assert_eq!(
                root.save(&wrong_identity).expect_err("obca tożsamość w pierścieniu"),
                "sprzeczne przejście transferu: pierścień wypartych ścieżek nie zgadza się z wyparciem"
            );
            // And even with the ring right, only the write that closes the
            // operation may evict at all.
            let mut complete = rotated.clone();
            complete.evicted_paths.push(evicted);
            assert_eq!(
                root.save(&complete).expect_err("wyparcie bez zamknięcia"),
                "sprzeczne przejście transferu: utknięty rekord poza zamknięciem jego operacji"
            );
            // The counter moves with the eviction and only with it.
            let mut counted = journal.clone();
            counted.stuck_evicted = 1;
            assert_eq!(
                root.save(&counted).expect_err("licznik bez wyparcia"),
                "sprzeczne przejście transferu: licznik wypartych ścieżek nie zgadza się z wyparciem"
            );
            assert_eq!(
                root.load(&journal.spec.array_id).expect("stan").stuck_paths.len(),
                STUCK_PATH_LIMIT
            );
        }

        #[test]
        fn stuck_hidden_counts_the_paths_no_shown_record_stands_on() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            let (records, paths) = stuck_seed(STUCK_LIMIT);
            journal.stuck = records;
            journal.stuck_paths = paths;
            journal.stuck_paths.extend(skip_seed(STUCK_PATH_LIMIT - STUCK_LIMIT));
            assert_eq!(journal.stuck_paths.len(), STUCK_PATH_LIMIT);
            // Every shown record still has its digest.
            assert_eq!(stuck_hidden(&journal), (STUCK_PATH_LIMIT - STUCK_LIMIT) as u64);
            // One shown record loses its digest to an eviction: it is shown in
            // full, so it is not hidden — and it is no longer skipped either.
            journal.stuck_paths.remove(0);
            journal.stuck_paths.push(StuckPath {
                path_sha256: path_digest("newer.bin"),
                operation_id: MOVER_RESUME.into(),
                temporary: None,
            });
            journal.stuck_evicted = 1;
            assert_eq!(journal.stuck_paths.len(), STUCK_PATH_LIMIT);
            assert_eq!(stuck_hidden(&journal), (STUCK_PATH_LIMIT - STUCK_LIMIT + 1) as u64);
            let state = observe(&journal, None);
            assert_eq!(state.stuck_hidden, (STUCK_PATH_LIMIT - STUCK_LIMIT + 1) as u64);
            // The count alone would mislead: the record whose digest went is
            // still shown, and it says on its face that it is not skipped.
            assert!(
                !serde_json::to_string(&journal).expect("json").contains("skipped"),
                "wyprowadzona flaga nie zajmuje miejsca w dzienniku"
            );
            let shown = &state.stuck_records[0];
            assert!(!shown.skipped, "rekord bez swojego skrótu nie jest omijany");
            assert!(state.stuck_records[1..].iter().all(|record| record.skipped), "reszta jest omijana");
        }

        #[test]
        fn a_refused_journal_write_during_a_restart_leaves_a_short_stuck_record() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let (cache, target) = record_dirs(&dir);
            // Room for the record as it stands, none for its next durable step.
            let mut journal = near_limit_journal(&root, &cache, &target, 8);
            seed(&root, &journal);
            let record = journal.transfer.clone().expect("transfer").current.expect("rekord");
            let (bench, _bench_root, _) = mover_bench(1);
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            let mut host = TestHost::new(&bench, &snapraid);
            let array_lock = root.array_lock(&journal.spec.array_id).expect("lock");
            let error = finish_file(
                &root,
                &mut journal,
                &cache,
                &target,
                &array_lock,
                record.clone(),
                &mut host,
            )
            .err()
            .expect("zapis odrzucony");
            assert!(error.contains("limit odczytu"), "{error}");
            let released = root.load(&journal.spec.array_id).expect("stan po awaryjnym zapisie");
            let transfer = released.transfer.clone().expect("transfer");
            assert_eq!(transfer.current_failed.as_deref(), Some(SHORT_STUCK_REASON));
            assert_eq!(
                transfer.current.as_ref().map(|file| file.phase),
                Some(record.phase),
                "rekord zostaje taki, jaki był"
            );
            assert!(
                !transfer_blocks_resume(&released, &transfer, MOVER_RESUME),
                "Resume może zwolnić macierz"
            );
        }

        /// A journal carrying everything validation lets it carry, with its
        /// in-flight record grown until only `spare` bytes are left. Reaching
        /// the limit needs the record: without one, every other field together
        /// stays far below it.
        fn near_limit_journal(root: &Root, cache: &Path, target: &Path, spare: usize) -> Journal {
            // The record is a real file's, so the state machine can act on it.
            let plan = || {
                let scanned = crate::elastic_transfer::scan_cache(cache, |_| false)
                    .expect("skan")
                    .into_iter()
                    .find_map(|entry| match entry {
                        ScanEntry::File(file) if file.path == "w.bin" => Some(file),
                        _ => None,
                    })
                    .expect("plik skanu");
                crate::elastic_transfer::plan_file(
                    cache,
                    target,
                    "w.bin",
                    (scanned.device, scanned.inode),
                    &format!(".tentanas-transfer-{MOVER_OPERATION}-1"),
                )
                .expect("rekord pliku")
            };
            std::fs::write(cache.join("w.bin"), b"w").expect("plik");
            set_user_xattr(&cache.join("w.bin"), "user.blob", b"x");
            let quoted = |count: usize| "\"".repeat(count);
            let disk = |index: usize| ElasticDiskSpec {
                disk_id: format!("serial:{index:02}{}", "d".repeat(100)),
                wwn: Some(format!("wwn-{index:02}{}", "w".repeat(100))),
                serial: Some(format!("serial-{index:02}{}", "s".repeat(100))),
                bytes: 32 << 40,
                expected_uuid: format!("{index:08x}-cccc-4ccc-8ccc-cccccccccccc"),
            };
            let mut spec = spec();
            // Its own array, so this fixture can stand beside another one
            // in the same root.
            spec.array_id = "abababab-abab-4aba-8aba-abababababab".into();
            spec.name = "media-heavy".into();
            spec.data = (1..=29).map(disk).collect();
            spec.cache = Some(disk(30));
            spec.parity = vec![disk(31), disk(32)];
            spec.validate().expect("najcięższa dopuszczalna specyfikacja");
            let mut journal = root.reserve(&spec, boot(), || Ok(())).expect("reserve");
            journal.formatted = roles(&spec);
            journal.stage = ElasticStage::NeedsAttention;
            let private = journal.private.as_mut().expect("private");
            private.anchor = Some(Anchor {
                union_source: "b".repeat(ANCHOR_SOURCE_BYTES - 2),
                ..anchor_fixture()
            });
            private.service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            let worst_pin = ElasticFilePin {
                device: u64::MAX,
                inode: u64::MAX,
                size: Some(u64::MAX),
                sha256: Some("f".repeat(64)),
            };
            let (records, paths) = stuck_seed(STUCK_LIMIT);
            journal.stuck = records
                .iter()
                .map(|base| ElasticStuckRecord {
                    path: quoted(TRANSFER_TEXT_LIMIT),
                    temporary: Some(bound_temporary(&base.operation_id)),
                    source: worst_pin.clone(),
                    temporary_copy: Some(worst_pin.clone()),
                    destination: Some(worst_pin.clone()),
                    reason: "\\".repeat(TRANSFER_TEXT_LIMIT),
                    ..base.clone()
                })
                .collect();
            journal.stuck_paths = (0..STUCK_PATH_LIMIT)
                .map(|index| {
                    paths.get(index).cloned().unwrap_or_else(|| StuckPath {
                        path_sha256: path_digest(&format!("more/{index}.bin")),
                        operation_id: MOVER_RESUME.into(),
                        temporary: None,
                    })
                })
                .collect();
            journal.transfer = Some(TransferJournal {
                resume_operation_id: MOVER_RESUME.into(),
                rules: MoverRules {
                    pinned_folders: (0..128)
                        .map(|index| format!("{index:03}{}", quoted(8 * 1024 / 128 - 3)))
                        .collect(),
                    ..move_everything_aged()
                },
                target: Some("d1".into()),
                sequence: 1,
                current: Some(plan()),
                issues: (0..TRANSFER_ISSUE_LIMIT)
                    .map(|_| ElasticMoverIssue {
                        path: quoted(TRANSFER_TEXT_LIMIT),
                        kind: ElasticMoverIssueKind::Attention,
                        reason: "\\".repeat(TRANSFER_TEXT_LIMIT),
                    })
                    .collect(),
                detail: Some("krótko".into()),
                ..transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, false)
            });
            let size = serde_json::to_vec(&journal).expect("json").len();
            let room = (JOURNAL_LIMIT as usize - size).saturating_sub(spare) / 2;
            set_user_xattr(&cache.join("w.bin"), "user.blob", &vec![b'x'; room]);
            journal.transfer.as_mut().expect("transfer").current = Some(plan());
            journal
        }

        /// A private cache and one data branch for a crafted record.
        fn record_dirs(dir: &Temp) -> (PathBuf, PathBuf) {
            let cache = dir.0.join("cache");
            let target = dir.0.join("d1");
            std::fs::create_dir(&cache).expect("cache");
            std::fs::create_dir(&target).expect("target");
            (cache, target)
        }

        #[test]
        fn a_refused_attention_write_falls_back_to_a_short_detail() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let (cache, target) = record_dirs(&dir);
            let journal = near_limit_journal(&root, &cache, &target, 300);
            let size = serde_json::to_vec(&journal).expect("json").len() as u64;
            assert!(size <= JOURNAL_LIMIT && size + 600 > JOURNAL_LIMIT, "{size} B");
            seed(&root, &journal);
            // The error no longer fits over this state, so the note falls back
            // to a short fixed one instead of leaving the run looking alive.
            let error = mark_attention(&root, &journal.spec.array_id, MOVER_OPERATION, "x".repeat(900));
            assert!(error.starts_with('x'), "{error}");
            let stored = root.load(&journal.spec.array_id).expect("stan po awaryjnym zapisie");
            assert_eq!(stored.detail.as_deref(), Some(SHORT_ATTENTION));
            assert_eq!(stored.stage, ElasticStage::NeedsAttention);
            let transfer = stored.transfer.expect("transfer");
            assert_eq!(transfer.detail.as_deref(), Some(SHORT_ATTENTION));
            assert_eq!(transfer.phase, ElasticMoverPhase::NeedsAttention);
            assert!(transfer.issues.is_empty(), "miejsce odzyskane z listy zgłoszeń");
            assert!(transfer.current.is_some(), "rekord zostaje nietknięty");
        }

        #[test]
        fn the_projection_prices_the_coupled_sync_a_run_has_not_written_yet() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let (cache, target) = record_dirs(&dir);
            let mut journal = near_limit_journal(&root, &cache, &target, 4096);
            let record = journal.transfer.clone().expect("transfer").current.expect("rekord");
            // A new transfer may not carry a coupled result, so at plan time it
            // is ALWAYS absent — and the run still writes the whole record.
            journal.last_run = None;
            journal.stale_parity_bytes = None;
            journal.stale_sync_operation = None;
            journal.transfer.as_mut().expect("transfer").coupled_sync_result = None;
            let fresh = worst_case_journal_size(&journal, &record).expect("projekcja");
            // The same journal that already carries them must project the same
            // size: what the run will write is priced either way.
            let mut written = journal.clone();
            written.last_run = Some(worst_case_run(MOVER_OPERATION));
            written.stale_parity_bytes = Some(u64::MAX);
            written.stale_sync_operation = Some(MOVER_OPERATION.into());
            written.transfer.as_mut().expect("transfer").coupled_sync_result =
                Some(worst_case_run(MOVER_OPERATION));
            assert_eq!(
                fresh,
                worst_case_journal_size(&written, &record).expect("projekcja"),
                "projekcja nie zależy od tego, czy Sync jest już zapisany"
            );
            // The same holds for the two fields the closing write moves.
            let mut closed = journal.clone();
            let transfer = closed.transfer.as_mut().expect("transfer");
            transfer.phase = ElasticMoverPhase::NeedsAttention;
            transfer.finished_at = Some("2026-12-31T23:59:59Z".into());
            assert_eq!(
                fresh,
                worst_case_journal_size(&closed, &record).expect("projekcja"),
                "projekcja wycenia fazę i czas zakończenia, które zapisze zamknięcie"
            );
        }

        #[test]
        fn a_temporary_beyond_the_sequence_bound_is_not_a_readable_journal() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            let mut entry = StuckPath {
                path_sha256: path_digest("over/1.bin"),
                operation_id: MOVER_RESUME.into(),
                temporary: Some(bound_temporary(MOVER_RESUME)),
            };
            journal.stuck_paths = vec![entry.clone()];
            validate_topology(&journal).expect("nazwa na granicy jest dopuszczalna");
            // One digit more than the validators admit.
            entry.temporary = Some(format!(
                ".tentanas-transfer-{MOVER_RESUME}-{}",
                "9".repeat(TEMPORARY_SEQUENCE_DIGITS + 1)
            ));
            journal.stuck_paths = vec![entry];
            assert_eq!(
                validate_topology(&journal).expect_err("ponad limit cyfr"),
                "nieprawidłowa lista pominiętych ścieżek"
            );
        }

        #[test]
        fn save_never_writes_a_journal_that_load_would_refuse() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = ready_service_journal(&root);
            let path = root.path.join(format!("{}.json", journal.spec.array_id));
            let before = std::fs::read(&path).expect("bytes");
            let mut oversized = journal.clone();
            oversized.detail = Some("x".repeat(JOURNAL_LIMIT as usize));
            assert_eq!(
                root.save(&oversized).expect_err("zbyt długi opis"),
                "zbyt długi opis stanu macierzy"
            );
            // And a journal whose every field is within its own bound, but
            // which together no longer fits.
            let (cache, target) = record_dirs(&dir);
            let mut heavy = near_limit_journal(&root, &cache, &target, 300);
            seed(&root, &heavy);
            heavy.detail = Some("x".repeat(TRANSFER_DETAIL_LIMIT));
            assert_eq!(
                root.save(&heavy).expect_err("zbyt duży journal"),
                "journal przekroczyłby limit odczytu"
            );
            let mut foreign_target = journal.clone();
            foreign_target.transfer = Some(TransferJournal {
                target: Some("d9".into()),
                ..transfer_fixture("66666666-6666-4666-8666-666666666666", ElasticMoverPhase::Holding, false)
            });
            assert!(root.save(&foreign_target).is_err());
            assert_eq!(std::fs::read(&path).expect("bytes"), before);
            root.load(&journal.spec.array_id).expect("poprzedni journal pozostaje czytelny");
        }

        #[test]
        fn mover_rules_stay_within_the_journal_bound() {
            assert!(MoverRules { pinned_folders: vec!["foto\u{7}".into()], ..MoverRules::default() }
                .validate()
                .is_err());
            assert!(MoverRules {
                eager_folders: (0..200).map(|index| format!("f{index}")).collect(),
                ..MoverRules::default()
            }
            .validate()
            .is_err());
            assert!(MoverRules { pinned_folders: vec!["a".repeat(4000); 5], ..MoverRules::default() }
                .validate()
                .is_err());
            assert!(MoverRules { pinned_folders: vec!["/foto".into()], ..MoverRules::default() }
                .validate()
                .is_err());
            MoverRules {
                pinned_folders: vec!["foto".into()],
                eager_folders: vec!["inbox/new".into()],
                ..MoverRules::default()
            }
            .validate()
            .expect("zwykłe reguły");
        }

        #[test]
        fn resume_closes_only_the_transfer_that_declared_it() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.transfer = Some(transfer_fixture(
                "66666666-6666-4666-8666-666666666666",
                ElasticMoverPhase::NeedsAttention,
                false,
            ));
            seed(&root, &journal);
            let mut foreign = journal.clone();
            close_transfer(&mut foreign, FOREIGN_RESUME).expect("obcy Resume");
            assert!(foreign.transfer.unwrap().finished_at.is_none());
            assert!(restore_checkpoint_guard(&journal).is_err());
            close_transfer(&mut journal, "77777777-7777-4777-8777-777777777777").expect("zapowiedziany Resume");
            let closed = journal.transfer.clone().unwrap();
            assert_eq!(closed.phase, ElasticMoverPhase::NeedsAttention);
            assert!(closed.finished_at.is_some());
            // A closed run is history: it no longer blocks Restore, Sync or Scrub.
            restore_checkpoint_guard(&journal).expect("historia nie blokuje Restore");
            root.save(&journal).expect("zamknięty transfer");
        }

        #[test]
        fn enter_service_refuses_foreign_operation_while_the_mover_owns_the_hold() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            write_aged(&bench.cache.join("a.bin"), b"payload", 9000);
            let broken = |_: &Path| -> Result<(u64, u64), String> { Err("statvfs: Input/output error".into()) };
            let rules = move_everything_aged();
            let array_lock = root.array_lock(&bench.spec.array_id).expect("lock");
            let snapraid = FakeSnapraid::new(&bench.dir.0, &[0]);
            run_mover(
                &root,
                &mut journal,
                &mover_request(&rules, &array_lock),
                &bench.env(&broken, &nothing_open),
                &mut TestHost::new(&bench, &snapraid),
            )
            .expect_err("przerwany mover");
            for (moment, finished) in [("niedokończony", false), ("zakończony", true)] {
                if finished {
                    run_mover(
                        &root,
                        &mut journal,
                        &mover_request(&rules, &array_lock),
                        &bench.env(&roomy, &nothing_open),
                        &mut TestHost::new(&bench, &snapraid),
                    )
                    .expect("wznowienie");
                    assert_eq!(tree(&bench.data[0]), vec!["a.bin"]);
                }
                let before = bench.journal_bytes();
                let mut calls = 0;
                assert!(
                    enter_service_with(&root, journal.clone(), FOREIGN_RESUME, &array_lock, || {
                        calls += 1;
                        Ok(())
                    })
                    .is_err(),
                    "{moment}"
                );
                assert_eq!(calls, 0, "obca operacja nie może przełączyć unii: {moment}");
                assert_eq!(bench.journal_bytes(), before, "{moment}");
            }
            // Only the declared Resume releases the finished run's Hold.
            assert!(authorize_resume(&root, &mut journal.clone(), FOREIGN_RESUME).is_err());
            authorize_resume(&root, &mut journal, MOVER_RESUME).expect("zapowiedziany Resume");
        }

        #[test]
        fn mover_rules_are_validated_on_every_journal_load() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (bench, root, mut journal) = mover_bench(1);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: MOVER_OPERATION.into(),
                pending: false,
            });
            let mut transfer = transfer_fixture(MOVER_OPERATION, ElasticMoverPhase::Moving, true);
            transfer.rules.pinned_folders = vec!["../escape".into()];
            journal.transfer = Some(transfer);
            let bytes = serde_json::to_vec(&journal).expect("json");
            atomic_write(&bench.journal_path(), &bytes, root.uid).expect("zapis");
            assert!(root.load(&bench.spec.array_id).is_err());
        }

        #[test]
        fn schema1_without_service_rejects_service_guard() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = legacy_journal(&root);
            assert!(service_guard(&journal, "55555555-5555-4555-8555-555555555555").is_err());
        }

        #[test]
        fn finish_online_pending_without_worker_preserves_journal_bytes() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::Mounting;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Online,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: true,
            });
            root.save(&journal).expect("pending");
            let path = root.path.join(format!("{}.json", journal.spec.array_id));
            let before = std::fs::read(&path).expect("bytes");
            assert!(finish(&root, &mut journal, Ok(()), None).is_err());
            assert_eq!(std::fs::read(path).expect("bytes"), before);
            assert!(journal.private.unwrap().service.unwrap().pending);
        }

        #[test]
        fn finish_error_preserves_online_pending_after_reopen() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::Mounting;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Online,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: true,
            });
            root.save(&journal).expect("pending");
            finish(&root, &mut journal, Err("restore failed".into()), None).expect("failure saved");
            drop(root);
            let reopened = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("reopen");
            let stored = reopened.load(&journal.spec.array_id).expect("load");
            assert_eq!(stored.stage, ElasticStage::NeedsAttention);
            assert!(stored.private.unwrap().service.unwrap().pending);
        }

        #[test]
        fn service_json_rejects_null_unknown_mode_and_unknown_field() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let journal = ready_service_journal(&root);
            let mut valid = serde_json::to_value(&journal).expect("json");
            valid["stage"] = serde_json::json!("needs_attention");
            valid["private"]["service"] = serde_json::json!({
                "mode":"hold",
                "operation_id":"55555555-5555-4555-8555-555555555555",
                "pending":false
            });
            assert!(decode_journal(&serde_json::to_vec(&valid).expect("json")).is_ok());
            let base = valid.clone();
            for service in [
                serde_json::Value::Null,
                serde_json::json!({"mode":"unknown","operation_id":"55555555-5555-4555-8555-555555555555","pending":false}),
                serde_json::json!({"mode":"hold","operation_id":"55555555-5555-4555-8555-555555555555","pending":false,"extra":true}),
                serde_json::json!({"mode":"hold","operation_id":journal.spec.operation_id,"pending":false}),
            ] {
                let mut value = base.clone();
                value["private"]["service"] = service;
                assert!(decode_journal(&serde_json::to_vec(&value).expect("json")).is_err());
            }
            let mut missing = base;
            missing["private"].as_object_mut().unwrap().remove("service");
            assert!(decode_journal(&serde_json::to_vec(&missing).expect("json")).is_ok());
            let encoded = serde_json::to_string(&valid).expect("json");
            let service_object = "{\"mode\":\"hold\",\"operation_id\":\"55555555-5555-4555-8555-555555555555\",\"pending\":false}";
            for duplicate_json in [
                encoded.replace("\"mode\":\"hold\",", "\"mode\":\"hold\",\"mode\":\"hold\",").to_string(),
                encoded.replace("\"pending\":false", "\"pending\":false,\"pending\":false"),
                encoded.replace(&format!("\"service\":{service_object}"), &format!("\"service\":{service_object},\"service\":{service_object}")),
            ] {
                assert!(serde_json::from_str::<serde_json::Value>(&duplicate_json).is_ok());
                assert!(decode_journal(duplicate_json.as_bytes()).is_err());
            }
        }

        #[test]
        fn root_save_rejects_removing_service_without_changing_bytes() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            journal.stage = ElasticStage::NeedsAttention;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Hold,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: false,
            });
            root.save(&journal).expect("service");
            let path = root.path.join(format!("{}.json", journal.spec.array_id));
            let before = std::fs::read(&path).expect("bytes");
            journal.private.as_mut().unwrap().service = None;
            assert!(root.save(&journal).is_err());
            assert_eq!(std::fs::read(path).expect("bytes"), before);
        }

        #[test]
        fn authorize_publication_rejects_hold_with_durable_union_pending() {
            let dir = Temp::new();
            let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
            let mut journal = ready_service_journal(&root);
            let actual_boot = boot_id().expect("boot");
            let mut anchor = anchor_fixture();
            anchor.boot_id = actual_boot.clone();
            journal.boot_id = actual_boot;
            journal.stage = ElasticStage::Mounting;
            journal.pending = Some(Pending::Union);
            journal.private.as_mut().unwrap().anchor = Some(anchor.clone());
            journal.private.as_mut().unwrap().published = false;
            journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                mode: ElasticServiceMode::Online,
                operation_id: "55555555-5555-4555-8555-555555555555".into(),
                pending: false,
            });
            root.save(&journal).expect("pending");
            authorize_publication(&root, &spec(), &anchor).expect("online publication");
            journal.private.as_mut().unwrap().service.as_mut().unwrap().mode = ElasticServiceMode::Hold;
            root.save(&journal).expect("hold");
            let before = std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes");
            let error = authorize_publication(&root, &spec(), &anchor).expect_err("hold publication");
            assert!(error.contains("service Hold"));
            assert_eq!(std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes"), before);
        }

        #[test]
        fn restore_checkpoint_guard_rejects_hold_after_reopen_in_both_boots() {
            let _isolation = FORK_REOPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for pending in [false, true] {
                for saved_boot in [boot_id().expect("boot"), boot()] {
                    let dir = Temp::new();
                    let root = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("root");
                    let mut journal = ready_service_journal(&root);
                    journal.stage = ElasticStage::NeedsAttention;
                    journal.private.as_mut().unwrap().service = Some(ElasticServiceState {
                        mode: ElasticServiceMode::Hold,
                        operation_id: "55555555-5555-4555-8555-555555555555".into(),
                        pending,
                    });
                    journal.boot_id = saved_boot.clone();
                    journal.private.as_mut().unwrap().anchor.as_mut().unwrap().boot_id = saved_boot;
                    root.save(&journal).expect("hold");
                    let before = std::fs::read(root.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes");
                    drop(root);
                    let reopened = Root::open(&dir.0, unsafe { libc::geteuid() }).expect("reopen");
                    let stored = reopened.load(&journal.spec.array_id).expect("load");
                    let command = crate::HelperCommand::ElasticRestore {
                        array_id: stored.spec.array_id.clone(),
                        owner: stored.spec.owner.clone(),
                    };
                    let error = private_operation(&reopened, stored.clone(), &command).expect_err("hold restore");
                    assert!(error.contains("Restore nie konsumuje trwałego service Hold"));
                    assert_eq!(std::fs::read(reopened.path.join(format!("{}.json", journal.spec.array_id))).expect("bytes"), before);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn spec() -> ElasticSpec {
        ElasticSpec {
            name: "media".to_string(),
            filesystem: "xfs".to_string(),
            data: vec![
                Branch {
                    disk: "sdg".to_string(),
                    device: "/dev/sdg".to_string(),
                },
                Branch {
                    disk: "sdh".to_string(),
                    device: "/dev/sdh".to_string(),
                },
            ],
            cache: vec![Branch {
                disk: "nvme2n1".to_string(),
                device: "/dev/nvme2n1".to_string(),
            }],
            parity: vec![ParityDisk {
                index: 1,
                disk: "sdj".to_string(),
                device: "/dev/sdj".to_string(),
            }],
            mergerfs: MergerfsOptions::default(),
            snapraid: SnapraidOptions::default(),
        }
    }

    fn tools() -> Tools {
        Tools::for_preview()
    }

    /// The topology claim of §5.3, checked where it is decided: the CACHE and
    /// the DATA disks are branches of ONE union, and the parity disk is not a
    /// branch at all.
    ///
    /// What would break if this were wrong: the mover would be moving files
    /// between two different mounts, so every client path would change under
    /// it — and a parity disk in the branch list would put the array's own
    /// protection inside a share.
    #[test]
    fn the_union_spans_cache_and_data_and_never_parity() {
        let s = spec();
        let branches = s.branch_specs();
        assert_eq!(branches.len(), 3, "one cache branch and two data branches");
        assert!(
            branches[0].starts_with(&cache_branch_path("media", "nvme2n1")),
            "the cache is the first branch: {branches:?}"
        );
        for disk in ["sdg", "sdh"] {
            assert!(
                branches
                    .iter()
                    .any(|b| b.starts_with(&data_branch_path("media", disk))),
                "{disk} is missing from the union: {branches:?}"
            );
        }
        assert!(
            !branches.iter().any(|b| b.contains("/parity/")),
            "a parity disk must never be a branch of the union: {branches:?}"
        );
        // And every branch really is under ONE mountpoint's branch tree.
        assert!(branches.iter().all(|b| b.starts_with(&branch_root("media"))));
    }

    /// New files land on the cache because the data branches are `=NC`, and
    /// they land on the data disks when there is no cache. This is the
    /// mechanism the whole cache feature rests on.
    #[test]
    fn branch_modes_send_new_files_to_the_cache_only_when_there_is_one() {
        let cached = spec().branch_specs();
        assert!(cached[0].ends_with("=RW"), "the cache branch takes creates: {cached:?}");
        assert!(
            cached[1..].iter().all(|b| b.ends_with("=NC")),
            "with a cache present the data branches must not accept creates: {cached:?}"
        );

        let mut uncached = spec();
        uncached.cache.clear();
        let modes = uncached.branch_specs();
        assert_eq!(modes.len(), 2);
        assert!(
            modes.iter().all(|b| b.ends_with("=RW")),
            "without a cache every data branch has to accept creates, or the array is \
             read-only: {modes:?}"
        );
    }

    /// The parity rule that keeps parity out of the union has a second half:
    /// the snapraid config lists the DATA disks and never the cache. A cache
    /// listed as a data disk would make snapraid report the array protected
    /// while the mover moved blocks out from under it.
    #[test]
    fn the_snapraid_config_covers_the_data_disks_and_not_the_cache() {
        let s = spec();
        let text = snapraid_config(&s).expect("config");
        let data_lines: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("data "))
            .collect();
        assert_eq!(data_lines.len(), 2, "one line per data disk: {data_lines:?}");
        assert!(data_lines.iter().any(|l| l.contains("/data/sdg")));
        assert!(data_lines.iter().any(|l| l.contains("/data/sdh")));
        assert!(
            !text.contains("/cache/"),
            "the cache must not appear anywhere in the snapraid config:\n{text}"
        );
        // The parity directive names the parity FILE on the parity mount.
        assert!(
            text.contains(&format!("parity {}", parity_file_path("media", 1))),
            "{text}"
        );
        let content: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("content "))
            .collect();
        assert_eq!(
            content,
            vec![
                "content /etc/tentanas/media-snapraid.content",
                "content /mnt/tentanas-branches/media/parity/1/snapraid.content",
            ]
        );
    }

    #[test]
    fn content_copies_stay_outside_union_branches_for_every_parity_count() {
        for data_count in [1, 2] {
            for parity_count in 0..=2 {
                for with_cache in [false, true] {
                    let mut s = spec();
                    s.data.truncate(data_count);
                    if !with_cache {
                        s.cache.clear();
                    }
                    s.parity.push(ParityDisk {
                        index: 2,
                        disk: "sdk".to_string(),
                        device: "/dev/sdk".to_string(),
                    });
                    s.parity.truncate(parity_count);
                    let steps = plan_create(&s, &tools()).expect("plan");
                    let writes: Vec<_> = steps
                        .iter()
                        .filter_map(|step| match step {
                            ElasticStep::WriteFile { path, content, .. } => Some((path, content)),
                            _ => None,
                        })
                        .collect();
                    if parity_count == 0 {
                        assert!(snapraid_config(&s).is_err());
                        assert!(writes.is_empty());
                        assert!(!steps
                            .iter()
                            .any(|step| matches!(step, ElasticStep::Run { .. })));
                        continue;
                    }
                    let text = snapraid_config(&s).expect("config");
                    assert_eq!(writes.len(), 1);
                    assert_eq!(writes[0].0, "/etc/tentanas/snapraid-media.conf");
                    assert_eq!(writes[0].1, &text);
                    let content: Vec<&str> = text
                        .lines()
                        .filter_map(|line| line.strip_prefix("content "))
                        .collect();
                    let mut expected = vec![
                        "/etc/tentanas/media-snapraid.content",
                        "/mnt/tentanas-branches/media/parity/1/snapraid.content",
                    ];
                    if parity_count == 2 {
                        expected.push("/mnt/tentanas-branches/media/parity/2/snapraid.content");
                    }
                    assert_eq!(content, expected);
                    assert_eq!(content.len(), parity_count + 1);
                    let branches = s.branch_specs();
                    for copy in content {
                        let copy = Path::new(copy);
                        assert_ne!(copy, Path::new(writes[0].0));
                        for branch in &branches {
                            let (path, _) = branch.rsplit_once('=').expect("branch mode");
                            assert!(
                                !copy.starts_with(Path::new(path)),
                                "plik content {} wpada pod branch {path}",
                                copy.display()
                            );
                        }
                    }
                }
            }
        }
    }

    /// A second parity disk changes the directive as well as the file — the
    /// pair that is easy to get half right.
    #[test]
    fn the_second_parity_disk_uses_its_own_directive_and_file() {
        let mut s = spec();
        s.parity.push(ParityDisk {
            index: 2,
            disk: "sdk".to_string(),
            device: "/dev/sdk".to_string(),
        });
        let text = snapraid_config(&s).expect("config");
        assert!(text.contains("\nparity /mnt/tentanas-branches/media/parity/1/snapraid.parity\n"), "{text}");
        assert!(
            text.contains("\n2-parity /mnt/tentanas-branches/media/parity/2/snapraid.2-parity\n"),
            "{text}"
        );
        assert_eq!(parity_directive(1).unwrap(), "parity");
        assert_eq!(parity_directive(2).unwrap(), "2-parity");
        assert!(parity_directive(3).is_err(), "the wizard offers two, so three has no spelling");
    }

    /// An array with no parity gets no config file and no sync step — and says
    /// so out loud rather than leaving a silence.
    #[test]
    fn an_array_without_parity_writes_no_config_and_runs_no_sync() {
        let mut s = spec();
        s.parity.clear();
        assert!(
            snapraid_config(&s).is_err(),
            "a config with no parity line is a file snapraid refuses on every run"
        );
        let steps = plan_create(&s, &tools()).expect("plan");
        assert!(
            !steps.iter().any(|s| matches!(s, ElasticStep::WriteFile { .. })),
            "no snapraid config may be written for an array with no parity"
        );
        assert!(
            !steps.iter().any(|s| matches!(s, ElasticStep::Run { .. })),
            "there is nothing to sync"
        );
        let text = render(&steps);
        assert!(
            text.contains("nothing in this array is protected"),
            "the plan has to SAY that this array has no protection:\n{text}"
        );
        assert!(plan_snapraid(&s, &SnapraidAction::Sync, &tools()).is_err());
    }

    /// The create plan wipes exactly the disks it was given — every data,
    /// cache and parity disk, and nothing else.
    ///
    /// The red button's count comes from this list, so a plan that wiped a
    /// fourth disk while the button said three is the failure this catches.
    #[test]
    fn the_create_plan_wipes_exactly_the_picked_disks() {
        let steps = plan_create(&spec(), &tools()).expect("plan");
        let mut wiped = wiped_devices(&steps);
        wiped.sort();
        assert_eq!(
            wiped,
            vec!["/dev/nvme2n1", "/dev/sdg", "/dev/sdh", "/dev/sdj"],
            "every picked disk, and only those"
        );
        assert_eq!(
            steps.iter().filter(|s| s.is_destructive()).count(),
            4,
            "one mkfs per disk, never two"
        );
        let text = render(&steps);
        for device in &wiped {
            assert!(
                text.contains(&format!("WIPE mkfs.xfs {device} filesystem=xfs")),
                "the render has to name every wipe:\n{text}"
            );
        }
    }

    /// Order matters and is asserted as an order, not as a set: a branch
    /// mounted after the union would be hidden underneath it, and a union
    /// mounted before its branches exist would put every client write on the
    /// root filesystem.
    #[test]
    fn the_create_plan_mounts_every_branch_before_the_union() {
        let steps = plan_create(&spec(), &tools()).expect("plan");
        let union_at = steps
            .iter()
            .position(|s| matches!(s, ElasticStep::MergerfsMount { .. }))
            .expect("the union is mounted");
        let branch_mounts: Vec<usize> = steps
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, ElasticStep::Mount { .. }))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(branch_mounts.len(), 4, "two data, one cache, one parity");
        assert!(
            branch_mounts.iter().all(|i| *i < union_at),
            "branches at {branch_mounts:?}, union at {union_at}"
        );
        // …and the first sync is after the union, because the config it reads
        // names branch paths that have to exist.
        let sync_at = steps
            .iter()
            .position(|s| matches!(s, ElasticStep::Run { .. }))
            .expect("the first sync");
        assert!(sync_at > union_at, "sync at {sync_at}, union at {union_at}");
        // Every mkfs precedes the mount of the same mountpoint.
        for (i, step) in steps.iter().enumerate() {
            if let ElasticStep::Mount { source, .. } = step {
                let mkfs_at = steps.iter().position(|s| {
                    matches!(s, ElasticStep::Mkfs { device, .. } if device == source)
                });
                assert!(
                    mkfs_at.is_some_and(|m| m < i),
                    "{source} is mounted at {i} before it is formatted"
                );
            }
        }
    }

    /// The reconcile plan can NEVER format anything.
    ///
    /// It runs unattended after every reboot (§3.4), so one wrong observation
    /// would otherwise be one `mkfs` away from destroying the array it was
    /// putting back. This is the single most important assertion in the file.
    #[test]
    fn the_reconcile_plan_never_formats_a_disk() {
        let s = spec();
        let steps = plan_mount(&s, &Observed::nothing_mounted(), &tools()).expect("plan");
        assert!(
            wiped_devices(&steps).is_empty(),
            "a restore plan must not contain a single mkfs: {}",
            render(&steps)
        );
        // It still does the whole job: four branches and the union.
        assert_eq!(
            steps.iter().filter(|s| matches!(s, ElasticStep::Mount { .. })).count(),
            4
        );
        assert_eq!(
            steps
                .iter()
                .filter(|s| matches!(s, ElasticStep::MergerfsMount { .. }))
                .count(),
            1
        );
    }

    /// A reconcile skips what is already mounted, and says so about the union
    /// instead of leaving nothing in the log.
    #[test]
    fn the_reconcile_plan_only_mounts_what_is_missing() {
        let s = spec();
        let mut observed = Observed::nothing_mounted();
        observed
            .mounted
            .insert(data_branch_path("media", "sdg"), true);
        observed
            .mounted
            .insert(cache_branch_path("media", "nvme2n1"), true);
        observed.mounted.insert(union_path("media"), true);

        let steps = plan_mount(&s, &observed, &tools()).expect("plan");
        let mounted: Vec<&str> = steps
            .iter()
            .filter_map(|s| match s {
                ElasticStep::Mount { mountpoint, .. } => Some(mountpoint.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            mounted,
            vec![
                data_branch_path("media", "sdh").as_str(),
                parity_mount_path("media", 1).as_str(),
            ],
            "only the branches that were not mounted"
        );
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, ElasticStep::MergerfsMount { .. })),
            "a mounted union must not be mounted a second time"
        );
        assert!(render(&steps).contains("is already mounted"));
    }

    /// An unreadable mount table produces NO plan at all.
    ///
    /// "Unknown is not zero" with real consequences: treating an unreadable
    /// table as "nothing is mounted" mounts the union over empty directories,
    /// and every client write then lands on the root filesystem and disappears
    /// the moment the real branches are mounted.
    #[test]
    fn an_unknown_mount_table_refuses_to_produce_a_plan() {
        let unknown = Observed {
            known: false,
            mounted: BTreeMap::new(),
        };
        let err = plan_mount(&spec(), &unknown, &tools()).expect_err("must refuse");
        let CatalogError::InvalidArgument(detail) = err else {
            panic!("wrong error kind");
        };
        assert!(detail.contains("mount table"), "{detail}");
        // The same reading with `known` set is a normal plan — so the refusal
        // is about the flag and not about the empty map.
        assert!(plan_mount(&spec(), &Observed::nothing_mounted(), &tools()).is_ok());
    }

    /// The coupled sync is IN the mover plan, as one sequence — and when it is
    /// switched off the plan says the window stays open instead of going
    /// quiet.
    #[test]
    fn the_mover_plan_carries_its_coupled_sync_as_one_sequence() {
        let s = spec();
        let rules = MoverRules {
            min_age_secs: 7200,
            min_free_pct: 20,
            pinned_folders: vec!["foto".to_string()],
            eager_folders: Vec::new(),
            skip_open_files: true,
        };
        let coupled = plan_mover(&s, &rules, true, &tools()).expect("plan");
        assert_eq!(coupled.len(), 2, "the move and the sync, in that order");
        assert!(matches!(coupled[0], ElasticStep::MoveFiles { .. }));
        let ElasticStep::Run { args, .. } = &coupled[1] else {
            panic!("the second step must be the sync: {coupled:?}");
        };
        assert!(args.contains(&"sync".to_string()), "{args:?}");

        let alone = plan_mover(&s, &rules, false, &tools()).expect("plan");
        assert!(
            !alone.iter().any(|s| matches!(s, ElasticStep::Run { .. })),
            "with the coupling off nothing may sync"
        );
        assert!(
            render(&alone).contains("outside parity until the next sync"),
            "{}",
            render(&alone)
        );

        // The rules reach the rendering, including the one that decides
        // whether an open file is moved underneath its writer.
        let text = render(&coupled);
        assert!(text.contains("older than 7200s"), "{text}");
        assert!(text.contains("extra run below 20% cache free"), "{text}");
        assert!(text.contains("skipping open files"), "{text}");
        assert!(text.contains("keep on cache: foto"), "{text}");
    }

    /// A mover on an array with no cache is a refusal, not an empty run:
    /// there is no branch to move from.
    #[test]
    fn a_mover_without_a_cache_is_refused() {
        let mut s = spec();
        s.cache.clear();
        assert!(plan_mover(&s, &MoverRules::default(), true, &tools()).is_err());
    }

    /// Dissolving keeps every byte: no mkfs, and the union comes down before
    /// the branches it sits on.
    #[test]
    fn dissolving_an_array_destroys_nothing_and_unmounts_top_down() {
        let steps = plan_dissolve(&spec()).expect("plan");
        assert!(wiped_devices(&steps).is_empty(), "{}", render(&steps));
        let unmounts: Vec<&str> = steps
            .iter()
            .filter_map(|s| match s {
                ElasticStep::Unmount { mountpoint } => Some(mountpoint.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            unmounts.first().copied(),
            Some(union_path("media").as_str()),
            "the union has to come down first, or its branches are busy: {unmounts:?}"
        );
        assert_eq!(unmounts.len(), 5, "the union, one cache, two data, one parity");
        assert!(render(&steps).contains("mountable on its own"));
    }

    /// Adding one disk: format the new one only, remount the union with it,
    /// re-write the config, sync. The "add a disk at any time" claim of §5.3
    /// is only true if the plan touches nothing else.
    #[test]
    fn adding_a_disk_formats_only_the_new_one_and_ends_in_a_sync() {
        let mut after = spec();
        let added = Branch {
            disk: "sdi".to_string(),
            device: "/dev/sdi".to_string(),
        };
        after.data.push(added.clone());
        let steps = plan_add_data_disk(&after, &added, &tools()).expect("plan");
        assert_eq!(
            wiped_devices(&steps),
            vec!["/dev/sdi"],
            "only the disk being added may be formatted"
        );
        let last = steps.last().expect("a step");
        let ElasticStep::Run { args, .. } = last else {
            panic!("the plan has to end in a sync: {last:?}");
        };
        assert!(args.contains(&"sync".to_string()));

        // THE UNION STAYS UP. MEASURED (2026-09-06, mergerfs 2.42.0): a
        // branch can be added to a running mount, so the array's headline
        // operation must not drop every client's handles. This assertion is
        // the one that would catch a return to the remount version.
        assert!(
            !steps.iter().any(|s| matches!(s, ElasticStep::Unmount { .. })),
            "adding a disk must not take the union down: {}",
            render(&steps)
        );
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, ElasticStep::MergerfsMount { .. })),
            "and it must not mount a second union over the first"
        );
        let ElasticStep::AddBranch { union, branch } = steps
            .iter()
            .find(|s| matches!(s, ElasticStep::AddBranch { .. }))
            .expect("the branch is added to the live union")
        else {
            unreachable!()
        };
        assert_eq!(union, &union_path("media"));
        // THE INVARIANT: what the online add sends must be byte for byte the
        // entry the next reboot's mount would produce for the same disk.
        // Anything else means the array behaves one way now and another after
        // a restart — which is exactly what happened when this carried a bare
        // path: the new disk joined `RW` beside `NC` siblings, won `mfs` on
        // every write, and new files stopped reaching the cache until the
        // next reboot put it back.
        assert!(
            after.branch_specs().contains(branch),
            "{branch} is not one of the branches a remount would build: {:?}",
            after.branch_specs()
        );
        assert_eq!(branch, &format!("{}=NC", data_branch_path("media", "sdi")));

        // And on an array with NO cache the same disk joins `RW`, because
        // there is nothing to steer writes towards.
        let mut uncached = after.clone();
        uncached.cache.clear();
        let steps_uncached = plan_add_data_disk(&uncached, &added, &tools()).expect("plan");
        let ElasticStep::AddBranch { branch, .. } = steps_uncached
            .iter()
            .find(|s| matches!(s, ElasticStep::AddBranch { .. }))
            .expect("the branch is added")
        else {
            unreachable!()
        };
        assert_eq!(branch, &format!("{}=RW", data_branch_path("media", "sdi")));
        assert!(uncached.branch_specs().contains(branch));
        // The branch is MOUNTED before it is added, or the union would take
        // an empty directory on the root filesystem as a data disk.
        let mount_at = steps
            .iter()
            .position(|s| matches!(s, ElasticStep::Mount { .. }))
            .expect("the new disk is mounted");
        let add_at = steps
            .iter()
            .position(|s| matches!(s, ElasticStep::AddBranch { .. }))
            .expect("the branch is added");
        assert!(mount_at < add_at, "mount at {mount_at}, add at {add_at}");
        assert!(
            render(&steps).contains("user.mergerfs.srcmounts"),
            "{}",
            render(&steps)
        );
        // A disk that is not in the resulting array is refused rather than
        // formatted on the strength of the argument alone.
        let stranger = Branch {
            disk: "sdz".to_string(),
            device: "/dev/sdz".to_string(),
        };
        assert!(plan_add_data_disk(&after, &stranger, &tools()).is_err());
    }

    /// Ręczny scrub obejmuje całość, a fix może wskazać tylko własny dysk.
    #[test]
    fn snapraid_arguments_come_from_the_arrays_own_settings() {
        let mut s = spec();
        s.snapraid.scrub_percent = 8;
        s.snapraid.scrub_older_than_days = 10;
        let scrub = snapraid_args(&s, &SnapraidAction::Scrub).expect("args");
        assert_eq!(
            scrub,
            vec![
                "-c".to_string(),
                config_path("media"),
                "-p".to_string(),
                "full".to_string(),
                "scrub".to_string(),
            ]
        );
        s.snapraid.scrub_percent = 25;
        assert_eq!(snapraid_args(&s, &SnapraidAction::Scrub).unwrap(), scrub);
        s.snapraid.scrub_percent = 0;
        assert_eq!(snapraid_args(&s, &SnapraidAction::Scrub).unwrap(), scrub);

        let s = spec();
        let fix = snapraid_args(
            &s,
            &SnapraidAction::Fix {
                disk: "sdg".to_string(),
            },
        )
        .expect("args");
        assert_eq!(fix.last().map(String::as_str), Some("fix"));
        assert!(fix.windows(2).any(|w| w == ["-d", "sdg"]), "{fix:?}");
        assert!(
            snapraid_args(
                &s,
                &SnapraidAction::Fix {
                    disk: "sdj".to_string()
                }
            )
            .is_err(),
            "the parity disk is not a data disk, so it cannot be fixed from parity"
        );
        assert!(
            snapraid_args(
                &s,
                &SnapraidAction::Fix {
                    disk: "nvme2n1".to_string()
                }
            )
            .is_err(),
            "the cache is not in parity at all"
        );
    }

    /// Every field of `MergerfsOptions` reaches the mount, and the enumeration
    /// really is exhaustive.
    ///
    /// The floor matters: a `mergerfs_options` that returned an empty list
    /// would otherwise pass a test that only looked for the absence of
    /// something.
    #[test]
    fn every_mergerfs_option_reaches_the_mount() {
        let opts = MergerfsOptions::default();
        let rendered = mergerfs_options(&opts).expect("options");
        assert!(rendered.len() >= 6, "too few options: {rendered:?}");
        assert!(rendered.contains(&"category.create=mfs".to_string()));
        assert!(rendered.contains(&"minfreespace=20G".to_string()));
        assert!(rendered.contains(&"cache.files=off".to_string()));
        assert!(rendered.contains(&"moveonenospc=true".to_string()));
        assert!(rendered.contains(&"allow_other".to_string()));
        assert!(rendered.contains(&"func.getattr=newest".to_string()));

        // Flipping a flag removes exactly its option.
        let off = MergerfsOptions {
            move_on_enospc: false,
            allow_other: false,
            getattr_newest: false,
            ..MergerfsOptions::default()
        };
        let rendered = mergerfs_options(&off).expect("options");
        assert!(!rendered.iter().any(|o| o.starts_with("moveonenospc")));
        assert!(!rendered.iter().any(|o| o == "allow_other"));
        assert!(!rendered.iter().any(|o| o.starts_with("func.getattr")));
        assert_eq!(rendered.len(), 3, "the three valued options are always there");

        // And the values are checked, not passed through: an option string is
        // built into a mount command.
        let bad = MergerfsOptions {
            create_policy: "whatever".to_string(),
            ..MergerfsOptions::default()
        };
        assert!(mergerfs_options(&bad).is_err());
        let bad = MergerfsOptions {
            min_free_space: "20G,allow_other".to_string(),
            ..MergerfsOptions::default()
        };
        assert!(
            mergerfs_options(&bad).is_err(),
            "a size must not be able to smuggle a second option in"
        );
    }

    /// The spec refuses the shapes that would produce a plan destroying the
    /// wrong thing.
    #[test]
    fn a_spec_that_would_format_one_disk_twice_is_refused() {
        let mut s = spec();
        s.cache.push(Branch {
            disk: "sdg".to_string(),
            device: "/dev/sdg".to_string(),
        });
        let err = validate_spec(&s).expect_err("one disk in two roles");
        let CatalogError::InvalidArgument(detail) = err else {
            panic!("wrong error kind");
        };
        assert!(detail.contains("/dev/sdg"), "{detail}");

        let mut s = spec();
        s.parity.push(ParityDisk {
            index: 1,
            disk: "sdk".to_string(),
            device: "/dev/sdk".to_string(),
        });
        assert!(validate_spec(&s).is_err(), "two parity disks may not share an index");

        let mut s = spec();
        s.parity = (1..=3)
            .map(|i| ParityDisk {
                index: i,
                disk: format!("sd{i}"),
                device: format!("/dev/sdp{i}"),
            })
            .collect();
        assert!(validate_spec(&s).is_err(), "three parity disks are outside the model");

        let mut s = spec();
        s.data.clear();
        assert!(validate_spec(&s).is_err(), "an array needs a data disk");

        let mut s = spec();
        s.filesystem = "btrfs".to_string();
        assert!(validate_spec(&s).is_err());

        let mut s = spec();
        s.name = "../etc".to_string();
        assert!(validate_spec(&s).is_err(), "the name becomes three paths");
    }

    /// The paths of an array never collide with the fleet mounts of another
    /// node's shares, and a share can be told it is pointing at a branch.
    #[test]
    fn branch_paths_are_outside_the_fleet_mount_root() {
        let branch = data_branch_path("media", "sdg");
        assert!(is_branch_path(&branch));
        assert!(
            !branch.starts_with(crate::FLEET_MOUNT_ROOT),
            "{branch} would fight a fleet mount of a remote share"
        );
        // The union, by contrast, is an ordinary pool mountpoint — that is
        // what lets a share name it.
        let union = union_path("media");
        assert!(!is_branch_path(&union));
        assert!(crate::validate_share_path(&union).is_ok());
        // A share aimed at a branch is still a valid PATH, which is exactly
        // why the share layer needs `is_branch_path` to refuse it.
        assert!(crate::validate_share_path(&branch).is_ok());
    }

    /// A device link is accepted, a partition and a traversal are not.
    #[test]
    fn only_whole_disks_and_stable_links_may_be_formatted() {
        assert!(validate_branch_device("/dev/sdg").is_ok());
        assert!(validate_branch_device("/dev/nvme2n1").is_ok());
        assert!(validate_branch_device("/dev/disk/by-id/ata-ST8000NM_ZR18AB3F").is_ok());
        assert!(validate_branch_device("/dev/sdg1").is_err());
        assert!(validate_branch_device("/dev/disk/by-id/../../sda").is_err());
        assert!(validate_branch_device("/dev/disk/by-id/a b").is_err());
        assert!(validate_branch_device("sdg").is_err());
    }

    /// The renderer is the only renderer, and it redacts.
    #[test]
    fn a_secret_file_is_never_rendered() {
        let steps = vec![ElasticStep::WriteFile {
            path: "/etc/tentanas/secret".to_string(),
            content: "swordfish".to_string(),
            secret: true,
        }];
        let text = render(&steps);
        assert!(!text.contains("swordfish"), "{text}");
        assert!(text.contains("***"), "{text}");
        // A non-secret file is shown in full, because the snapraid config is
        // the document an admin has to be able to read before it is written.
        let steps = plan_create(&spec(), &tools()).expect("plan");
        let text = render(&steps);
        assert!(text.contains("    parity /mnt/tentanas-branches/media/parity/1/"), "{text}");
    }

    /// The create policies the wizard may offer are the ones the binary
    /// accepts.
    ///
    /// MEASURED (2026-09-06, mergerfs 2.42.0). The list is asserted as a SET
    /// with a floor, and the two corrections are named individually: `lus`
    /// was offered and is not a policy this build knows (the mount would have
    /// failed), `lfs` and `msplfs` are and were hidden.
    #[test]
    fn the_offered_create_policies_are_the_ones_mergerfs_accepts() {
        assert_eq!(CREATE_POLICIES.len(), 7, "{CREATE_POLICIES:?}");
        for policy in ["mfs", "epmfs", "ff", "lfs", "epff", "rand", "msplfs"] {
            assert!(
                CREATE_POLICIES.contains(&policy),
                "{policy} is accepted by mergerfs 2.42.0 and is not offered"
            );
            // …and each one really is usable, not merely listed.
            let opts = MergerfsOptions {
                create_policy: policy.to_string(),
                ..MergerfsOptions::default()
            };
            assert!(
                mergerfs_options(&opts)
                    .expect("policy")
                    .contains(&format!("category.create={policy}")),
                "{policy} is listed but the mount options drop it"
            );
        }
        assert!(
            !CREATE_POLICIES.contains(&"lus"),
            "mergerfs 2.42.0 does not know `lus`; offering it produces a mount that fails"
        );
        let bad = MergerfsOptions {
            create_policy: "lus".to_string(),
            ..MergerfsOptions::default()
        };
        assert!(mergerfs_options(&bad).is_err());
    }

    /// `snapraid diff` exits 2 when it finds differences, and that is a
    /// SUCCESS.
    ///
    /// MEASURED (2026-09-06, snapraid 14.7). Without this the job runner
    /// paints the one command that can answer "what is not in parity yet" as
    /// a permanent failure, and an admin learns to ignore it.
    #[test]
    fn a_diff_that_found_differences_is_not_a_failed_job() {
        assert_eq!(
            snapraid_outcome(&SnapraidAction::Diff, 2),
            SnapraidOutcome::Differences
        );
        assert_eq!(snapraid_outcome(&SnapraidAction::Diff, 0), SnapraidOutcome::Ok);
        // The exemption is for `diff` ALONE. A sync or a scrub that exits 2
        // has gone wrong, and reusing the code across actions would hide it.
        for action in [
            SnapraidAction::Sync,
            SnapraidAction::Scrub,
            SnapraidAction::Status,
        ] {
            assert_eq!(
                snapraid_outcome(&action, 2),
                SnapraidOutcome::Failed,
                "{action:?} exiting 2 is a failure"
            );
            assert_eq!(snapraid_outcome(&action, 0), SnapraidOutcome::Ok);
        }
        // A run killed by a signal reaches us as a negative code (the broker
        // has no exit status to report). A segfaulting snapraid build is a
        // real, measured thing, and it must never read as "differences".
        assert_eq!(
            snapraid_outcome(&SnapraidAction::Diff, -1),
            SnapraidOutcome::Failed
        );
        assert_eq!(
            snapraid_outcome(&SnapraidAction::Sync, -1),
            SnapraidOutcome::Failed
        );
    }

    /// mkfs labels fit XFS's 12 bytes AND are all different.
    ///
    /// The length half used to be the whole test, and it would have passed
    /// against a function returning one constant four times — which is very
    /// nearly what the function did: `tn-{array}-{role}-{disk}` truncated to
    /// 12 gave `tn-media-dat` for every data disk of `media`. Uniqueness is
    /// the property a label is FOR, and it is now the assertion.
    #[test]
    fn mkfs_labels_fit_xfs_and_are_unique_within_an_array() {
        for name in ["media", "a-very-long-array-name"] {
            let mut s = spec();
            s.name = name.to_string();
            // Two data disks whose names share a long prefix — the pair the
            // old truncation collapsed.
            s.data.push(Branch {
                disk: "sdaa".to_string(),
                device: "/dev/sdaa".to_string(),
            });
            let steps = plan_create(&s, &tools()).expect("plan");
            let labels: Vec<&str> = steps
                .iter()
                .filter_map(|st| match st {
                    ElasticStep::Mkfs { label, .. } => Some(label.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(labels.len(), 5, "three data, one cache, one parity");
            assert!(
                labels.iter().all(|l| l.len() <= 12),
                "XFS refuses a label over 12 bytes: {labels:?}"
            );
            let mut unique: Vec<&str> = labels.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                labels.len(),
                "two disks of one array share a label: {labels:?}"
            );
        }
    }
}
