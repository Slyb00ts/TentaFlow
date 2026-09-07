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
    pub parity: Vec<ElasticDiskSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElasticRole {
    Data(u16),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticResult {
    pub array_id: String,
    pub operation_id: String,
    pub owner: ElasticOwner,
    pub stage: ElasticStage,
    pub disks: Vec<ElasticDiskObservation>,
    pub union_mounted: Option<bool>,
    pub sync_completed_at: Option<String>,
    pub detail: Option<String>,
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
        if self.data.is_empty() || self.data.len() + self.parity.len() > 32 || self.parity.len() > 2 {
            return Err(invalid("Elastic wymaga danych, najwyżej 2 parity i 32 urządzeń łącznie"));
        }
        let disks: Vec<_> = self.data.iter().chain(&self.parity).collect();
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
    /// Keep moving until at least this much of the cache is free.
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
                    "move {} -> {} (older than {}s, until {}% free, {})\n",
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
            let percent = spec.snapraid.scrub_percent;
            if percent == 0 || percent > 100 {
                return Err(invalid(format!("scrub percent {percent} is outside 1..=100")));
            }
            args.push("scrub".to_string());
            args.push("-p".to_string());
            args.push(percent.to_string());
            args.push("-o".to_string());
            args.push(spec.snapraid.scrub_older_than_days.to_string());
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
    if rules.min_free_pct > 100 {
        return Err(invalid(format!(
            "minimum free space {}% is outside 0..=100",
            rules.min_free_pct
        )));
    }
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
    use std::sync::atomic::{AtomicU64, Ordering};

    const ROOT: &str = "/var/lib/tentanas/elastic";
    const JOURNAL_LIMIT: u64 = 128 * 1024;
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Pending {
        Format(ElasticRole),
        Mount(ElasticRole),
        Config,
        Union,
        Sync,
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
            let journal: Journal =
                serde_json::from_slice(&bytes).map_err(|e| format!("journal: {e}"))?;
            journal.spec.validate().map_err(|e| e.to_string())?;
            validate_elastic_uuid(&journal.boot_id).map_err(|e| e.to_string())?;
            if journal.schema != 1
                || journal.spec.array_id != id
                || journal.formatted.iter().enumerate().any(|(i, role)| {
                    role_disk(&journal.spec, *role).is_err()
                        || journal.formatted[..i].contains(role)
                })
            {
                return Err("niespójny journal Elastic".into());
            }
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
            let target = self.path.join(format!("{}.json", journal.spec.array_id));
            if target.try_exists().map_err(|e| e.to_string())? {
                let old = self.load(&journal.spec.array_id)?;
                if old.spec != journal.spec {
                    return Err("zmiana trwałej specyfikacji Elastic".into());
                }
            }
            let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
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
                schema: 1,
                spec: spec.clone(),
                stage: ElasticStage::Prepared,
                formatted: Vec::new(),
                pending: None,
                boot_id,
                sync_completed_at: None,
                detail: None,
            };
            self.save(&journal)?;
            Ok(journal)
        }
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
        let candidate = parent.join(format!(".elastic-{}-{sequence}.new", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&candidate)
            .map_err(|e| e.to_string())?;
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
            ElasticRole::Parity(i) => spec.parity.get(usize::from(i).wrapping_sub(1)),
        }
        .ok_or_else(|| "nieznana rola journala".into())
    }

    fn roles(spec: &ElasticCreateSpec) -> Vec<ElasticRole> {
        (1..=spec.data.len())
            .map(|i| ElasticRole::Data(i as u16))
            .chain((1..=spec.parity.len()).map(|i| ElasticRole::Parity(i as u8)))
            .collect()
    }

    fn mount_path(spec: &ElasticCreateSpec, role: ElasticRole) -> String {
        match role {
            ElasticRole::Data(i) => data_branch_path(&spec.name, &format!("d{i}")),
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
    struct MountRow {
        path: String,
        major_minor: String,
        filesystem: String,
    }

    fn mount_rows() -> Result<Vec<MountRow>, String> {
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
                    path,
                    major_minor: left[2].into(),
                    filesystem: right[0].into(),
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

    fn vacant_namespace(spec: &ElasticCreateSpec) -> Result<(), String> {
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
                && [union_path(&spec.name), branch_root(&spec.name)]
                    .iter()
                    .any(|p| overlaps(Path::new(p), Path::new(&m.path)))
        }) {
            return Err("obcy mount zasłania przestrzeń macierzy".into());
        }
        Ok(())
    }

    fn layout(spec: &ElasticCreateSpec, devices: &[Device]) -> Result<ElasticSpec, String> {
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
                device: resolve(disk, devices)?.path.clone(),
            });
        }
        for (i, disk) in spec.parity.iter().enumerate() {
            value.parity.push(ParityDisk {
                index: (i + 1) as u8,
                disk: format!("p{}", i + 1),
                device: resolve(disk, devices)?.path.clone(),
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

    fn validate_union_options(
        spec: &ElasticSpec,
        values: &BTreeMap<String, String>,
    ) -> Result<(), String> {
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
        let bytes = digits
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(multiplier))
            .ok_or("niepoprawny minfreespace")?;
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

    fn observe(journal: &Journal) -> ElasticResult {
        let devices = inventory();
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
            .and_then(|s| union_mounted(&s).ok());
        ElasticResult {
            array_id: journal.spec.array_id.clone(),
            operation_id: journal.spec.operation_id.clone(),
            owner: journal.spec.owner.clone(),
            stage: journal.stage,
            disks,
            union_mounted: union,
            sync_completed_at: journal.sync_completed_at.clone(),
            detail: journal.detail.clone(),
        }
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

    fn plan_role(plan: &ElasticSpec, device: &str) -> Result<ElasticRole, String> {
        if let Some(index) = plan.data.iter().position(|d| d.device == device) {
            return Ok(ElasticRole::Data((index + 1) as u16));
        }
        plan.parity
            .iter()
            .find(|d| d.device == device)
            .map(|d| ElasticRole::Parity(d.index))
            .ok_or_else(|| "urządzenie planu poza specyfikacją".into())
    }

    fn execute_steps(
        root: &Root,
        array_lock: &File,
        journal: &mut Journal,
        plan: &ElasticSpec,
        steps: Vec<ElasticStep>,
        allow_format: bool,
    ) -> Result<(), String> {
        let locks = [root.node_lock.as_raw_fd(), array_lock.as_raw_fd()];
        for step in steps {
            match step {
                ElasticStep::Note(_) => (),
                ElasticStep::Mkdir { path } => directory(Path::new(&path), false, root.uid)?,
                ElasticStep::Mkfs {
                    program,
                    device,
                    filesystem,
                    label,
                } => {
                    if !allow_format {
                        return Err("restore nie może formatować".into());
                    }
                    let devices = inventory()?;
                    let role = plan_role(plan, &device)?;
                    if journal.formatted.contains(&role) || journal.pending.is_some() {
                        return Err("formatowanie już rozpoczęte".into());
                    }
                    let disk = role_disk(&journal.spec, role)?;
                    let current = resolve(disk, &devices)?;
                    clean_device(current)?;
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
                        || success(run(Path::new(&program), &args, None, &locks)?).map(|_| ()),
                    )?;
                    let refreshed = inventory()?;
                    filesystem_matches(
                        &journal.spec,
                        role,
                        resolve(role_disk(&journal.spec, role)?, &refreshed)?,
                    )?;
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
                    let devices = inventory()?;
                    let role = plan_role(plan, &source)?;
                    if mountpoint != mount_path(&journal.spec, role) {
                        return Err("mountpoint niezgodny z rolą".into());
                    }
                    let device = resolve(role_disk(&journal.spec, role)?, &devices)?;
                    filesystem_matches(&journal.spec, role, device)?;
                    if branch_mounted(&journal.spec, role, device)? {
                        continue;
                    }
                    if std::fs::read_dir(&mountpoint)
                        .map_err(|e| e.to_string())?
                        .next()
                        .is_some()
                    {
                        return Err("katalog brancha nie jest pusty".into());
                    }
                    pending_operation(
                        journal,
                        Pending::Mount(role),
                        ElasticStage::Mounting,
                        |journal| root.save(journal),
                        || {
                            success(run(
                                &tool(&["/usr/bin/mount", "/bin/mount"])?,
                                &[
                                    "-t".into(),
                                    filesystem,
                                    "-o".into(),
                                    options.join(","),
                                    device.path.clone(),
                                    mountpoint,
                                ],
                                None,
                                &locks,
                            )?)
                            .map(|_| ())
                        },
                    )?;
                    if !branch_mounted(&journal.spec, role, device)? {
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
                        || atomic_write(Path::new(&path), content.as_bytes(), root.uid),
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
                    let devices = inventory()?;
                    for role in roles(&journal.spec) {
                        let device = resolve(role_disk(&journal.spec, role)?, &devices)?;
                        filesystem_matches(&journal.spec, role, device)?;
                        if !branch_mounted(&journal.spec, role, device)? {
                            return Err("brak potwierdzonego brancha".into());
                        }
                    }
                    let spec = layout(&journal.spec, &devices)?;
                    if union_mounted(&spec)? {
                        continue;
                    }
                    if std::fs::read_dir(&mountpoint)
                        .map_err(|e| e.to_string())?
                        .next()
                        .is_some()
                    {
                        return Err("katalog unii nie jest pusty".into());
                    }
                    // Demon FUSE nie dziedziczy blokad krótkich operacji dyskowych.
                    pending_operation(
                        journal,
                        Pending::Union,
                        ElasticStage::Mounting,
                        |journal| root.save(journal),
                        || {
                            success(run(
                                Path::new(&program),
                                &[
                                    "-o".into(),
                                    options.join(","),
                                    branches.join(":"),
                                    mountpoint,
                                ],
                                None,
                                &[],
                            )?)
                            .map(|_| ())
                        },
                    )?;
                    if !union_mounted(&spec)? {
                        return Err("brak potwierdzonej unii".into());
                    }
                    journal.pending = None;
                    root.save(journal)?;
                }
                ElasticStep::Run { program, args } => {
                    if !allow_format || journal.spec.parity.is_empty() {
                        return Err("nieoczekiwany sync".into());
                    }
                    pending_operation(
                        journal,
                        Pending::Sync,
                        ElasticStage::SyncPending,
                        |journal| root.save(journal),
                        || success(run(Path::new(&program), &args, None, &locks)?).map(|_| ()),
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

    fn finish(
        root: &Root,
        journal: &mut Journal,
        outcome: Result<(), String>,
    ) -> Result<ElasticResult, String> {
        match outcome {
            Ok(()) => {
                journal.stage = ElasticStage::Ready;
                journal.detail = None;
            }
            Err(error) => {
                journal.stage = ElasticStage::NeedsAttention;
                journal.detail = Some(error);
            }
        }
        root.save(journal)?;
        Ok(observe(journal))
    }

    fn create(root: &Root, spec: &ElasticCreateSpec) -> Result<ElasticResult, String> {
        let array_lock = root.array_lock(&spec.array_id)?;
        let devices = inventory()?;
        let layout = layout(spec, &devices)?;
        let tools = Tools::resolve(&layout).map_err(|e| e.to_string())?;
        let steps = plan_create(&layout, &tools).map_err(|e| e.to_string())?;
        let mut journal = root.reserve(spec, boot_id()?, || {
            vacant_namespace(spec)?;
            for disk in spec.data.iter().chain(&spec.parity) {
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
        let outcome = execute_steps(root, &array_lock, &mut journal, &layout, steps, true);
        finish(root, &mut journal, outcome)
    }

    fn restore(root: &Root, mut journal: Journal) -> Result<ElasticResult, String> {
        let array_lock = root.array_lock(&journal.spec.array_id)?;
        let outcome = (|| -> Result<(), String> {
            let current_boot = boot_id()?;
            if matches!(journal.pending, Some(Pending::Sync))
                || (!journal.spec.parity.is_empty() && journal.sync_completed_at.is_none())
            {
                return Err("sync niepotwierdzony; restore nie wykonuje sync".into());
            }
            let devices = inventory()?;
            let spec = layout(&journal.spec, &devices)?;
            for role in roles(&journal.spec) {
                filesystem_matches(
                    &journal.spec,
                    role,
                    resolve(role_disk(&journal.spec, role)?, &devices)?,
                )?;
            }
            if matches!(journal.pending, Some(Pending::Union))
                && current_boot == journal.boot_id
                && !union_mounted(&spec)?
            {
                return Err("niepewne zakończenie mount w tym samym boot; wymagany restart".into());
            }
            if journal.formatted.len() != roles(&journal.spec).len() {
                return Err("nieukończone formatowanie; restore nie formatuje".into());
            }
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
                false,
            )
        })();
        finish(root, &mut journal, outcome)
    }

    pub(crate) fn execute(command: &crate::HelperCommand) -> Result<String, String> {
        if unsafe { libc::geteuid() } != 0 {
            return Err("wykonawca Elastic wymaga root".into());
        }
        let root = Root::open(Path::new(ROOT), 0)?;
        let value = match command {
            crate::HelperCommand::ElasticCreate { operation } => {
                serde_json::to_value(create(&root, operation)?)
            }
            crate::HelperCommand::ElasticInspect { array_id, owner }
            | crate::HelperCommand::ElasticRestore { array_id, owner } => {
                let journal = root.load(array_id)?;
                if &journal.spec.owner != owner {
                    return Err("macierz niedostępna dla właściciela".into());
                }
                let result = if matches!(command, crate::HelperCommand::ElasticRestore { .. }) {
                    restore(&root, journal)?
                } else {
                    observe(&journal)
                };
                serde_json::to_value(result)
            }
            crate::HelperCommand::ElasticClaims { name } => {
                let journals = root.journals()?;
                let disks = journals
                    .iter()
                    .flat_map(|j| j.spec.data.iter().chain(&j.spec.parity))
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
            for disk in spec.data.iter().chain(&spec.parity) {
                if journal
                    .spec
                    .data
                    .iter()
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
            .flat_map(|j| j.spec.data.iter().chain(&j.spec.parity))
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
    mod tests {
        use super::*;
        use std::os::fd::FromRawFd;
        use std::os::unix::fs::{symlink, PermissionsExt};
        use std::os::unix::process::ExitStatusExt;

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
                parity: Vec::new(),
            }
        }
        fn boot() -> String {
            "44444444-4444-4444-8444-444444444444".into()
        }

        #[test]
        fn journal_reservation_survives_reopen_and_refuses_repeated_create() {
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
            let dir = Temp::new();
            let uid = unsafe { libc::geteuid() };
            let root = Root::open(&dir.0, uid).expect("first");
            assert!(Root::open(&dir.0, uid).is_err());
            drop(root);
            assert!(Root::open(&dir.0, uid).is_ok());
        }

        #[test]
        fn child_keeps_mutation_lock_after_parent_closes_but_daemon_does_not() {
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
                false,
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
        assert!(text.contains("until 20% free"), "{text}");
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

    /// `snapraid scrub` carries the percentage and the age from the array's
    /// settings, and `fix` may only name a disk of THIS array.
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
                "scrub".to_string(),
                "-p".to_string(),
                "8".to_string(),
                "-o".to_string(),
                "10".to_string(),
            ]
        );
        // A different setting produces a different argv — so the assertion
        // above is reading the settings and not a constant.
        s.snapraid.scrub_percent = 25;
        assert!(snapraid_args(&s, &SnapraidAction::Scrub)
            .unwrap()
            .contains(&"25".to_string()));
        s.snapraid.scrub_percent = 0;
        assert!(snapraid_args(&s, &SnapraidAction::Scrub).is_err());

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
