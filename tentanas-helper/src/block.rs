// =============================================================================
// File: tentanas-helper/src/block.rs — block targets (plan-02 §5.5): the
// desired state of one iSCSI target or one NVMe-oF subsystem, and the ordered
// list of configfs operations that makes the kernel serve it.
//
// WHY configfs and not targetcli/nvmetcli:
//   * `targetcli` is an interactive shell with a `saveconfig`/`restoreconfig`
//     pair. §3.4 fixes ONE source of truth — `tentanas.db` — so a tool whose
//     main job is writing a second one is the wrong tool, and putting it in
//     the catalog would hand the channel a command that reads an arbitrary
//     JSON file as root.
//   * The kernel interface those tools drive is `/sys/kernel/config`, and it
//     is a plain directory tree: `mkdir` creates an object, a write sets an
//     attribute, a symlink links two objects. Rendering that tree is a pure
//     function of the desired state, which is why the plan below can be
//     asserted byte for byte in a test on a host with no LIO at all.
//
// The plan is rendered on BOTH sides: core renders it to show and to log
// (`render`, which never prints a secret), the wrapper renders it to run it.
// One function, so the preview and the action cannot disagree.
//
// TRAP — nothing here binds to an interface NAME. LIO's network portal is
// `<address>:<port>` and nvmet's is `addr_traddr`; neither kernel subsystem
// knows what a netdev is called. "Portal bound to storage0" therefore means
// "bound to the address storage0 currently has", and an address change breaks
// the portal rather than following the interface. The wizard resolves the
// name; this file only ever sees the address.
// =============================================================================

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{invalid, CatalogError};

/// The LIO (SCSI target) configfs root and the nvmet one. Both are subtrees of
/// the single configfs mount; the app never touches anything else under it.
pub const TARGET_CONFIGFS: &str = "/sys/kernel/config/target";
pub const NVMET_CONFIGFS: &str = "/sys/kernel/config/nvmet";

/// The IANA ports of the two protocols. Both are configurable per portal in
/// the model; these are what the wizard offers.
pub const ISCSI_PORT: u32 = 3260;
pub const NVME_PORT: u32 = 4420;

/// The single HBA index the app uses under `target/core/`. LIO numbers HBAs
/// per plugin and the number carries no meaning — one is enough, and a fixed
/// one keeps the plan a pure function of the desired state.
const IBLOCK_HBA: &str = "iblock_0";

/// LIO's ALUA access type: 1 = implicit only.
///
/// WHY not 3 (implicit + explicit): explicit ALUA lets an INITIATOR write the
/// port group state with SET TARGET PORT GROUPS. The state would then no
/// longer come from `tentanas.db`, and the next apply would silently take it
/// back — exactly the two-sources-of-truth drift §3.4 forbids.
const ALUA_ACCESS_TYPE_IMPLICIT: &str = "1";

/// What the SAME attribute prints when it holds that value.
///
/// MEASURED (run 5), and the one attribute in this plan whose write shape and
/// show shape differ: `alua_access_type` takes the number `1` and prints the
/// word `Implicit`. Every other attribute the diff compares — `param/AuthMethod`,
/// all four `attrib/*`, `tg_pt_gp_id`, `alua_access_state`, `np/<portal>/iser`
/// — round-trips byte for byte, which is why they go through the plain
/// comparison.
///
/// Comparing the written form against the printed one never matched, so this
/// attribute was rewritten on every apply of every LUN — harmless (the write
/// is accepted) but exactly the "guessed read shape" class of bug that
/// `serial_holds` exists for, and the test fixture pinned a reading no real
/// LIO produces.
///
/// EXACT equality, never `contains`: a group that reads `Implicit and
/// Explicit` holds something this app must take back to implicit-only, since
/// explicit ALUA would let an initiator set the state and make the kernel a
/// second source of truth.
const ALUA_ACCESS_TYPE_SHOWN: &str = "Implicit";

/// `attr_model` — what `nvme list` prints in the Model column. A constant
/// because the plan compares it against what the kernel already holds:
/// `nvmet_subsys_attr_model_store` refuses a change once a controller has
/// connected.
const NVMET_MODEL: &str = "TentaNas";

// =============================================================================
// Desired state
// =============================================================================

/// One port group of the ALUA (iSCSI) / ANA (NVMe-oF) model — carried from the
/// first version (research R8) so adding a second path later is a new row, not
/// a reshaped table.
///
/// The four states are the ones BOTH protocols have. SCSI's Standby and
/// LBA-dependent have no ANA equivalent, and a state that means one thing over
/// iSCSI and another over NVMe-oF would be worse than one that does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BlockPortGroup {
    /// 1..=65535. Group 1 is the one every target starts with.
    pub group_id: u32,
    /// 'optimized' | 'non-optimized' | 'unavailable' | 'transitioning'
    pub state: String,
    /// LIO's `preferred` bit on the target port group. NVMe ANA has no such
    /// flag, so `plan_nvmet` REFUSES a group that sets it rather than dropping
    /// it silently.
    #[serde(default)]
    pub preferred: bool,
}

impl BlockPortGroup {
    /// LIO's numeric `alua_access_state`.
    fn lio_state(&self) -> Result<&'static str, CatalogError> {
        Ok(match self.state.as_str() {
            "optimized" => "0",
            "non-optimized" => "1",
            "unavailable" => "3",
            "transitioning" => "15",
            other => return Err(invalid(format!("unknown port group state '{other}'"))),
        })
    }

    /// nvmet's `ana_state` string.
    fn ana_state(&self) -> Result<&'static str, CatalogError> {
        Ok(match self.state.as_str() {
            "optimized" => "optimized",
            "non-optimized" => "non-optimized",
            "unavailable" => "inaccessible",
            "transitioning" => "change",
            other => return Err(invalid(format!("unknown port group state '{other}'"))),
        })
    }

    /// The LIO target port group directory name of this group.
    fn lio_name(&self) -> String {
        format!("tentanas_gp{}", self.group_id)
    }
}

/// One LUN (iSCSI) / namespace (NVMe-oF): the backing device and the port
/// group it answers for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BlockLun {
    /// LUN number / NSID. Both start at 0 for iSCSI and at 1 for NVMe.
    pub index: u32,
    /// The backstore object's name under `target/core/<hba>/`. Ignored by
    /// nvmet, which addresses the device by path.
    pub name: String,
    /// `/dev/zvol/<pool>/<volume>` for a zvol, an absolute file path for a
    /// file-backed LUN.
    pub device_path: String,
    /// The stable identity two nodes must agree on for multipath to see ONE
    /// device with two paths: LIO's `vpd_unit_serial`, nvmet's `device_uuid`.
    pub uuid: String,
    pub group_id: u32,
}

/// A network portal (iSCSI) / port (NVMe-oF).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BlockPortal {
    /// A literal IPv4 address, or `0.0.0.0` for every interface.
    pub address: String,
    pub port: u32,
    /// iSCSI: RDMA is a flag ON the portal (`iser`), because iSER and iSCSI
    /// share one TCP portal for discovery. NVMe-oF: the transport IS the port,
    /// so 'tcp' and 'rdma' are two ports.
    #[serde(default)]
    pub transport: String,
}

/// iSCSI CHAP. `mutual` adds the target's own credentials, which is the only
/// thing that stops an initiator talking to an impostor target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IscsiAuth {
    pub enabled: bool,
    #[serde(default)]
    pub mutual: bool,
    #[serde(default)]
    pub userid: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub mutual_userid: String,
    #[serde(default)]
    pub mutual_password: String,
}

/// One allowed NVMe host. DH-HMAC-CHAP keys live on the HOST object in nvmet,
/// never on the subsystem — see `plan_nvmet` for what that implies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NvmetHost {
    pub nqn: String,
    /// `DHHC-1:…` — the host's key. Empty = this host authenticates with
    /// nothing, which is the NQN allowlist on its own.
    #[serde(default)]
    pub dhchap_key: String,
    /// The controller's key for bidirectional authentication.
    #[serde(default)]
    pub dhchap_ctrl_key: String,
    #[serde(default)]
    pub dhchap_hash: String,
    #[serde(default)]
    pub dhchap_dhgroup: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IscsiTargetSpec {
    pub iqn: String,
    pub luns: Vec<BlockLun>,
    pub portals: Vec<BlockPortal>,
    pub port_groups: Vec<BlockPortGroup>,
    pub auth: IscsiAuth,
    /// The initiator IQNs that may log in. EMPTY means "generate an ACL for
    /// whoever connects" — see `plan_iscsi` for why that is not the same as
    /// "no security" when CHAP is on.
    #[serde(default)]
    pub initiators: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NvmetSubsystemSpec {
    pub nqn: String,
    /// `attr_serial` — shown by `nvme list` next to the namespace.
    pub serial: String,
    pub namespaces: Vec<BlockLun>,
    pub portals: Vec<BlockPortal>,
    pub port_groups: Vec<BlockPortGroup>,
    #[serde(default)]
    pub hosts: Vec<NvmetHost>,
    /// `attr_allow_any_host`. True skips the allowlist AND every host key with
    /// it, because nvmet reads the keys off the host objects the allowlist is
    /// made of.
    #[serde(default)]
    pub allow_any_host: bool,
}

// =============================================================================
// The plan
// =============================================================================

/// One configfs operation. Everything the two subsystems need is expressible
/// with four verbs, which is what makes the plan renderable and diffable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigfsStep {
    /// Create an object. Already existing is success — the apply is a
    /// reconcile, not a first-time installer.
    Mkdir(String),
    Write {
        path: String,
        value: String,
        /// A CHAP / DH-HMAC-CHAP value. `render` prints `***` for it, always:
        /// there is no rendering mode that prints the real one. Tightening the
        /// attribute's mode is NOT part of this step — see `Protect`.
        secret: bool,
    },
    /// Takes a credential attribute's mode down to 0600 and CHECKS that it
    /// took.
    ///
    /// Its own verb, and unconditional, because of what was measured. nvmet
    /// creates `hosts/<nqn>/dhchap_key` as `-rw-r--r-- root:root` and an
    /// unprivileged local account really can read the key out of it (measured
    /// by reading it, not by looking at the mode). `chmod 600` on a configfs
    /// attribute takes, and the same account can then no longer read it — so
    /// this is a real protection and not a no-op.
    ///
    /// The two measurements that decide the SHAPE of the step:
    ///   * the mode SURVIVES the next write to the attribute (still 600, still
    ///     unreadable), so protecting once per object is enough and an apply
    ///     that rewrites a key does not reopen the hole;
    ///   * it does NOT survive the object being recreated — a fresh `mkdir`
    ///     makes a fresh attribute at 0644.
    ///
    /// Together those mean the chmod belongs to "this object holds a secret",
    /// never to "this secret changed". The plan below skips writes whose value
    /// the kernel already holds, and if the chmod rode along on the write, a
    /// host object created before this step existed would keep its 0644
    /// forever — nothing would ever rewrite the key. So it is emitted for
    /// every secret-bearing attribute the plan names, whether or not the value
    /// is being written. There is a test pinning exactly that.
    Protect(String),
    /// Empties a credential attribute.
    ///
    /// TRAP, and the reason this is its own verb instead of `Write` with an
    /// empty string: a zero-length write NEVER reaches a configfs store
    /// method. `configfs_write_iter` calls `flush_write_buffer` only for
    /// `len > 0`, and `O_TRUNC` goes to `simple_setattr`, which no attribute
    /// sees. MEASURED: writing `""` to `{tpg}/auth/password` SUCCEEDS and the
    /// old secret reads back unchanged, while the literal `NULL` is stored and
    /// clears the flag. An "empty" write is the exact opposite of what turning
    /// authentication off must do.
    ///
    /// LIO has a value that means "nothing": `__iscsi_*_store` compares the
    /// buffer against the literal `NULL` and only then clears
    /// `NAF_USERID_SET`/`NAF_PASSWORD_SET` — MEASURED in obs. 7. The sentinel
    /// belongs in the PLAN, where it is visible and testable, not in a
    /// heuristic over the path.
    ///
    /// nvmet has NO such value, which is why this verb is LIO's alone.
    /// MEASURED (obs. 37) on `hosts/<nqn>/dhchap_key`: `"\n"`, `"NULL"`,
    /// `"0"` and `" "` are each EINVAL, and `""` is accepted while changing
    /// nothing. A key stops existing there by the OBJECT ceasing to exist.
    Clear { path: String, sentinel: String },
    /// `link` -> `target`, both absolute.
    Symlink { link: String, target: String },
    /// A line for the job log and nothing else — no syscall, no failure.
    ///
    /// It exists so a plan can say why it did NOT do something. The one case
    /// today is a shared nvmet host object whose key this subsystem would have
    /// cleared: silence there reads as "nothing to do", when what happened is
    /// "another target still depends on this and I left it alone".
    Note(String),
    /// Removes a configfs symlink — a mapped LUN's backing link, or an
    /// `allowed_hosts/<nqn>` entry.
    ///
    /// It is half of what makes `apply` a reconcile instead of an installer:
    /// taking an initiator off the allowlist has to REACH the kernel, and a
    /// plan that only ever creates leaves the old grant in place while
    /// reporting success.
    Unlink(String),
    /// Removes a configfs object. Ordered by the caller, because configfs
    /// refuses to remove a group that still has children (`-ENOTEMPTY`) or one
    /// something else still links to (`-EBUSY`).
    Rmdir(String),
}

/// The kernel modules a block protocol needs, in load order — EMPTY for
/// anything that is not a block protocol.
///
/// THE list, and there is exactly one of it on purpose. It used to exist twice
/// — here, where `modprobe` runs, and in the core, where the verdict decides
/// whether a node "can serve" the protocol at all. Nothing pinned the two
/// together, and a divergence would mean the core answering "yes, the modules
/// are in this kernel's tree" about one set while the helper loads another:
/// the target would be judged appliable, the apply would load the wrong
/// module, and no target would come back after a reboot. That is round 3's
/// BLK-01 arriving through a different door, so the duplicate was removed
/// rather than tested.
///
/// `iscsi_target_mod` pulls `target_core_mod` in through its dependencies, but
/// naming both keeps the failure message honest about which half a kernel is
/// missing — and lets the unprivileged probe check for both.
pub fn modules_for(protocol: &str) -> &'static [&'static str] {
    match protocol {
        "iscsi" => &["target_core_mod", "iscsi_target_mod"],
        "nvmet" => &["nvmet", "nvmet-tcp"],
        _ => &[],
    }
}

/// LIO's "this credential is unset" literal.
///
/// MEASURED (obs. 6, 7 and run 5), not read out of `drivers/target/iscsi`:
/// writing `""` succeeds and changes nothing, writing `NULL` stores the
/// sentinel, and a credential cleared that way READS BACK the literal `NULL`.
///
/// Two shapes mean "unset" and they are not the same reading: a credential
/// nobody ever wrote reads as the EMPTY string, one that was cleared reads as
/// `NULL`. A diff could therefore skip a redundant `Clear` on an
/// already-cleared attribute — it is not done, and `auth_steps` says why.
pub const LIO_CLEAR: &str = "NULL";
// `NVMET_CLEAR` used to live here — a sentinel for "no key" on an nvmet host.
//
// It is gone because there is no such value. MEASURED (obs. 37) on a host
// object, linked and unlinked alike: `"\n"`, `"NULL"`, `"0"` and `" "` are all
// EINVAL, and `""` is accepted and changes nothing — the same silent no-op
// LIO's empty write turned out to be in obs. 6. Round 1 asserted that nvmet
// clears on an empty value; nobody had measured it, and it was false.
//
// A constant that cannot do its job is worse than no constant, because every
// use of it is a step the kernel refuses — and `Clear` is fatal, so each one
// takes its whole plan down. "This host must stop requiring a key" is spelled
// `rmdir hosts/<nqn>` instead: obs. 24 showed a recreated object gets fresh
// attributes, and `remove_nvmet` was already removing unreferenced hosts that
// way.
//
// `LIO_CLEAR` stays: obs. 7 measured that one working.

impl ConfigfsStep {
    fn write(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Write {
            path: path.into(),
            value: value.into(),
            secret: false,
        }
    }

    fn secret(path: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Write {
            path: path.into(),
            value: value.into(),
            secret: true,
        }
    }

    fn clear(path: impl Into<String>, sentinel: &str) -> Self {
        Self::Clear {
            path: path.into(),
            sentinel: sentinel.to_string(),
        }
    }

    /// The path this step acts on — what an error message names. `Note` acts
    /// on nothing and returns `""`: it used to return the whole note, so the
    /// first error message to use this would have printed a sentence of prose
    /// where a path belongs.
    pub fn path(&self) -> &str {
        match self {
            Self::Mkdir(p) => p,
            Self::Write { path, .. } => path,
            Self::Clear { path, .. } => path,
            Self::Protect(p) => p,
            Self::Note(_) => "",
            Self::Symlink { link, .. } => link,
            Self::Unlink(p) => p,
            Self::Rmdir(p) => p,
        }
    }

    /// Whether this step performs a syscall.
    ///
    /// `Note` does not, and the job log's "(N configfs steps)" is a count the
    /// admin reads as "what the node did in the kernel" — so notes are not in
    /// it. With a shared host object that is one or two steps of difference in
    /// exactly the situation where the number is being checked.
    pub fn touches_kernel(&self) -> bool {
        !matches!(self, Self::Note(_))
    }
}

/// How many steps of a plan actually reach the kernel — see `touches_kernel`.
pub fn kernel_step_count(steps: &[ConfigfsStep]) -> usize {
    steps.iter().filter(|s| s.touches_kernel()).count()
}

/// The plan as text: one line per step, secrets replaced by `***`.
///
/// This is what the job log records, what the target detail shows and what the
/// tests assert. It is the only rendering there is, so a secret cannot reach a
/// log through a caller that forgot to redact.
pub fn render(steps: &[ConfigfsStep]) -> String {
    let mut out = String::new();
    for step in steps {
        match step {
            ConfigfsStep::Mkdir(path) => out.push_str(&format!("mkdir {path}\n")),
            ConfigfsStep::Write {
                path,
                value,
                secret: true,
            } => {
                let _ = value;
                out.push_str(&format!("write {path} = ***\n"));
            }
            ConfigfsStep::Write { path, value, .. } => {
                out.push_str(&format!("write {path} = {value}\n"))
            }
            // The sentinel is shown, not hidden: "clear = NULL" is what an
            // admin has to be able to recognize in the job log when a
            // credential goes away.
            ConfigfsStep::Clear { path, sentinel } => {
                out.push_str(&format!("clear {path} = {}\n", sentinel.trim_end_matches('\n')))
            }
            // Loud in the log on purpose: this line is the evidence that the
            // key stopped being world-readable, and its absence is the
            // evidence that it did not.
            ConfigfsStep::Protect(path) => out.push_str(&format!("protect {path} = 0600\n")),
            ConfigfsStep::Note(text) => out.push_str(&format!("note: {text}\n")),
            ConfigfsStep::Symlink { link, target } => {
                out.push_str(&format!("link {link} -> {target}\n"))
            }
            // The removals are the loudest lines in the log on purpose: this
            // is where an admin sees that an initiator lost its access or that
            // a portal stopped listening.
            ConfigfsStep::Unlink(path) => out.push_str(&format!("unlink {path}\n")),
            ConfigfsStep::Rmdir(path) => out.push_str(&format!("rmdir {path}\n")),
        }
    }
    out
}

// =============================================================================
// Observation — what the kernel already holds
//
// WHY this exists at all: `mkdir` and `symlink` are idempotent, a configfs
// WRITE is not, and `apply` is a reconcile that re-renders every target on
// every mutation — so the SECOND run is the normal case, not the edge case.
// Every write below is emitted only when the kernel does not already hold the
// wanted value, because each of these refuses an in-place change.
//
// MEASURED on a live LIO + nvmet node, with the kernel's own words:
//   * `{dev}/control` = `udev_path=…` a second time — EEXIST, "Unable to set
//     udev_path= while ib_dev->ibd_bd exists". (`{dev}/control` with
//     `udev_path=` IS the right way to point an iblock backstore at a device;
//     the plain `{dev}/udev_path` attribute sets target-core's copy only, and
//     `enable` then fails.)
//   * `{dev}/wwn/vpd_unit_serial` while the LUN is exported — EINVAL, "Unable
//     to set VPD Unit Serial while active 1 $FABRIC_MOD exports exist". There
//     is no `export_count` attribute to read, so what the observation looks at
//     is the device itself.
//   * `{dev}/enable` = 1 a second time — EEXIST.
//   * `{gp}/tg_pt_gp_id` a second time — EINVAL, "ALUA TG PT Group already has
//     a valid ID, ignoring request".
//   * `{sub}/attr_allow_any_host` = 1 while a host is linked — EINVAL, "Can't
//     set allow_any_host when explicit hosts are set!"; unlinking first makes
//     the same write succeed.
//   * `{port}/addr_traddr` on an enabled port — EACCES, "Disable port '247'
//     before changing attribute in nvmet_addr_traddr_store".
//   * `{ns}/device_path` on an enabled namespace — EBUSY.
//
//   * `{sub}/attr_model` and `{sub}/attr_serial` after a controller has
//     connected — EINVAL, "Can't set model number. Linux is already assigned"
//     (obs. 16, 17). The source calls that `subsys->subsys_discovered`; the
//     kernel said it out loud.
//
// The second half of the observation is the list of objects that EXIST but the
// spec no longer names. Without it, taking an initiator off the allowlist or
// re-picking a portal writes a row, reports success, and leaves the ACL (with
// its CHAP credentials) and the old `np/<address>:3260` serving in the kernel.
//
// Reading configfs needs no privilege (the tree is 0755/0644), so the core
// renders its preview from the same observation the wrapper applies from. The
// one thing this canNOT read is LIO's `auth/*`: those show methods are gated
// behind `capable(CAP_SYS_ADMIN)` (measured: EPERM from an ordinary account,
// while `param/*` on the same TPG reads fine), so the credentials are always
// rewritten —
// which is harmless, their store methods have no guard at all.
// =============================================================================

/// One backstore of an iSCSI target as the kernel currently holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IscsiDeviceObserved {
    /// The backstore object name, i.e. `BlockLun::name`.
    pub name: String,
    /// `{dev}/enable` reads 1. That is the same condition as "the iblock
    /// plugin has opened the block device", which is what makes `control` and
    /// a second `enable` fail.
    pub configured: bool,
    /// `{dev}/wwn/vpd_unit_serial` VERBATIM — label and all, exactly as the
    /// show method printed it, or empty when it could not be read.
    ///
    /// NOT parsed here, and that is deliberate. The label
    /// (`T10 VPD Unit Serial Number: <value>`) is really there — MEASURED in
    /// run 5, where a device with no serial written reads exactly
    /// `T10 VPD Unit Serial Number: `. So an equality check against the uuid
    /// would never match, and a parse of that label would break the moment the
    /// kernel changed it: an empty parse result never equals the uuid either,
    /// and the write it would then emit is EINVAL on an exported LUN.
    /// `serial_holds` compares by suffix for exactly that reason.
    pub unit_serial: String,
    /// Whether a LUN of THIS TPG currently links to this backstore.
    ///
    /// MEASURED: `{dev}/wwn/vpd_unit_serial` is EINVAL — "Unable to set VPD
    /// Unit Serial while active 1 $FABRIC_MOD exports exist" — from the moment
    /// a LUN links to the backstore, and there is no `export_count` attribute
    /// to read. The link is the observable form of that count, so this is what
    /// gates the write.
    ///
    /// KNOWN NARROWING, and it is in the name: the count the kernel keeps is
    /// over EVERY fabric, and this looks only at `{tpg}/lun/lun_*` of the one
    /// target being planned. A backstore exported by another fabric or another
    /// TPG would read as unexported here and the plan would emit the write the
    /// kernel refuses. It is unreachable today — a backstore is named after
    /// the target that owns it, and the wizard refuses a second target on a
    /// zvol another one already exports — so it is recorded rather than
    /// guarded against with an observation nothing can currently produce.
    pub exported: bool,
    /// `{gp}/tg_pt_gp_id` of the group this LUN's spec asks for. EMPTY means
    /// "no valid id yet" and not "id zero": the show method returns zero bytes
    /// while `tg_pt_gp_valid_id` is unset.
    pub tg_pt_gp_id: String,
}

/// One node ACL of the TPG together with the mapped LUNs under it. The mapped
/// LUNs are carried because configfs refuses to remove a group that still has
/// children, so pruning an ACL is an ordered walk and not one `rmdir`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IscsiAclObserved {
    /// The initiator IQN, i.e. the directory name under `{tpg}/acls/`.
    pub initiator: String,
    /// Whether `{acl}/info` describes a live session — `None` when this node
    /// could not read it, or read it empty.
    ///
    /// TRAP this exists for: configfs offers NO way to tell an ACL this app
    /// created from one LIO generated itself under `generate_node_acls = 1`
    /// (there is no `dynamic_node_acl` attribute; `lio_target_initiator_attrs`
    /// is `info`, `cmdsn_depth`, `tag` — measured). A connected one is
    /// therefore assumed to be the kernel's business — see `prune_iscsi`.
    ///
    /// The three states are NOT two. `Some(false)` is "this node read the
    /// file and LIO said nobody is logged in"; `None` is "this node does not
    /// know". They are kept apart because the only action that follows from
    /// them is `rmdir` of the ACL, and `core_tpg_del_initiator_node_acl`
    /// force-logs-out whoever is on it. A kernel that renames `info`, or one
    /// read that fails with EIO, must not read as "no session" and revoke a
    /// client that is writing right now.
    pub session: Option<bool>,
    /// `(mapped LUN directory name, the symlink names inside it)`.
    pub mapped: Vec<(String, Vec<String>)>,
}

/// Whether an ACL's `info` describes a live session.
///
/// LIO writes "No active iSCSI Session for Initiator Endpoint: <iqn>" when
/// nothing is connected, and a block starting with the session's InitiatorName
/// when something is. Matching the NEGATIVE sentence is what keeps a future
/// extra line from reading as a session.
///
/// The idle sentence is MEASURED, read verbatim out of a real ACL's `info` on
/// a live TPG — not inferred from the LIO source. It matters more than it
/// looks: with no allowlist this string is the ONLY thing separating an ACL
/// the app abandoned from one the kernel generated for a client that is
/// logged in right now, because configfs publishes no `dynamic_*` attribute
/// to tell them apart (also measured — the ACL carries only `cmdsn_depth`,
/// `info` and `tag`).
pub fn acl_info_connected(info: &str) -> bool {
    let text = info.trim();
    !text.is_empty() && !text.starts_with("No active iSCSI Session")
}

/// What the kernel already holds for one iSCSI target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IscsiObserved {
    /// One entry per LUN of the spec, in the same order.
    pub devices: Vec<IscsiDeviceObserved>,
    /// Every ACL under `{tpg}/acls/`, whether the spec names it or not.
    pub acls: Vec<IscsiAclObserved>,
    /// Every portal directory under `{tpg}/np/` — `<address>:<port>`.
    pub portals: Vec<String>,
    /// Every LUN directory under `{tpg}/lun/` with the symlinks inside it.
    pub luns: Vec<(String, Vec<String>)>,
    /// The backstores that will be left with nothing pointing at them once the
    /// prune has run, each with the non-default ALUA groups underneath it:
    /// `(core/<hba>/<name>, ["tentanas_gp2", …])`.
    ///
    /// WHY: a LUN that leaves the target takes `{tpg}/lun/lun_N` with it, but
    /// the BACKSTORE underneath holds the zvol open — `zfs destroy` and
    /// `zpool export` then fail on a device nobody is exporting any more,
    /// which is the orphan §5.8 forbids. The names are never guessed from a
    /// prefix: they are the symlink names read out of the very LUN directories
    /// that are going away, so nothing another target still exports can appear
    /// here.
    pub stale_devices: Vec<(String, Vec<String>)>,
    /// The current value of every attribute `plan_iscsi` may write and this
    /// node could read, keyed by the path the PLAN uses.
    ///
    /// WHY the key is the production path while the read comes from `root`:
    /// the plan renders absolute `/sys/kernel/config/target/...` strings, and
    /// the tests observe a temporary directory. Keying the map the way the
    /// plan asks for it is what lets the same comparison work in both.
    ///
    /// A missing key means "this node did not read that attribute", which is
    /// NOT "it is empty": the write goes out. Skipping only ever happens on an
    /// exact match, so a value the kernel prints back in another shape than it
    /// takes costs one redundant write and never a wrong state.
    pub attrs: BTreeMap<String, String>,
}

/// One configfs attribute, or `None` when this process could not read it.
///
/// The Option is the whole point and it is not defensive style: EVERY caller
/// below turns an observation into a decision, and two of those decisions —
/// "this ACL has no session, remove it" and "this attribute already holds the
/// wanted value, skip the write" — are only safe on a read that actually
/// happened. A failed read collapsed into `""` reads as "empty", and an empty
/// `{acl}/info` reads as "nobody is logged in", which is how an unreadable
/// file force-logs-out three live clients. `None` means "unknown" and every
/// caller has to say what it does with that.
fn attr(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

/// The same read where "unreadable" and "empty" genuinely mean the same thing
/// to the caller — a value compared against a non-empty wanted one, where an
/// unknown reading has to lose and produce the write.
fn attr_or_empty(dir: &Path, name: &str) -> String {
    attr(dir, name).unwrap_or_default()
}

/// The names of the entries of `dir` that are directories, sorted.
fn child_dir_names(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|p| p.is_dir() && !p.is_symlink())
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// The names of the symlinks directly inside `dir`, sorted.
fn child_link_names(dir: &Path) -> Vec<String> {
    entries(dir)
        .into_iter()
        .filter(|p| p.is_symlink())
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Everything `plan_iscsi` needs to know about this node's current state.
pub fn observe_iscsi(root: &Path, spec: &IscsiTargetSpec) -> IscsiObserved {
    let tpg = root.join("iscsi").join(&spec.iqn).join("tpgt_1");
    let tpg_key = format!("{TARGET_CONFIGFS}/iscsi/{}/tpgt_1", spec.iqn);
    let mut attrs: BTreeMap<String, String> = BTreeMap::new();
    let mut record = |dir: &Path, name: &str, key: String| {
        if let Some(value) = attr(dir, name) {
            attrs.insert(key, value);
        }
    };
    let lun_dirs: Vec<(String, Vec<String>)> = child_dir_names(&tpg.join("lun"))
        .into_iter()
        .filter(|name| name.starts_with("lun_"))
        .map(|name| {
            let links = child_link_names(&tpg.join("lun").join(&name));
            (name, links)
        })
        .collect();
    let devices = spec
        .luns
        .iter()
        .map(|lun| {
            let dev = root.join("core").join(IBLOCK_HBA).join(&lun.name);
            let group = spec
                .port_groups
                .iter()
                .find(|g| g.group_id == lun.group_id)
                .map(|g| g.lio_name())
                .unwrap_or_default();
            let gp = dev.join("alua").join(&group);
            let gp_key =
                format!("{TARGET_CONFIGFS}/core/{IBLOCK_HBA}/{}/alua/{group}", lun.name);
            for name in ["alua_access_type", "alua_access_state", "preferred"] {
                record(&gp, name, format!("{gp_key}/{name}"));
            }
            IscsiDeviceObserved {
                name: lun.name.clone(),
                configured: attr_or_empty(&dev, "enable") == "1",
                exported: lun_dirs
                    .iter()
                    .any(|(_, links)| links.iter().any(|l| *l == lun.name)),
                unit_serial: attr_or_empty(&dev.join("wwn"), "vpd_unit_serial"),
                tg_pt_gp_id: attr_or_empty(&gp, "tg_pt_gp_id"),
            }
        })
        .collect();
    for name in [
        "attrib/generate_node_acls",
        "attrib/cache_dynamic_acls",
        "attrib/demo_mode_write_protect",
        "attrib/authentication",
        "param/AuthMethod",
        "enable",
    ] {
        let (dir, file) = name.rsplit_once('/').unwrap_or((".", name));
        record(&tpg.join(dir), file, format!("{tpg_key}/{name}"));
    }
    for portal in &spec.portals {
        let np = format!("{}:{}", portal.address, portal.port);
        record(
            &tpg.join("np").join(&np),
            "iser",
            format!("{tpg_key}/np/{np}/iser"),
        );
    }
    // TRAP: a node ACL and a TPG both carry configfs DEFAULT GROUPS, which are
    // directories like any other. MEASURED on a live ACL, the four are exactly
    // `attrib/`, `auth/`, `param/` and `fabric_statistics/` (the plain files
    // beside them being `cmdsn_depth`, `info` and `tag`). Only `lun_<n>` is a
    // mapped LUN, and only those may ever reach a removal step.
    //
    // configfs refuses `rmdir` on a default group with EPERM — also measured —
    // so a wrong name here would fail rather than dismantle a live ACL. That
    // is a backstop, not the reason: the filter is what keeps the plan from
    // ever aiming at one.
    let mapped_lun = |name: &String| name.starts_with("lun_");
    let acls = child_dir_names(&tpg.join("acls"))
        .into_iter()
        .map(|initiator| {
            let acl = tpg.join("acls").join(&initiator);
            let mapped = child_dir_names(&acl)
                .into_iter()
                .filter(&mapped_lun)
                .map(|name| {
                    let links = child_link_names(&acl.join(&name));
                    (name, links)
                })
                .collect();
            // An `info` this node could not read, or read empty, is NOT "no
            // session" — see `IscsiAclObserved::session`. `attr` already
            // separates the failed read; the empty-string case joins it here,
            // because LIO's idle answer is a whole sentence and an ACL that
            // prints nothing at all is a shape nobody has measured.
            let session = attr(&acl, "info")
                .filter(|text| !text.trim().is_empty())
                .map(|text| acl_info_connected(&text));
            IscsiAclObserved {
                initiator,
                session,
                mapped,
            }
        })
        .collect();
    // The backstores the prune will orphan. A name only gets here by being the
    // symlink inside a LUN directory the spec dropped, and only when nothing
    // the spec keeps points at it too.
    let wanted_luns: Vec<String> = spec.luns.iter().map(|l| format!("lun_{}", l.index)).collect();
    let mut stale_devices: Vec<(String, Vec<String>)> = Vec::new();
    for (dir, links) in &lun_dirs {
        if wanted_luns.contains(dir) {
            continue;
        }
        for name in links {
            let kept_by_spec = spec.luns.iter().any(|l| l.name == *name);
            let kept_by_another_lun = lun_dirs
                .iter()
                .any(|(other, l)| wanted_luns.contains(other) && l.contains(name));
            if kept_by_spec || kept_by_another_lun || stale_devices.iter().any(|(n, _)| n == name) {
                continue;
            }
            // A user-created target port group is a child item and has to go
            // before its device; `default_tg_pt_gp` belongs to the device and
            // goes with it — the same rule `remove_iscsi` walks.
            let groups = child_dir_names(&root.join("core").join(IBLOCK_HBA).join(name).join("alua"))
                .into_iter()
                .filter(|g| g != "default_tg_pt_gp")
                .collect();
            stale_devices.push((name.clone(), groups));
        }
    }
    IscsiObserved {
        devices,
        stale_devices,
        attrs,
        acls,
        portals: child_dir_names(&tpg.join("np")),
        luns: lun_dirs,
    }
}

/// One nvmet port as it currently is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NvmetPortObserved {
    pub id: u32,
    /// The port already carries exactly the wanted transport, address and
    /// service id, so its `addr_*` attributes must NOT be rewritten.
    pub configured: bool,
}

/// One namespace of the subsystem as it currently is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NvmetNsObserved {
    pub nsid: u32,
    pub enabled: bool,
    /// `device_path` and `device_uuid` already hold the wanted values.
    pub matches: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NvmetObserved {
    /// One entry per portal of the spec, in the same order.
    pub ports: Vec<NvmetPortObserved>,
    /// One entry per namespace of the spec, in the same order.
    pub namespaces: Vec<NvmetNsObserved>,
    /// `attr_serial` as the kernel prints it, trimmed. Empty when the
    /// subsystem does not exist yet.
    pub serial: String,
    /// `attr_model`, same rule.
    pub model: String,
    /// `attr_allow_any_host` VERBATIM (`"1"`, `"0"` or empty for a subsystem
    /// that is not there). A string rather than a bool so "the kernel does not
    /// hold this yet" stays distinguishable from "the kernel holds 0".
    pub allow_any_host: String,
    /// Every namespace id currently under the subsystem, spec or not — the
    /// prune list.
    pub existing_namespaces: Vec<u32>,
    /// Every link name under `{sub}/allowed_hosts/`.
    pub allowed_hosts: Vec<String>,
    /// Every port id whose `{port}/subsystems/<nqn>` link points at THIS
    /// subsystem — the ports it currently answers on, spec or not.
    ///
    /// WHY it is separate from `ports`: `ports` is one entry per portal of the
    /// SPEC, i.e. where the subsystem is meant to answer. Without this second
    /// list the plan has no way to see where it answers TODAY, and the
    /// difference is the whole point. Re-picking the interface — the one thing
    /// the portal-drift alert asks an admin to do — creates a port on the new
    /// address and leaves the subsystem linked into the old one, so a raw disk
    /// keeps being served on the address the admin just stopped choosing.
    /// Changing `tcp+rdma` to `tcp` leaves the RDMA port answering while the
    /// UI chip says TCP.
    pub linked_ports: Vec<u32>,
    /// Of the hosts under `{sub}/allowed_hosts/`, the ones ANOTHER subsystem
    /// on this node also links.
    ///
    /// THE fact that makes a host object different from everything else this
    /// plan touches: `hosts/<nqn>/` is NODE-WIDE. A subsystem does not own it,
    /// it only symlinks to it, and the DH-HMAC-CHAP key lives on the object —
    /// so "this host left my allowlist" says nothing about whether the key is
    /// still authenticating somebody else.
    ///
    /// And the topology that makes it ordinary rather than exotic: the UI
    /// exports one LUN per target (§6.1), so two zvols handed to the same
    /// VMware host are two targets carrying the SAME host NQN. Clearing the
    /// key on one of them took the other one's client offline at its next
    /// reconnect, in silence, with the row still green.
    ///
    /// `remove_nvmet` has always asked this question before reaping a host;
    /// the prune did not, which is what this list is for.
    pub shared_hosts: Vec<String>,
    /// Host NQNs whose `dhchap_key` currently reads non-empty.
    ///
    /// Needed because a DH-HMAC-CHAP key CANNOT BE CLEARED IN PLACE. Measured
    /// (obs. 37): every candidate sentinel — `"\n"`, `"NULL"`, `"0"`, `" "` —
    /// is refused with EINVAL, and the empty string is accepted and changes
    /// nothing, exactly like LIO's empty write in obs. 6. Linked or unlinked
    /// makes no difference.
    ///
    /// So "this host must stop requiring a key" is spelled `rmdir` of the host
    /// object: obs. 24 showed a recreated object comes back with fresh
    /// attributes. Knowing WHICH objects hold a key is what keeps that from
    /// happening on every apply of every unauthenticated target.
    ///
    /// A key this process cannot read is not listed. The helper runs as root
    /// and reads it; the core, which renders the same plan unprivileged for
    /// the preview, cannot once `Protect` has run — so the preview may show
    /// one recreate fewer than the apply performs. That is a cosmetic
    /// divergence in a preview, not a different outcome.
    pub hosts_with_key: Vec<String>,
    /// The same for `dhchap_ctrl_key` — a subsystem going from bidirectional
    /// DH-HMAC-CHAP back to one-way has to stop presenting a controller key,
    /// and it cannot be cleared either.
    pub hosts_with_ctrl_key: Vec<String>,
    /// Host NQNs whose key material on the node ALREADY equals what this spec
    /// wants — both `dhchap_key` and `dhchap_ctrl_key`, empty counting as a
    /// value.
    ///
    /// The comparison is made here, in the observation, and only its RESULT is
    /// carried: putting the kernel's copy of a secret into a struct that
    /// travels through the planner is a leak waiting for the first `Debug`.
    ///
    /// It is what lets `host_verdict` tell "another target wants the same key"
    /// from "another target wants a different one" — the difference between a
    /// shared host that works and one the node cannot serve at all.
    pub hosts_matching_spec: Vec<String>,
}

/// What one subsystem may do to a node-wide nvmet host object.
///
/// THE authority on that question, and there is one because the question kept
/// being answered separately for each operation that had just gone wrong:
/// round 6 for the tick, round 7 for the CLEAR, round 8 for the WRITE. Every
/// one was the same fact — `hosts/<nqn>/` belongs to the NODE, carries the
/// key, and is linked by every subsystem that allows that host, which is the
/// ordinary case here because the UI exports one LUN per target (§6.1), so two
/// zvols for one VMware host are two targets naming one NQN.
///
/// So: link, unlink, write, clear and remove all ask this, and nothing else
/// decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostVerdict {
    /// Nothing else on this node links the object. This subsystem owns it and
    /// may write it, empty it (by removing it) or remove it outright.
    Sole,
    /// Another subsystem links it AND it already holds exactly the key
    /// material this spec wants. Nothing needs doing and nothing may be done:
    /// the object is shared and correct.
    SharedAndAgrees,
    /// Another subsystem links it and holds DIFFERENT key material.
    ///
    /// There is no correct apply here. One object holds one key: writing ours
    /// takes the other target's client offline at its next reconnect, and
    /// leaving theirs means this target is not what its own row says it is.
    /// The node refuses rather than picking a loser silently — which is what
    /// it used to do, while the wizard promised in five languages that it
    /// would not.
    SharedAndConflicts,
}

/// The verdict for one host of the spec.
pub fn host_verdict(observed: &NvmetObserved, nqn: &str) -> HostVerdict {
    if !observed.shared_hosts.iter().any(|h| h == nqn) {
        return HostVerdict::Sole;
    }
    if observed.hosts_matching_spec.iter().any(|h| h == nqn) {
        return HostVerdict::SharedAndAgrees;
    }
    HostVerdict::SharedAndConflicts
}

/// Everything `plan_nvmet` needs to know about the node's current state.
///
/// A PORT IS NODE-WIDE: it is an address plus a transport, and every subsystem
/// reachable there is linked into the same one. So an existing port with the
/// wanted attributes is reused as-is, and only a genuinely new index is
/// configured — which is what makes two targets on `10.10.0.5:4420/tcp`
/// possible at all.
pub fn observe_nvmet(root: &Path, spec: &NvmetSubsystemSpec) -> NvmetObserved {
    let ports_dir = root.join("ports");
    let mut linked_ports: Vec<u32> = Vec::new();
    let existing: Vec<(u32, String, String, String)> = entries(&ports_dir)
        .into_iter()
        .filter_map(|dir| {
            let id = dir.file_name()?.to_string_lossy().parse::<u32>().ok()?;
            // `symlink_metadata`, not `exists`: a dangling link still means
            // this subsystem is attached to that port as far as configfs is
            // concerned, and `exists()` follows the link.
            if std::fs::symlink_metadata(dir.join("subsystems").join(&spec.nqn)).is_ok() {
                linked_ports.push(id);
            }
            Some((
                id,
                attr_or_empty(&dir, "addr_trtype"),
                attr_or_empty(&dir, "addr_traddr"),
                attr_or_empty(&dir, "addr_trsvcid"),
            ))
        })
        .collect();
    let mut taken: Vec<u32> = existing.iter().map(|(id, ..)| *id).collect();
    let mut ports = Vec::with_capacity(spec.portals.len());
    for portal in &spec.portals {
        let svcid = portal.port.to_string();
        match existing.iter().find(|(_, trtype, traddr, trsvcid)| {
            *trtype == portal.transport && *traddr == portal.address && *trsvcid == svcid
        }) {
            Some((id, ..)) => ports.push(NvmetPortObserved {
                id: *id,
                configured: true,
            }),
            None => {
                let next = (1u32..).find(|c| !taken.contains(c)).unwrap_or(1);
                taken.push(next);
                ports.push(NvmetPortObserved {
                    id: next,
                    configured: false,
                });
            }
        }
    }

    let sub = root.join("subsystems").join(&spec.nqn);
    let namespaces = spec
        .namespaces
        .iter()
        .map(|ns| {
            let dir = sub.join("namespaces").join(ns.index.to_string());
            if !dir.is_dir() {
                return NvmetNsObserved {
                    nsid: ns.index,
                    enabled: false,
                    matches: false,
                };
            }
            NvmetNsObserved {
                nsid: ns.index,
                enabled: attr_or_empty(&dir, "enable") == "1",
                matches: attr_or_empty(&dir, "device_path") == ns.device_path
                    && attr_or_empty(&dir, "device_uuid") == ns.uuid,
            }
        })
        .collect();
    let existing_namespaces = child_dir_names(&sub.join("namespaces"))
        .into_iter()
        .filter_map(|name| name.parse::<u32>().ok())
        .collect();
    NvmetObserved {
        ports,
        namespaces,
        serial: attr_or_empty(&sub, "attr_serial"),
        model: attr_or_empty(&sub, "attr_model"),
        allow_any_host: attr_or_empty(&sub, "attr_allow_any_host"),
        existing_namespaces,
        shared_hosts: shared_hosts(root, &spec.nqn),
        hosts_with_key: hosts_holding(root, spec, "dhchap_key"),
        hosts_with_ctrl_key: hosts_holding(root, spec, "dhchap_ctrl_key"),
        hosts_matching_spec: hosts_matching_spec(root, spec),
        allowed_hosts: child_link_names(&sub.join("allowed_hosts")),
        linked_ports,
    }
}

/// Every host NQN that a subsystem OTHER than `nqn` links on this node.
///
/// A directory walk of `subsystems/*/allowed_hosts/`, unprivileged like the
/// rest of the observation. It answers exactly the question `remove_nvmet`
/// already asks before it reaps a host object — "is anybody else still using
/// this?" — for the prune, which used to answer it by assuming no.
///
/// A directory this walk could not read reads as "nobody else links it" — an
/// under-report, and the direction that would let the plan aim a removal at an
/// object another subsystem depends on.
///
/// THE KERNEL HOLDS THE OTHER END OF THIS. MEASURED (obs. 42 and 44): `rmdir`
/// of a host object that anything still links is EBUSY. So a wrong answer here
/// costs a failed apply step — loud, in the job log, with the target's own
/// error — and not another client's key. That is the fact that makes this
/// mechanism safe rather than merely careful, and it is worth knowing before
/// anyone "simplifies" the guard away: the app and the kernel are enforcing
/// the same invariant from two sides, and only one of them is in this file.
///
/// (The under-report is also unreachable on the executing side: configfs is
/// 0755/0644 throughout and the helper is root. The core, rendering an
/// unprivileged preview, executes nothing.)
/// Of the hosts this spec names, the ones whose `attribute` reads non-empty.
///
/// Only the spec's own hosts: those are the only objects the plan may decide
/// to recreate, and reading every host on the node would be a syscall per host
/// for nothing.
fn hosts_holding(root: &Path, spec: &NvmetSubsystemSpec, attribute: &str) -> Vec<String> {
    spec.hosts
        .iter()
        .filter(|h| {
            attr(&root.join("hosts").join(&h.nqn), attribute)
                .is_some_and(|value| !value.is_empty())
        })
        .map(|h| h.nqn.clone())
        .collect()
}

/// Of the hosts this spec names, the ones whose key material on the node is
/// already exactly what the spec asks for.
///
/// Both attributes, with "absent or empty" treated as a value of its own, so a
/// spec that wants no key agrees with an object that has none. The comparison
/// happens here and the secret does not leave: only the NQN is returned.
///
/// A key this process cannot read counts as NOT matching. On the helper, which
/// is root, that never happens; on the core rendering an unprivileged preview
/// it means the preview is more pessimistic than the apply, which is the safe
/// direction for a screen that executes nothing.
fn hosts_matching_spec(root: &Path, spec: &NvmetSubsystemSpec) -> Vec<String> {
    spec.hosts
        .iter()
        .filter(|h| {
            let dir = root.join("hosts").join(&h.nqn);
            let held = |name: &str, wanted: &str| match attr(&dir, name) {
                Some(value) => value == wanted,
                // Nothing to read is "no key", which agrees with a spec that
                // wants none and disagrees with one that wants a key.
                None => wanted.is_empty(),
            };
            held("dhchap_key", &h.dhchap_key) && held("dhchap_ctrl_key", &h.dhchap_ctrl_key)
        })
        .map(|h| h.nqn.clone())
        .collect()
}

fn shared_hosts(root: &Path, nqn: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for subsystem in entries(&root.join("subsystems")) {
        if subsystem.file_name().is_some_and(|n| n == nqn) {
            continue;
        }
        for host in child_link_names(&subsystem.join("allowed_hosts")) {
            if !out.contains(&host) {
                out.push(host);
            }
        }
    }
    out.sort();
    out
}

// =============================================================================
// iSCSI (LIO)
// =============================================================================

/// The configfs tree of one iSCSI target, in the order the kernel accepts it.
///
/// The order is not cosmetic, and neither is `observed`. A backstore must
/// exist and be enabled before a LUN links to it, everything the spec still
/// names is created before anything it dropped is taken away, and `enable = 1`
/// is the last step.
///
/// TRAP that is NOT here, and must never come back: this plan does not cycle
/// `{tpg}/enable`. LIO's disable branch is `iscsit_tpg_disable_portal_group(tpg,
/// 1)` — the kernel's own comment says "assumes force=1" — which logs every
/// initiator of the target out, and `apply` re-renders EVERY target on every
/// mutation, so one edit anywhere would have dropped every session on the node.
/// It bought nothing: measured on a live LIO target, `param/AuthMethod`,
/// `attrib/generate_node_acls` and a repeated `enable = 1` all succeed while
/// the TPG reads `enable = 1`.
///
/// **The allowlist is a filter, never authentication.** An initiator IQN is a
/// string the client sends about itself; anyone can send any string. So:
///   * initiators listed  → `generate_node_acls = 0` and one ACL per IQN. Only
///     those IQNs get in, and with CHAP on, each carries the credentials.
///   * initiators empty   → `generate_node_acls = 1`: LIO builds an ACL for
///     whoever logs in, and takes the credentials from the TPG-level `auth/`
///     directory. With CHAP that is still authenticated; without it the target
///     is open, which is what the wizard warns about in so many words.
pub fn plan_iscsi(
    spec: &IscsiTargetSpec,
    observed: &IscsiObserved,
) -> Result<Vec<ConfigfsStep>, CatalogError> {
    validate_iscsi(spec)?;
    if observed.devices.len() != spec.luns.len() {
        return Err(invalid("one observed backstore per LUN is required"));
    }
    let mut steps = Vec::new();
    let tpg = format!("{TARGET_CONFIGFS}/iscsi/{}/tpgt_1", spec.iqn);
    // The diff. `apply` is a reconcile that runs on every mutation of this
    // target and on every restore, so the second run is the normal case: a
    // plan that re-renders every attribute unconditionally writes ~15 times
    // per target for nothing, buries the lines that DID change in a job log
    // nobody can then read, and re-chmods credentials that never moved. A
    // write is emitted only when the kernel does not already hold the value —
    // and a value this node could not read is not "held", so an unreadable
    // attribute always produces its write. See `IscsiObserved::attrs`.
    //
    // MEASURED (run 5) that comparing the written form is sound at all:
    // `param/AuthMethod`, all four `attrib/*`, `tg_pt_gp_id`,
    // `alua_access_state` and `np/<portal>/iser` read back byte for byte as
    // they are written. The ONE exception is `alua_access_type` — written `1`,
    // printed `Implicit` — which is compared separately below instead.
    let write = |steps: &mut Vec<ConfigfsStep>, path: String, value: &str| {
        if observed.attrs.get(&path).map(String::as_str) == Some(value) {
            return;
        }
        steps.push(ConfigfsStep::write(path, value));
    };

    // ----- backstores, with their ALUA groups -----
    for (lun, state) in spec.luns.iter().zip(&observed.devices) {
        let dev = format!("{TARGET_CONFIGFS}/core/{IBLOCK_HBA}/{}", lun.name);
        steps.push(ConfigfsStep::Mkdir(format!(
            "{TARGET_CONFIGFS}/core/{IBLOCK_HBA}"
        )));
        steps.push(ConfigfsStep::Mkdir(dev.clone()));
        if !state.configured {
            // MEASURED: a second `udev_path=` is EEXIST, "Unable to set
            // udev_path= while ib_dev->ibd_bd exists" — and `ibd_bd` is what
            // `enable = 1` opened on the previous run.
            steps.push(ConfigfsStep::write(
                format!("{dev}/control"),
                format!("udev_path={}", lun.device_path),
            ));
        }
        // The identity multipath matches two paths by. Written before
        // `enable`, because LIO pins the VPD page when the device comes up.
        //
        // TWO guards, and the first one is what matters. MEASURED: this write
        // is EINVAL — "Unable to set VPD Unit Serial while active 1
        // $FABRIC_MOD exports exist" — from the moment a LUN links to the
        // backstore, and `apply_plan` stops at the first failed step. So it is
        // gated on the OBSERVED export (`{tpg}/lun/lun_N` linking this device)
        // and never on a comparison alone: on a device the kernel is already
        // exporting there is nothing this write could achieve except breaking
        // every subsequent apply of the target.
        //
        // The second guard is `serial_holds`, which is deliberately loose.
        // LIO's show method prints `T10 VPD Unit Serial Number: <value>` and
        // that LABEL is the one format in this file nobody has measured. A
        // strict parse that misses the label yields an empty serial, which
        // never equals the uuid — so on the day the label changes, a strict
        // parse would emit the forbidden write instead of skipping it. Suffix
        // matching cannot fail that way: whatever the label turns into, a
        // serial that ENDS with this LUN's uuid is this LUN's serial.
        if !state.exported && !serial_holds(&state.unit_serial, &lun.uuid) {
            steps.push(ConfigfsStep::write(
                format!("{dev}/wwn/vpd_unit_serial"),
                lun.uuid.clone(),
            ));
        }
        if !state.configured {
            // MEASURED: a second `enable = 1` on a configured device is EEXIST.
            steps.push(ConfigfsStep::write(format!("{dev}/enable"), "1"));
        }

        let group = group_of(&spec.port_groups, lun.group_id)?;
        let gp = format!("{dev}/alua/{}", group.lio_name());
        steps.push(ConfigfsStep::Mkdir(gp.clone()));
        // MEASURED: the id is accepted ONCE — EINVAL, "ALUA TG PT Group
        // already has a valid ID, ignoring request".
        //
        // Read out of mainline and NOT measured: the show method returns zero
        // bytes while `tg_pt_gp_valid_id` is unset, so an empty read means
        // "not set yet" rather than "zero". The skip is safe either way — it
        // only ever leaves a group whose id already reads as the wanted one
        // alone.
        if state.tg_pt_gp_id != group.group_id.to_string() {
            steps.push(ConfigfsStep::write(
                format!("{gp}/tg_pt_gp_id"),
                group.group_id.to_string(),
            ));
        }
        // NOT the plain comparison, because this attribute does not read back
        // what it takes: it is written as `1` and printed as `Implicit`
        // (MEASURED, run 5). See `ALUA_ACCESS_TYPE_SHOWN`. Both forms are
        // accepted as "already implicit" so that a kernel which one day prints
        // the number is not fought with a write every apply either.
        let access_type = observed
            .attrs
            .get(&format!("{gp}/alua_access_type"))
            .map(String::as_str);
        if !matches!(
            access_type,
            Some(ALUA_ACCESS_TYPE_SHOWN | ALUA_ACCESS_TYPE_IMPLICIT)
        ) {
            steps.push(ConfigfsStep::write(
                format!("{gp}/alua_access_type"),
                ALUA_ACCESS_TYPE_IMPLICIT,
            ));
        }
        write(
            &mut steps,
            format!("{gp}/alua_access_state"),
            group.lio_state()?,
        );
        write(
            &mut steps,
            format!("{gp}/preferred"),
            if group.preferred { "1" } else { "0" },
        );
    }

    // ----- target and TPG -----
    steps.push(ConfigfsStep::Mkdir(format!(
        "{TARGET_CONFIGFS}/iscsi/{}",
        spec.iqn
    )));
    steps.push(ConfigfsStep::Mkdir(tpg.clone()));

    let allowlisted = !spec.initiators.is_empty();
    write(
        &mut steps,
        format!("{tpg}/attrib/generate_node_acls"),
        if allowlisted { "0" } else { "1" },
    );
    // A generated ACL is kept only for the life of the session: the next login
    // is authenticated again instead of inheriting a cached grant.
    write(
        &mut steps,
        format!("{tpg}/attrib/cache_dynamic_acls"),
        "0",
    );
    // Without an allowlist and without CHAP, LIO's demo mode would otherwise
    // hand every initiator a READ-ONLY LUN and call it a target. Whatever this
    // target is, it is not a surprise: writable when it is meant to be.
    write(
        &mut steps,
        format!("{tpg}/attrib/demo_mode_write_protect"),
        "0",
    );
    write(
        &mut steps,
        format!("{tpg}/attrib/authentication"),
        if spec.auth.enabled { "1" } else { "0" },
    );
    write(
        &mut steps,
        format!("{tpg}/param/AuthMethod"),
        if spec.auth.enabled { "CHAP" } else { "None" },
    );

    // TPG-level credentials: what a generated ACL authenticates against. They
    // are written whatever the allowlist says, so switching the allowlist off
    // later cannot leave an open target behind.
    steps.extend(auth_steps(&format!("{tpg}/auth"), &spec.auth));

    // ----- LUNs -----
    for lun in &spec.luns {
        let dev = format!("{TARGET_CONFIGFS}/core/{IBLOCK_HBA}/{}", lun.name);
        let lun_dir = format!("{tpg}/lun/lun_{}", lun.index);
        steps.push(ConfigfsStep::Mkdir(lun_dir.clone()));
        steps.push(ConfigfsStep::Symlink {
            link: format!("{lun_dir}/{}", lun.name),
            target: dev,
        });
        let group = group_of(&spec.port_groups, lun.group_id)?;
        // NOT diffed, unlike every other attribute here: this one TAKES
        // `tg_pt_gp_name=<name>` and SHOWS a multi-line description of the
        // group, so the value written and the value read are not the same
        // string and nothing can be compared. Rewriting it is harmless — LIO's
        // store method re-points the LUN at a group it may already be in.
        steps.push(ConfigfsStep::write(
            format!("{lun_dir}/alua_tg_pt_gp"),
            format!("tg_pt_gp_name={}", group.lio_name()),
        ));
    }

    // ----- the allowlist -----
    for initiator in &spec.initiators {
        let acl = format!("{tpg}/acls/{initiator}");
        steps.push(ConfigfsStep::Mkdir(acl.clone()));
        for lun in &spec.luns {
            let mapped = format!("{acl}/lun_{}", lun.index);
            steps.push(ConfigfsStep::Mkdir(mapped.clone()));
            steps.push(ConfigfsStep::Symlink {
                link: format!("{mapped}/{}", lun.name),
                target: format!("{tpg}/lun/lun_{}", lun.index),
            });
        }
        steps.extend(auth_steps(&format!("{acl}/auth"), &spec.auth));
    }

    // ----- portals -----
    for portal in &spec.portals {
        let np = format!("{tpg}/np/{}:{}", portal.address, portal.port);
        steps.push(ConfigfsStep::Mkdir(np.clone()));
        // iSER rides the SAME portal as iSCSI: the login is TCP either way and
        // the initiator asks to switch to RDMA afterwards. So it is a flag on
        // the portal, not a second portal (§5.5a).
        write(
            &mut steps,
            format!("{np}/iser"),
            if portal.transport == "iser" { "1" } else { "0" },
        );
    }

    steps.extend(prune_iscsi(spec, observed, &tpg));
    // Last, and MEASURED to be safe when it does go out: a second `enable = 1`
    // on a live TPG succeeds (`target_fabric_tpg_base_enable_store`
    // short-circuits on `se_tpg->enabled == op`). It is skipped when the TPG
    // already reads 1, which is the ordinary case.
    //
    // A TPG somebody disabled BY HAND therefore comes back on the next apply —
    // and only then. This is not periodic self-healing, though the reason
    // changed: the 20 s tick DOES reach the kernel now (it sweeps removals and
    // applies), but its apply half only picks up rows the node is not serving
    // at all, and a hand-disabled TPG still has its configfs directory. So
    // "the next apply" remains the next mutation of this target or the next
    // restore, which may be days away. Saying otherwise would promise a repair
    // nothing performs.
    write(&mut steps, format!("{tpg}/enable"), "1");
    Ok(steps)
}

/// Whether a `vpd_unit_serial` reading already carries this LUN's identity.
///
/// MEASURED (run 5): the show method really does print
/// `T10 VPD Unit Serial Number: <value>`, label and all — on a device with no
/// serial written yet it reads exactly `T10 VPD Unit Serial Number: `. So a
/// plain equality against the uuid would NEVER match, and the suffix
/// comparison is required rather than a widened net.
///
/// It is a suffix match and not a parse of that label because the label is the
/// kernel's to change: a strict parse that stopped matching would yield an
/// empty serial, which never equals the uuid, and the write it would then emit
/// is the one measured as EINVAL on an exported LUN (obs. 8). The only thing a
/// suffix can be fooled by is a serial genuinely ending in this uuid, and it
/// is read from THIS backstore's own directory, so there is no other device's
/// serial to collide with.
///
/// `!state.exported` is what actually gates the write; this is the second
/// guard, not the first.
fn serial_holds(reading: &str, uuid: &str) -> bool {
    !uuid.is_empty() && reading.trim().ends_with(uuid)
}

/// The objects the kernel holds for this target that the spec no longer names.
///
/// This is the half of the reconcile the first two rounds did not have, and
/// its absence was not cosmetic: taking an initiator off the allowlist left
/// `{tpg}/acls/<iqn>` — CHAP credentials and all — serving in the kernel while
/// the UI said the change was saved, and re-picking a portal left the OLD
/// `np/<address>:3260` listening next to the new one, so the export answered
/// on an interface nobody chose. That is the exact exposure the portal-drift
/// policy exists to prevent.
///
/// MEASURED on a LIVE target: unlinking a LUN's backstore link, and `rmdir` of
/// `{tpg}/lun/lun_0`, of `{tpg}/acls/<iqn>` and of `{tpg}/np/<addr>:<port>` all
/// succeed while `{tpg}/enable` reads 1 — and it still reads 1 afterwards. So
/// revoking one initiator and dropping a stale portal happen with the target
/// serving, and nothing here needs the TPG taken down or any ordering against
/// `enable`.
///
/// Emitted after everything the spec still wants, so a re-picked portal never
/// has a window with nothing listening. The order INSIDE the prune is what
/// matters, because configfs refuses `rmdir` on a group with children
/// (`-ENOTEMPTY`) or one something still links to (`-EBUSY`): a mapped LUN's
/// symlink before the mapped LUN, every mapped LUN before its ACL, and every
/// ACL's mapped LUNs before the TPG LUN they point at.
fn prune_iscsi(
    spec: &IscsiTargetSpec,
    observed: &IscsiObserved,
    tpg: &str,
) -> Vec<ConfigfsStep> {
    let wanted_luns: Vec<String> = spec.luns.iter().map(|l| format!("lun_{}", l.index)).collect();
    let mut steps = Vec::new();

    // Mapped LUNs of the ACLs that STAY: a LUN that left the target must stop
    // being mapped into the initiators that keep their access.
    for acl in &observed.acls {
        if !spec.initiators.iter().any(|i| *i == acl.initiator) {
            continue;
        }
        for (mapped, links) in &acl.mapped {
            if wanted_luns.contains(mapped) {
                continue;
            }
            let dir = format!("{tpg}/acls/{}/{mapped}", acl.initiator);
            for link in links {
                steps.push(ConfigfsStep::Unlink(format!("{dir}/{link}")));
            }
            steps.push(ConfigfsStep::Rmdir(dir));
        }
    }

    // ACLs the allowlist dropped. MEASURED: this succeeds on a live TPG and
    // the target keeps serving — `core_tpg_del_initiator_node_acl` shuts that
    // one initiator's sessions down as it goes, which is what revoking access
    // means. The alternative is a client that keeps writing to a disk it is no
    // longer allowed to see.
    //
    // TRAP: with NO allowlist the plan sets `generate_node_acls = 1`, and then
    // LIO creates an ACL of its own for every initiator that logs in. configfs
    // publishes nothing that tells those apart from ours (measured: an ACL
    // carries `attrib/`, `auth/`, `param/`, `fabric_statistics/`,
    // `cmdsn_depth`, `info`, `tag` — and no `dynamic_*` of any kind), so
    // removing one by name would force-log-out a live client on every
    // reconcile. The rule is therefore: with an allowlist, every unnamed ACL
    // is ours and goes; with no allowlist, an ACL goes only when this node
    // POSITIVELY read that it has no session — which is exactly the stale one
    // this app left behind, carrying credentials that would still let its
    // initiator in after the secret was changed.
    //
    // "Positively read" is the whole safety of it, and `session` is a
    // three-state for that reason: an `info` that could not be read, or read
    // empty, leaves the ACL ALONE. The alternative — an unreadable file
    // meaning "nobody is connected" — hands a single EIO, or a kernel that
    // renames the attribute, the power to force-log-out every client of a
    // target on the next mutation of any target on the node.
    //
    // TWO known limitations, neither of them measurable on the machine these
    // observations come from (it has no `iscsiadm`, so no initiator ever
    // logged in), both recorded rather than assumed away:
    //
    //   * the RACE. Between LIO creating a generated ACL during login and the
    //     session being registered on it, `info` reads as "No active iSCSI
    //     Session" — so a reconcile landing inside that window removes the ACL
    //     of a client that is logging in right now. The window is
    //     milliseconds, the consequence is one failed login that the initiator
    //     retries, and the alternative (never removing an unnamed ACL) leaves
    //     stale credentials serving forever. It is a deliberate trade, not an
    //     unnoticed one — `targets.initiators_hint` says so in the UI, where
    //     the operator who turns the allowlist off is standing.
    //   * the SHAPE of `info` while a session IS active has never been
    //     measured here; only the idle sentence has, verbatim. The predicate
    //     is written to survive that: it matches the NEGATIVE sentence, so any
    //     other text at all — including a shape nobody has seen — counts as a
    //     session and keeps the ACL.
    let allowlisted = !spec.initiators.is_empty();
    for acl in &observed.acls {
        if spec.initiators.iter().any(|i| *i == acl.initiator) {
            continue;
        }
        if !allowlisted && acl.session != Some(false) {
            continue;
        }
        for (mapped, links) in &acl.mapped {
            let dir = format!("{tpg}/acls/{}/{mapped}", acl.initiator);
            for link in links {
                steps.push(ConfigfsStep::Unlink(format!("{dir}/{link}")));
            }
            steps.push(ConfigfsStep::Rmdir(dir));
        }
        steps.push(ConfigfsStep::Rmdir(format!(
            "{tpg}/acls/{}",
            acl.initiator
        )));
    }

    // TPG LUNs, after every mapped LUN that pointed at them is gone.
    for (lun, links) in &observed.luns {
        if wanted_luns.contains(lun) {
            continue;
        }
        let dir = format!("{tpg}/lun/{lun}");
        for link in links {
            steps.push(ConfigfsStep::Unlink(format!("{dir}/{link}")));
        }
        steps.push(ConfigfsStep::Rmdir(dir));
    }

    // The BACKSTORES those LUNs pointed at, once nothing links them any more.
    // §5.8 forbids orphans and this is the one that bites hardest: an iblock
    // backstore holds the zvol OPEN, so a device left behind here makes
    // `zfs destroy` and `zpool export` fail on a volume nobody is exporting.
    // Necessarily last — configfs refuses `rmdir` on an object something still
    // links to with -EBUSY, so the LUN directory above has to be gone first.
    for (name, groups) in &observed.stale_devices {
        let dev = format!("{TARGET_CONFIGFS}/core/{IBLOCK_HBA}/{name}");
        for group in groups {
            steps.push(ConfigfsStep::Rmdir(format!("{dev}/alua/{group}")));
        }
        steps.push(ConfigfsStep::Rmdir(dev));
    }

    // Portals. The whole point of the drift policy: after an admin re-picks
    // the interface, the address the target used to answer on stops listening.
    // MEASURED: `rmdir` of an `np` on a live TPG succeeds and the TPG stays
    // enabled, so this costs the other portal's sessions nothing.
    let wanted_portals: Vec<String> = spec
        .portals
        .iter()
        .map(|p| format!("{}:{}", p.address, p.port))
        .collect();
    for np in &observed.portals {
        if wanted_portals.contains(np) {
            continue;
        }
        steps.push(ConfigfsStep::Rmdir(format!("{tpg}/np/{np}")));
    }
    steps
}

/// The credential files of one LIO `auth/` directory.
///
/// Turning CHAP off has to REMOVE the credentials, not hide them behind
/// `authentication = 0` — otherwise switching CHAP back on without typing a new
/// secret would revive the old one. LIO only accepts the literal `NULL` as
/// "unset" (`__iscsi_*_store` compares against it before clearing
/// `NAF_USERID_SET`/`NAF_PASSWORD_SET`), so the clear is its own step with that
/// sentinel in it — see `ConfigfsStep::Clear`.
///
/// TRAP: `authenticate_target` — the mutual half — is NOT written here and
/// must never be. In LIO it is `CONFIGFS_ATTR_RO` on both the TPG and the node
/// ACL (`__DEF_NACL_AUTH_INT` defines only a `_show`), and configfs refuses
/// `O_WRONLY` on a read-only attribute in `check_perm()`, before DAC, so not
/// even root gets the file open — MEASURED on a live target, where the write
/// fails with EACCES on both `{tpg}/auth` and `{acl}/auth`.
///
/// The kernel DERIVES the flag instead, and that too was measured: the
/// attribute read 0, and flipped to 1 BY ITSELF once `userid_mutual` and
/// `password_mutual` had been written. The same `__iscsi_*_store` clears it
/// again when either is set to `NULL`. Writing the mutual pair is therefore
/// the whole operation, which is why rtslib and targetcli carry
/// `authenticate_target` as a read-only property.
fn auth_steps(dir: &str, auth: &IscsiAuth) -> Vec<ConfigfsStep> {
    let clear_mutual = || {
        vec![
            ConfigfsStep::clear(format!("{dir}/userid_mutual"), LIO_CLEAR),
            ConfigfsStep::clear(format!("{dir}/password_mutual"), LIO_CLEAR),
        ]
    };
    if !auth.enabled {
        let mut steps = vec![
            ConfigfsStep::clear(format!("{dir}/userid"), LIO_CLEAR),
            ConfigfsStep::clear(format!("{dir}/password"), LIO_CLEAR),
        ];
        steps.extend(clear_mutual());
        return steps;
    }
    // The credentials are the one group of attributes that is NOT diffed, and
    // the reason is a permission, not an oversight: LIO's `auth/*` show
    // methods are gated behind `capable(CAP_SYS_ADMIN)`, so the core, which
    // renders this same plan unprivileged for the preview, can never read
    // them.
    //
    // MEASURED, from a real unprivileged account on a live node: `auth/userid`,
    // `auth/password` and both `*_mutual` are created `-rw-r--r-- root:root`,
    // yet reading any of them as an ordinary user fails with EPERM — the
    // capability check in the show method overrides the permissive mode bits.
    // The same account reads `param/AuthMethod` from the same TPG without a
    // problem, so the refusal is specific to `auth/*` and not a property of
    // configfs. nvmet is the mirror image: `dhchap_key` has no such gate and
    // is genuinely readable (measured) until `protect_attr` chmods it.
    //
    // The plan emits `Protect` for BOTH anyway, and that is deliberate rather
    // than an oversight: on nvmet the chmod is the only thing standing between
    // the key and every local account, while on LIO it is a second lock behind
    // the capability check — the mode bits really are 0644, so a kernel that
    // ever dropped that check would expose the CHAP secrets and this step is
    // what would still be holding. A defence that costs one `chmod` is not
    // worth making conditional on the kernel keeping a promise.
    //
    // The same EPERM is why the credentials are not diffed: an observation
    // only the root side can make would hand the admin a preview that does not
    // match the apply. Rewriting them costs two writes whose store methods
    // have no guard at all.
    //
    // This applies to the `Clear` steps above too, and run 5 measured what
    // they would have been diffed against: an unset credential has TWO
    // readings — the empty string when nothing was ever written, the literal
    // `NULL` once it has been cleared — so skipping a redundant clear would be
    // possible and is deliberately not done for the same
    // preview-versus-apply reason. Round 4 recorded the opposite belief; the
    // measurement disproved it in the harmless direction.
    let mut steps = vec![
        ConfigfsStep::write(format!("{dir}/userid"), auth.userid.clone()),
        ConfigfsStep::secret(format!("{dir}/password"), auth.password.clone()),
        ConfigfsStep::Protect(format!("{dir}/password")),
    ];
    if auth.mutual {
        steps.push(ConfigfsStep::write(
            format!("{dir}/userid_mutual"),
            auth.mutual_userid.clone(),
        ));
        steps.push(ConfigfsStep::secret(
            format!("{dir}/password_mutual"),
            auth.mutual_password.clone(),
        ));
        steps.push(ConfigfsStep::Protect(format!("{dir}/password_mutual")));
    } else {
        steps.extend(clear_mutual());
    }
    steps
}

// =============================================================================
// NVMe-oF (nvmet)
// =============================================================================

/// The configfs tree of one NVMe-oF subsystem.
///
/// `observed` carries what the node already holds — see `observe_nvmet`. Two
/// kernel rules make it mandatory rather than an optimization:
///
///   * `nvmet_addr_*_store` calls `nvmet_is_port_enabled()` and returns
///     `-EACCES` — MEASURED with `addr_traddr`: "Disable port '247' before
///     changing attribute in nvmet_addr_traddr_store" — for a port ANY
///     subsystem is linked into. A port is node-wide, so the second subsystem
///     on `10.10.0.5:4420/tcp` would never come up if the plan rewrote the
///     address it is already listening on. An already-correct port is left
///     completely alone.
///   * `nvmet_ns_device_path_store` returns `-EBUSY` on an enabled namespace,
///     so a namespace whose backing device changed is disabled, rewritten and
///     enabled again; one that already matches is not touched.
///   * `attr_serial` and `attr_model` are both MEASURED as `-EINVAL` once a
///     controller has connected (obs. 16, 17; `subsys->subsys_discovered` in
///     the source) — so they are written only when they
///     differ from what the kernel holds.
///   * `allow_any_host` and the host links are mutually exclusive in BOTH
///     directions, and both were measured. Setting the flag while a host is
///     still linked is `-EINVAL` ("Can't set allow_any_host when explicit
///     hosts are set!", obs. 13), and LINKING a host while the flag is 1 is
///     refused too ("nvmet: can't add hosts when allow_any_host is set!",
///     obs. 31). That is what fixes the order of this whole function: stale
///     links are unlinked FIRST, `attr_allow_any_host` is written SECOND, and
///     the new links are created LAST. Taking authentication off a subsystem
///     needs the first half (or it fails halfway and the old host keeps its
///     key and its access); putting authentication ON needs the second (or the
///     link is refused and the subsystem stays open to everyone).
///
/// TRAP that shapes the rest of the function: nvmet keeps DH-HMAC-CHAP keys on
/// the HOST object (`hosts/<nqn>/dhchap_key`), which only exists because the
/// NQN is on the allowlist. There is therefore NO "authenticate but let anyone
/// connect" for NVMe-oF: `attr_allow_any_host = 1` bypasses the host objects
/// and with them every key. iSCSI's TPG-level CHAP has no counterpart here.
pub fn plan_nvmet(
    spec: &NvmetSubsystemSpec,
    observed: &NvmetObserved,
) -> Result<Vec<ConfigfsStep>, CatalogError> {
    validate_nvmet(spec)?;
    if observed.ports.len() != spec.portals.len() {
        return Err(invalid("one observed nvmet port per portal is required"));
    }
    if observed.namespaces.len() != spec.namespaces.len() {
        return Err(invalid(
            "one observed nvmet namespace per namespace is required",
        ));
    }
    let mut steps = Vec::new();
    let sub = format!("{NVMET_CONFIGFS}/subsystems/{}", spec.nqn);
    steps.push(ConfigfsStep::Mkdir(sub.clone()));

    // Hosts the allowlist dropped, FIRST: the link is what grants access, and
    // `attr_allow_any_host = 1` below is refused while any of them is still
    // there. MEASURED in both directions — with the link in place the write is
    // EINVAL, and after the unlink the same write succeeds. The host OBJECT
    // stays: it may be on a second subsystem's allowlist, and `remove_nvmet`
    // is what reaps an unreferenced one.
    for host in &observed.allowed_hosts {
        if spec.hosts.iter().any(|h| h.nqn == *host) {
            continue;
        }
        // The link is what grants access, so it goes first and unconditionally.
        steps.push(ConfigfsStep::Unlink(format!("{sub}/allowed_hosts/{host}")));
        // Then the OBJECT, but only when nothing else on this node links it.
        //
        // Two measurements decide this shape. A DH-HMAC-CHAP key cannot be
        // cleared in place at all (obs. 37: every sentinel is EINVAL, the
        // empty string is a silent no-op), and a recreated host object comes
        // back with fresh attributes (obs. 24) — so `rmdir` IS the clear, and
        // it is the same thing `remove_nvmet` has always done for a host
        // nobody references.
        //
        // The guard is the other half. `hosts/<nqn>/` is NODE-WIDE and the key
        // lives on it; a subsystem only symlinks to it. Two zvols exported to
        // one VMware host are two targets carrying the same NQN — one LUN per
        // target, §6.1 — so removing the object because it left OUR allowlist
        // took the other target's client offline at its next reconnect,
        // silently, with that row still green.
        //
        // MEASURED (obs. 45): `rmdir` of a host object that still holds a key
        // succeeds and the directory goes. And obs. 42/44: the kernel refuses
        // that same `rmdir` with EBUSY while ANYTHING still links the host —
        // so the guard below and the kernel hold the same invariant from two
        // sides, and a bug in `shared_hosts` costs a failed step rather than
        // another client's key.
        //
        // The verdict itself comes from the one authority (`HostVerdict`), the
        // same one the write path asks: this decision has been made
        // separately, one operation at a time, in three consecutive rounds.
        //
        // A host that IS shared keeps its object and its key: they are not
        // this target's to remove.
        match host_verdict(observed, host) {
            HostVerdict::Sole => steps.push(ConfigfsStep::Rmdir(format!(
                "{NVMET_CONFIGFS}/hosts/{host}"
            ))),
            HostVerdict::SharedAndAgrees | HostVerdict::SharedAndConflicts => {
                steps.push(ConfigfsStep::Note(format!(
                    "{host}: another target of this node still allows this host, so its object \
                     and its DH-HMAC-CHAP key stay — they are not this target's to remove"
                )));
            }
        }
    }

    if observed.serial != spec.serial {
        steps.push(ConfigfsStep::write(
            format!("{sub}/attr_serial"),
            spec.serial.clone(),
        ));
    }
    if observed.model != NVMET_MODEL {
        steps.push(ConfigfsStep::write(
            format!("{sub}/attr_model"),
            NVMET_MODEL,
        ));
    }
    let allow_any = if spec.allow_any_host { "1" } else { "0" };
    if observed.allow_any_host != allow_any {
        steps.push(ConfigfsStep::write(
            format!("{sub}/attr_allow_any_host"),
            allow_any,
        ));
    }

    // Namespaces the spec dropped.
    //
    // MEASURED, and NOT what this code was written to assume: `rmdir` of an
    // ENABLED namespace succeeds, even with a controller attached — the host
    // simply logs "rescanning namespaces" and the disk goes. nvmet reaches
    // `nvmet_ns_disable` through the configfs drop itself, so the kernel
    // enforces nothing here. The explicit `enable = 0` stays anyway, because
    // an ordered disable-then-remove is the difference between a namespace
    // that stops cleanly and one yanked mid-IO.
    //
    // The consequence is the part worth remembering: the kernel will NOT stop
    // this. Whatever protects a client writing to a namespace we are about to
    // drop has to live in the app — the desired state alone reaching here is
    // already the decision.
    for nsid in &observed.existing_namespaces {
        if spec.namespaces.iter().any(|ns| ns.index == *nsid) {
            continue;
        }
        let dir = format!("{sub}/namespaces/{nsid}");
        steps.push(ConfigfsStep::write(format!("{dir}/enable"), "0"));
        steps.push(ConfigfsStep::Rmdir(dir));
    }

    // ----- namespaces -----
    for (ns, state) in spec.namespaces.iter().zip(&observed.namespaces) {
        let dir = format!("{sub}/namespaces/{}", ns.index);
        let group = group_of(&spec.port_groups, ns.group_id)?;
        steps.push(ConfigfsStep::Mkdir(dir.clone()));
        if state.matches && state.enabled {
            // The namespace already serves exactly this device. Rewriting
            // `device_path` here would be refused with -EBUSY and take the
            // whole reconcile down with it.
            continue;
        }
        if state.enabled {
            steps.push(ConfigfsStep::write(format!("{dir}/enable"), "0"));
        }
        steps.push(ConfigfsStep::write(
            format!("{dir}/device_path"),
            ns.device_path.clone(),
        ));
        steps.push(ConfigfsStep::write(
            format!("{dir}/device_uuid"),
            ns.uuid.clone(),
        ));
        steps.push(ConfigfsStep::write(
            format!("{dir}/ana_grpid"),
            group.group_id.to_string(),
        ));
        steps.push(ConfigfsStep::write(format!("{dir}/enable"), "1"));
    }

    // ----- hosts and their keys -----
    for host in &spec.hosts {
        let dir = format!("{NVMET_CONFIGFS}/hosts/{}", host.nqn);
        // EVERY decision about this object goes through the one authority —
        // see `HostVerdict`. It is node-wide, it carries the key, and three
        // rounds in a row fixed one operation on it while leaving the others.
        match host_verdict(observed, &host.nqn) {
            HostVerdict::SharedAndConflicts => {
                // Refused, not reconciled. One object holds one key: writing
                // ours would take the other target's client offline at its
                // next reconnect, with that row still green and the only trace
                // in THIS target's job log. There is no third option the node
                // can carry out, so it declines to pick a loser.
                return Err(invalid(format!(
                    "another target of this node already allows host {} with different \
                     DH-HMAC-CHAP key material — nvmet keeps the key on the host object, which is \
                     shared node-wide, so both targets must use the same key (or a different host \
                     NQN)",
                    host.nqn
                )));
            }
            HostVerdict::SharedAndAgrees => {
                // Shared and already correct: the object needs nothing, and
                // must be given nothing. `Mkdir` is idempotent and the link
                // below is this subsystem's own.
                steps.push(ConfigfsStep::Note(format!(
                    "{}: this host is allowed by another target of this node too, and already \
                     holds exactly this key — the shared object is left untouched",
                    host.nqn
                )));
                steps.push(ConfigfsStep::Mkdir(dir.clone()));
            }
            HostVerdict::Sole => {
                // Key material this host holds that the spec no longer wants.
                // There is no way to write it away — obs. 37: every sentinel
                // is EINVAL and the empty string is a silent no-op — so the
                // object is REMOVED and recreated, which obs. 46 showed comes
                // back empty.
                //
                // This is what makes "the admin turned authentication off"
                // true in the kernel and not only in the UI: a host object
                // that keeps its key keeps DEMANDING it, so a subsystem the
                // wizard calls unauthenticated would refuse every client that
                // stopped sending one.
                let stale_key =
                    host.dhchap_key.is_empty() && observed.hosts_with_key.contains(&host.nqn);
                let stale_ctrl = host.dhchap_ctrl_key.is_empty()
                    && observed.hosts_with_ctrl_key.contains(&host.nqn);
                if stale_key || stale_ctrl {
                    // Unlink first if we hold it: MEASURED (obs. 42/44) that
                    // the kernel refuses `rmdir` of a host anything links,
                    // with EBUSY — so this is required, not cautious.
                    if observed.allowed_hosts.iter().any(|h| *h == host.nqn) {
                        steps.push(ConfigfsStep::Unlink(format!(
                            "{sub}/allowed_hosts/{}",
                            host.nqn
                        )));
                    }
                    steps.push(ConfigfsStep::Rmdir(dir.clone()));
                }
                steps.push(ConfigfsStep::Mkdir(dir.clone()));
                if !host.dhchap_key.is_empty() {
                    steps.push(ConfigfsStep::write(
                        format!("{dir}/dhchap_hash"),
                        host.dhchap_hash.clone(),
                    ));
                    steps.push(ConfigfsStep::write(
                        format!("{dir}/dhchap_dhgroup"),
                        host.dhchap_dhgroup.clone(),
                    ));
                    steps.push(ConfigfsStep::secret(
                        format!("{dir}/dhchap_key"),
                        host.dhchap_key.clone(),
                    ));
                    // The one credential in this app a local user can actually
                    // read out of the kernel — measured by performing that
                    // read, not by looking at the mode. Unconditional, and
                    // REQUIRED after a recreate: obs. 47 measured a fresh
                    // object's attribute back at mode 644.
                    steps.push(ConfigfsStep::Protect(format!("{dir}/dhchap_key")));
                    if !host.dhchap_ctrl_key.is_empty() {
                        steps.push(ConfigfsStep::secret(
                            format!("{dir}/dhchap_ctrl_key"),
                            host.dhchap_ctrl_key.clone(),
                        ));
                        steps.push(ConfigfsStep::Protect(format!("{dir}/dhchap_ctrl_key")));
                    }
                    // A controller key that is no longer wanted cannot be
                    // written away either (obs. 37); the recreate above is
                    // what removed it.
                }
                // An allowlist entry with no key at all is nvmet's "this host
                // may connect without authenticating" — §5.5's filter rather
                // than a login. Nothing to write.
            }
        }
        steps.push(ConfigfsStep::Symlink {
            link: format!("{sub}/allowed_hosts/{}", host.nqn),
            target: dir,
        });
    }

    // ----- ports -----
    for (portal, state) in spec.portals.iter().zip(&observed.ports) {
        let port = format!("{NVMET_CONFIGFS}/ports/{}", state.id);
        steps.push(ConfigfsStep::Mkdir(port.clone()));
        // A port that already carries this address and transport is left
        // untouched. Rewriting `addr_*` on a port a subsystem is linked into
        // returns -EACCES, and a port is node-wide: the second target on the
        // same address would never come up, and the first one's re-apply would
        // fail too.
        if !state.configured {
            steps.push(ConfigfsStep::write(format!("{port}/addr_adrfam"), "ipv4"));
            steps.push(ConfigfsStep::write(
                format!("{port}/addr_traddr"),
                portal.address.clone(),
            ));
            steps.push(ConfigfsStep::write(
                format!("{port}/addr_trsvcid"),
                portal.port.to_string(),
            ));
            // trtype last among the addresses: it is what makes the port ready
            // to be enabled by the first subsystem link.
            steps.push(ConfigfsStep::write(
                format!("{port}/addr_trtype"),
                portal.transport.clone(),
            ));
        }
        // ANA state is per (port, group) and stays writable on a live port, so
        // it is reconciled every time. Group 1 exists with the port; the rest
        // are created.
        for group in &spec.port_groups {
            let ana = format!("{port}/ana_groups/{}", group.group_id);
            if group.group_id != 1 {
                steps.push(ConfigfsStep::Mkdir(ana.clone()));
            }
            steps.push(ConfigfsStep::write(
                format!("{ana}/ana_state"),
                group.ana_state()?,
            ));
        }
        steps.push(ConfigfsStep::Symlink {
            link: format!("{port}/subsystems/{}", spec.nqn),
            target: sub.clone(),
        });
    }

    // ----- ports the subsystem must STOP answering on -----
    //
    // The nvmet half of the prune, and it is not symmetric with iSCSI by
    // accident of the tree: LIO's portal is a directory under the target
    // (`{tpg}/np/<addr>:<port>`), nvmet's is a node-wide port object with a
    // LINK into it. Removing the link is what takes this subsystem off that
    // address; the port itself stays.
    //
    // Without this, the one action the portal-drift alert asks for — "re-pick
    // the interface" — leaves the subsystem answering on BOTH addresses: the
    // new one and the one the admin just stopped choosing. Serving a raw disk
    // on an address nobody picked is the exact exposure the drift policy
    // exists to prevent, so the repair performing it would be worse than no
    // repair at all. A `tcp+rdma` → `tcp` edit has the same shape: the RDMA
    // port keeps serving while the UI chip says TCP.
    //
    // AFTER the links above, so a re-picked portal never has a window with
    // nothing listening. MEASURED for the analogous iSCSI case: removals on a
    // live target do not take it down.
    //
    // The empty port is LEFT BEHIND on purpose. A port is node-wide and may
    // carry other subsystems; deciding it is unused is `remove_nvmet`'s job,
    // which sees the whole tree. An empty port listens for nothing.
    let wanted_ports: Vec<u32> = observed.ports.iter().map(|p| p.id).collect();
    for id in &observed.linked_ports {
        if wanted_ports.contains(id) {
            continue;
        }
        steps.push(ConfigfsStep::Unlink(format!(
            "{NVMET_CONFIGFS}/ports/{id}/subsystems/{}",
            spec.nqn
        )));
    }

    Ok(steps)
}

fn group_of(groups: &[BlockPortGroup], id: u32) -> Result<&BlockPortGroup, CatalogError> {
    groups
        .iter()
        .find(|g| g.group_id == id)
        .ok_or_else(|| invalid(format!("port group {id} is not declared on this target")))
}

// =============================================================================
// Validation — the same rules on both sides of the channel
// =============================================================================

/// An iSCSI IQN or an NVMe NQN. Both are RFC-shaped names of at most 223
/// characters over the same restricted alphabet; neither may contain a path
/// separator, because it becomes a configfs DIRECTORY NAME.
fn validate_target_name(kind: &str, name: &str) -> Result<(), CatalogError> {
    if name.is_empty() || name.len() > 223 {
        return Err(invalid(format!("{kind} must be 1..=223 characters")));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b":.-".contains(&b))
    {
        return Err(invalid(format!(
            "{kind} '{name}' may only hold lowercase letters, digits, ':', '.' and '-'"
        )));
    }
    if name.starts_with('.') || name.contains("..") {
        return Err(invalid(format!("{kind} '{name}' is not a valid name")));
    }
    Ok(())
}

/// `iqn.YYYY-MM.<reverse-domain>[:unique]`, `eui.<16 hex>` or `naa.<hex>` —
/// the three forms RFC 3720 §3.2.6 defines.
pub fn validate_iqn(iqn: &str) -> Result<(), CatalogError> {
    validate_target_name("IQN", iqn)?;
    if iqn.starts_with("iqn.") || iqn.starts_with("eui.") || iqn.starts_with("naa.") {
        Ok(())
    } else {
        Err(invalid(format!(
            "IQN '{iqn}' must start with 'iqn.', 'eui.' or 'naa.'"
        )))
    }
}

/// `nqn.YYYY-MM.<reverse-domain>:unique` (NVMe base spec §4.5).
pub fn validate_nqn(nqn: &str) -> Result<(), CatalogError> {
    validate_target_name("NQN", nqn)?;
    if nqn.starts_with("nqn.") {
        Ok(())
    } else {
        Err(invalid(format!("NQN '{nqn}' must start with 'nqn.'")))
    }
}

/// The object name of a backstore: it becomes a configfs directory, so the
/// same shape rules as a share name, plus the dot LIO allows.
pub fn validate_backstore_name(name: &str) -> Result<(), CatalogError> {
    if name.is_empty() || name.len() > 64 {
        return Err(invalid("backstore name must be 1..=64 characters"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    {
        return Err(invalid(format!(
            "backstore name '{name}' may only hold letters, digits, '_', '-' and '.'"
        )));
    }
    if name.starts_with('.') {
        return Err(invalid("backstore name may not start with '.'"));
    }
    Ok(())
}

/// What the catalog is willing to export as a raw disk.
///
/// A block export hands the client the device with no file permissions in the
/// way, so the boundary has to be narrow: a ZFS volume under `/dev/zvol`, and
/// nothing else. A whole disk, a partition, `/dev/sda` — all refused here,
/// because exporting a pool member would corrupt the pool.
///
/// §5.5 also names a file as a possible LUN source. This slice does NOT
/// implement it: there is no wizard step for it and no way to reach it, so the
/// LIO fileio plugin is out rather than sitting here as unreachable code.
pub fn validate_backing_device(path: &str) -> Result<(), CatalogError> {
    if path.contains("..") || path.contains('\0') || !path.starts_with('/') {
        return Err(invalid(format!("'{path}' is not an absolute clean path")));
    }
    if !path.starts_with("/dev/zvol/") {
        return Err(invalid(format!(
            "only ZFS volumes under /dev/zvol may be exported, '{path}' is not one"
        )));
    }
    if path.trim_end_matches('/').matches('/').count() < 4 {
        return Err(invalid(format!("'{path}' is not <pool>/<volume>")));
    }
    Ok(())
}

/// The address a portal binds to. IPv4 literals only, `0.0.0.0` included: a
/// hostname would make the bound address depend on DNS at apply time, and the
/// UI promises the admin an address it showed them.
pub fn validate_portal_address(address: &str) -> Result<(), CatalogError> {
    match address.parse::<std::net::Ipv4Addr>() {
        Ok(_) => Ok(()),
        Err(_) => Err(invalid(format!(
            "'{address}' is not an IPv4 address (IPv6 portals are not offered yet)"
        ))),
    }
}

pub fn validate_portal_port(port: u32) -> Result<(), CatalogError> {
    if (1..=65535).contains(&port) {
        Ok(())
    } else {
        Err(invalid(format!("port {port} is out of range")))
    }
}

/// A CHAP user name. It travels in the login PDU as text.
pub fn validate_chap_user(user: &str) -> Result<(), CatalogError> {
    if user.is_empty() || user.len() > 255 {
        return Err(invalid("the CHAP user must be 1..=255 characters"));
    }
    if !user
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_-.:@".contains(&b))
    {
        return Err(invalid(
            "the CHAP user may only hold letters, digits, '_', '-', '.', ':' and '@'",
        ));
    }
    Ok(())
}

/// A CHAP secret. RFC 3720 §8.2.1 requires at least 96 bits (12 bytes) of
/// secret for CHAP to be worth anything, and Windows' initiator refuses
/// anything longer than 16 — the wizard says so, this refuses the short ones.
pub fn validate_chap_secret(secret: &str) -> Result<(), CatalogError> {
    if secret.len() < 12 || secret.len() > 255 {
        return Err(invalid(
            "a CHAP secret must be 12..=255 characters (RFC 3720 asks for at least 96 bits)",
        ));
    }
    if !secret.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(invalid(
            "a CHAP secret must be printable ASCII without spaces",
        ));
    }
    Ok(())
}

/// A DH-HMAC-CHAP key as `nvme gen-dhchap-key` prints it. The kernel parses
/// the whole `DHHC-1:<hmac>:<base64>:` form and verifies its CRC, so the shape
/// check here only keeps a typo from reaching configfs as a root write.
pub fn validate_dhchap_key(key: &str) -> Result<(), CatalogError> {
    if !key.starts_with("DHHC-1:") {
        return Err(invalid(
            "a DH-HMAC-CHAP key must be a 'DHHC-1:…' value from `nvme gen-dhchap-key`",
        ));
    }
    if key.len() > 1024 {
        return Err(invalid("the DH-HMAC-CHAP key is too long"));
    }
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b":+/=-_".contains(&b))
    {
        return Err(invalid("the DH-HMAC-CHAP key holds a character it cannot"));
    }
    Ok(())
}

/// The hashes and DH groups nvmet accepts (`drivers/nvme/common/auth.c`).
pub const DHCHAP_HASHES: &[&str] = &["hmac(sha256)", "hmac(sha384)", "hmac(sha512)"];
pub const DHCHAP_DHGROUPS: &[&str] = &[
    "null", "ffdhe2048", "ffdhe3072", "ffdhe4096", "ffdhe6144", "ffdhe8192",
];

fn validate_port_groups(groups: &[BlockPortGroup]) -> Result<(), CatalogError> {
    if groups.is_empty() {
        return Err(invalid("a target needs at least one port group"));
    }
    for group in groups {
        if !(1..=65535).contains(&group.group_id) {
            return Err(invalid(format!(
                "port group id {} is out of range",
                group.group_id
            )));
        }
        group.lio_state()?;
        if groups
            .iter()
            .filter(|g| g.group_id == group.group_id)
            .count()
            != 1
        {
            return Err(invalid(format!(
                "port group {} is declared twice",
                group.group_id
            )));
        }
    }
    Ok(())
}

fn validate_luns(luns: &[BlockLun], groups: &[BlockPortGroup]) -> Result<(), CatalogError> {
    if luns.is_empty() {
        return Err(invalid("a target needs at least one LUN"));
    }
    for lun in luns {
        validate_backstore_name(&lun.name)?;
        validate_backing_device(&lun.device_path)?;
        if lun.uuid.is_empty() || lun.uuid.len() > 254 {
            return Err(invalid("a LUN needs a stable identity of 1..=254 characters"));
        }
        group_of(groups, lun.group_id)?;
        if luns.iter().filter(|l| l.index == lun.index).count() != 1 {
            return Err(invalid(format!("LUN {} is declared twice", lun.index)));
        }
    }
    Ok(())
}

fn validate_portals(portals: &[BlockPortal], allowed: &[&str]) -> Result<(), CatalogError> {
    if portals.is_empty() {
        return Err(invalid("a target needs at least one portal"));
    }
    for portal in portals {
        validate_portal_address(&portal.address)?;
        validate_portal_port(portal.port)?;
        if !allowed.contains(&portal.transport.as_str()) {
            return Err(invalid(format!(
                "'{}' is not a transport of this protocol",
                portal.transport
            )));
        }
    }
    Ok(())
}

pub fn validate_iscsi(spec: &IscsiTargetSpec) -> Result<(), CatalogError> {
    validate_iqn(&spec.iqn)?;
    validate_port_groups(&spec.port_groups)?;
    validate_luns(&spec.luns, &spec.port_groups)?;
    validate_portals(&spec.portals, &["tcp", "iser"])?;
    for initiator in &spec.initiators {
        validate_iqn(initiator)?;
    }
    if spec.auth.enabled {
        validate_chap_user(&spec.auth.userid)?;
        validate_chap_secret(&spec.auth.password)?;
        if spec.auth.mutual {
            validate_chap_user(&spec.auth.mutual_userid)?;
            validate_chap_secret(&spec.auth.mutual_password)?;
            if spec.auth.mutual_password == spec.auth.password {
                return Err(invalid(
                    "the mutual CHAP secret must differ from the initiator's",
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_nvmet(spec: &NvmetSubsystemSpec) -> Result<(), CatalogError> {
    validate_nqn(&spec.nqn)?;
    validate_port_groups(&spec.port_groups)?;
    for group in &spec.port_groups {
        group.ana_state()?;
        if group.preferred {
            // Not dropped quietly: a UI that showed "preferred" and a target
            // that ignores it is exactly the kind of lie this codebase does
            // not ship.
            return Err(invalid(
                "NVMe ANA has no preferred-path flag — express the preference with the group state",
            ));
        }
    }
    validate_luns(&spec.namespaces, &spec.port_groups)?;
    for ns in &spec.namespaces {
        if ns.index == 0 {
            return Err(invalid("an NVMe namespace id starts at 1"));
        }
    }
    validate_portals(&spec.portals, &["tcp", "rdma"])?;
    if spec.serial.is_empty() || spec.serial.len() > 20 {
        return Err(invalid("the subsystem serial must be 1..=20 characters"));
    }
    for host in &spec.hosts {
        validate_nqn(&host.nqn)?;
        if host.dhchap_key.is_empty() {
            continue;
        }
        validate_dhchap_key(&host.dhchap_key)?;
        if !host.dhchap_ctrl_key.is_empty() {
            validate_dhchap_key(&host.dhchap_ctrl_key)?;
        }
        if !DHCHAP_HASHES.contains(&host.dhchap_hash.as_str()) {
            return Err(invalid(format!(
                "'{}' is not a DH-HMAC-CHAP hash nvmet accepts",
                host.dhchap_hash
            )));
        }
        if !DHCHAP_DHGROUPS.contains(&host.dhchap_dhgroup.as_str()) {
            return Err(invalid(format!(
                "'{}' is not a DH-HMAC-CHAP DH group nvmet accepts",
                host.dhchap_dhgroup
            )));
        }
    }
    if spec.allow_any_host && !spec.hosts.is_empty() {
        // Two different failures, one rule.
        //
        // With KEYS on the hosts the kernel would take both and silently let
        // anyone in: `attr_allow_any_host = 1` bypasses the host objects the
        // keys live on, so the subsystem would authenticate nobody while the
        // UI said it authenticates.
        //
        // With hosts and NO keys the kernel refuses outright —
        // `nvmet_allowed_hosts_allow_link`: "can't add hosts when
        // allow_any_host is set!" — and the refusal lands halfway through the
        // apply, after the subsystem exists. The core cannot build this today
        // (`allow_any_host` is exactly `initiators.is_empty()`), but the
        // catalog rules are the layer that has to catch it: they are what a
        // spec arriving on the helper's stdin is judged by, and the whole
        // point of judging it there is not to trust the caller.
        return Err(invalid(
            "'allow any host' and an explicit host list are mutually exclusive — nvmet refuses \
             the link, and with DH-HMAC-CHAP keys it would bypass the host objects they live on",
        ));
    }
    Ok(())
}

// =============================================================================
// Execution — the root side. Every function takes the configfs subtree as an
// argument so the removal walkers can be exercised against a real directory
// tree in a test instead of against this host's kernel.
// =============================================================================

/// Writes one configfs attribute.
///
/// TRAP: a configfs attribute takes ONE write of the exact value, and the value
/// arrives verbatim — a trailing newline becomes part of LIO's auth strings, so
/// `userid` would end up as `vmware01\n` and no initiator would ever match.
///
/// A ZERO-LENGTH value is refused here rather than written. `configfs_write_iter`
/// calls `flush_write_buffer` only for `len > 0`, so an empty write never
/// reaches the attribute's store method and silently changes nothing; `O_TRUNC`
/// does not help either, because it goes to `simple_setattr`. Clearing a
/// credential is `ConfigfsStep::Clear`, which carries the subsystem's own
/// sentinel — this refusal is what stops that rule being bypassed by accident.
fn write_attr(path: &Path, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!(
            "{}: an empty configfs write never reaches the attribute — use a Clear step",
            path.display()
        ));
    }
    // `truncate` is what targetcli's `open(path, 'w')` does. configfs ignores
    // it (the size goes to `simple_setattr`, which no attribute sees), so it
    // changes nothing there — but it keeps the shorter of two values from
    // leaving a tail behind on any other filesystem.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let written = file
        .write(value.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    if written != value.len() {
        return Err(format!(
            "{} took {written} of {} bytes",
            path.display(),
            value.len()
        ));
    }
    Ok(())
}

/// Takes a credential attribute's mode down to 0600 and READS THE MODE BACK.
///
/// Returns the sentence for the job log when the attribute is still readable
/// by somebody other than root, and `None` when it is not.
///
/// WHY it exists: LIO gates its own auth attributes behind
/// `capable(CAP_SYS_ADMIN)` in their show methods — measured, an ordinary
/// account gets EPERM on `{tpg}/auth/password` while reading
/// `{tpg}/param/AuthMethod` beside it just fine — but nvmet's
/// `nvmet_host_dhchap_key_show` has no such gate. MEASURED, four ways, on a
/// live node:
///   * `hosts/<nqn>/dhchap_key` is created `-rw-r--r-- root:root`, and an
///     unprivileged local account really reads the key out of it — the
///     exposure was confirmed by performing the read, not by reading the mode;
///   * `chmod 600` on a configfs attribute takes, and that same account can no
///     longer read it afterwards;
///   * the mode survives the next write to the attribute;
///   * it does not survive the object being recreated.
///
/// WHY it verifies instead of trusting the syscall: this chmod is the ONLY
/// thing between a DH-HMAC-CHAP key and every local user of the node, the UI
/// tells the admin in five languages that it happened, and a `let _ =` would
/// have made that sentence a claim nobody checked. It still does not fail the
/// apply — a target that stops serving is not the right answer to a file mode
/// — but the divergence is reported, which is the difference between an admin
/// who knows and one who does not.
fn protect_attr(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        return Some(format!(
            "{}: the file mode could not be tightened to 0600 ({e}) — treat this credential as \
             readable by every local user of this node",
            path.display()
        ));
    }
    // Read back rather than believe the syscall: a filesystem may accept a
    // `chmod` and keep its own mode, and this is exactly the claim that must
    // not be made on trust.
    let mode = match std::fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o777,
        Err(e) => {
            return Some(format!(
                "{}: the file mode was set but could not be read back ({e}) — this node cannot \
                 confirm the credential stopped being world-readable",
                path.display()
            ))
        }
    };
    (mode & 0o077 != 0).then(|| {
        format!(
            "{}: the file mode is still {mode:04o} after the chmod — this credential is readable \
             by other users of this node",
            path.display()
        )
    })
}

fn mkdir(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Already there: the apply is a reconcile, and an object that survived
        // the last run is the desired state, not an error.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("cannot create {}: {e}", path.display())),
    }
}

fn link(link_path: &Path, target: &Path) -> Result<(), String> {
    if let Ok(existing) = std::fs::read_link(link_path) {
        if existing == target {
            return Ok(());
        }
        std::fs::remove_file(link_path)
            .map_err(|e| format!("cannot replace {}: {e}", link_path.display()))?;
    }
    std::os::unix::fs::symlink(target, link_path).map_err(|e| {
        format!(
            "cannot link {} -> {}: {e}",
            link_path.display(),
            target.display()
        )
    })
}

/// Runs a rendered plan. Returns the lines that have to reach the job log
/// because something the plan promised did not fully happen — today that is
/// exactly the credential-mode check. The plan itself is logged by the caller
/// through `render`, which redacts, and the step count is `steps.len()`.
///
/// A warning is not an error on purpose: a target that refuses to serve is the
/// wrong answer to a file mode, and an admin who is not told is worse than one
/// who is told late.
pub fn apply_plan(steps: &[ConfigfsStep]) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    for step in steps {
        match step {
            ConfigfsStep::Mkdir(path) => mkdir(Path::new(path))?,
            ConfigfsStep::Write { path, value, .. } => write_attr(Path::new(path), value)?,
            ConfigfsStep::Protect(path) => {
                if let Some(warning) = protect_attr(Path::new(path)) {
                    warnings.push(warning);
                }
            }
            // Says something, does nothing.
            ConfigfsStep::Note(_) => {}
            // The sentinel is the value: LIO reads `NULL` as "unset" (obs. 7),
            // and it is a non-empty write, which is the only kind configfs
            // acts on. This verb is LIO's alone now — nvmet has no value that
            // means "no key", so it clears by removing the host object.
            ConfigfsStep::Clear { path, sentinel } => {
                write_attr(Path::new(path), sentinel)?;
            }
            ConfigfsStep::Symlink { link: l, target } => link(Path::new(l), Path::new(target))?,
            // An object that is already gone is the desired state, the same
            // way an existing one is for `Mkdir`. Anything else is fatal:
            // "the initiator lost its access" must not be reported for a
            // removal the kernel refused.
            ConfigfsStep::Unlink(path) => unlink(Path::new(path))?,
            ConfigfsStep::Rmdir(path) => remove_object(Path::new(path))?,
        }
    }
    Ok(warnings)
}

fn unlink(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot unlink {}: {e}", path.display())),
    }
}

/// Removes one configfs object as a PLAN step, and fails loudly when it stays.
///
/// The ENOTEMPTY branch is the same accommodation `rmdir` makes for the
/// teardown walkers: configfs deletes an item's attribute files with the item,
/// so it never happens there, while an ordinary filesystem — the one the tests
/// build their trees on — keeps them as real files.
fn remove_object(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => {
            clear_plain_children(path);
            std::fs::remove_dir(path)
                .map_err(|e| format!("cannot remove {}: {e}", path.display()))
        }
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// Directory entries of `dir`, sorted, or an empty list when it does not
/// exist. Sorted because a plan and a teardown must not depend on the order
/// the kernel happens to hand entries back in.
fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    out.sort();
    out
}

/// Removes one configfs object.
///
/// TRAP: configfs deletes an item's ATTRIBUTE files and its DEFAULT GROUPS
/// together with the item, so `rmdir` is the whole operation there — and
/// `unlink` on an attribute returns EPERM, so there is nothing to clear first.
/// On an ordinary filesystem those are real files and real directories and
/// `rmdir` alone fails with ENOTEMPTY. Clearing them on that one error is a
/// no-op against the kernel and correct everywhere else, which is what lets
/// the teardown walkers be tested against a directory tree instead of against
/// this host's LIO.
fn rmdir(path: &Path, log: &mut Vec<String>) {
    match std::fs::remove_dir(path) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => {
            clear_plain_children(path);
            if let Err(e) = std::fs::remove_dir(path) {
                log.push(format!("{} not removed: {e}", path.display()));
            }
        }
        Err(e) => log.push(format!("{} not removed: {e}", path.display())),
    }
}

/// The attribute files and default groups of one object, on a filesystem that
/// keeps them as ordinary entries.
fn clear_plain_children(path: &Path) {
    for entry in entries(path) {
        if entry.is_dir() && !entry.is_symlink() {
            clear_plain_children(&entry);
            let _ = std::fs::remove_dir(&entry);
        } else {
            let _ = std::fs::remove_file(&entry);
        }
    }
}

/// Takes one app-created iSCSI target out of LIO, backstores included.
///
/// Only the named target is touched: a node that also runs a hand-made target
/// keeps it. The backstores are found by reading the LUN symlinks rather than
/// by reconstructing their names, so a target created by an older build is
/// still removed completely.
pub fn remove_iscsi(root: &Path, iqn: &str) -> Result<Vec<String>, String> {
    validate_iqn(iqn).map_err(|e| e.to_string())?;
    let target = root.join("iscsi").join(iqn);
    let mut log = Vec::new();
    if !target.is_dir() {
        log.push(format!("{iqn}: not present in configfs"));
        return Ok(log);
    }
    let tpg = target.join("tpgt_1");
    if tpg.is_dir() {
        // Stop accepting LOGINS first. Not an ordering requirement — MEASURED
        // that every removal step below (unlink, `rmdir` of a mapped LUN, of a
        // TPG LUN, of an ACL, of an `np`) succeeds on a live TPG that stays
        // enabled afterwards, which is exactly what `prune_iscsi` relies on.
        // What this buys is that no initiator can log in DURING the teardown
        // and get a target that is halfway gone. The client that is already
        // connected loses its disk either way: the whole target is going.
        if let Err(e) = write_attr(&tpg.join("enable"), "0") {
            log.push(format!("{iqn}: not disabled: {e}"));
        }

        for acl in entries(&tpg.join("acls")) {
            for mapped in entries(&acl) {
                if !mapped.is_dir() {
                    continue;
                }
                for entry in entries(&mapped) {
                    if entry.is_symlink() {
                        let _ = std::fs::remove_file(&entry);
                    }
                }
                rmdir(&mapped, &mut log);
            }
            rmdir(&acl, &mut log);
        }

        for np in entries(&tpg.join("np")) {
            rmdir(&np, &mut log);
        }

        let mut backstores: Vec<PathBuf> = Vec::new();
        for lun in entries(&tpg.join("lun")) {
            for entry in entries(&lun) {
                if !entry.is_symlink() {
                    continue;
                }
                if let Ok(dev) = std::fs::read_link(&entry) {
                    backstores.push(dev);
                }
                let _ = std::fs::remove_file(&entry);
            }
            rmdir(&lun, &mut log);
        }
        rmdir(&tpg, &mut log);
        rmdir(&target, &mut log);

        for dev in backstores {
            // A user-created target port group is a child item and has to go
            // before its device; `default_tg_pt_gp` belongs to the device and
            // goes with it.
            for group in entries(&dev.join("alua")) {
                if group.file_name().is_some_and(|n| n == "default_tg_pt_gp") {
                    continue;
                }
                rmdir(&group, &mut log);
            }
            rmdir(&dev, &mut log);
            log.push(format!("backstore {} removed", dev.display()));
        }
    } else {
        rmdir(&target, &mut log);
    }
    log.push(format!("iSCSI target {iqn} removed"));
    Ok(log)
}

/// Takes one app-created nvmet subsystem out of the kernel.
///
/// A PORT is only removed when our subsystem was the last one on it: ports are
/// node-wide (see `observe_nvmet`), so tearing one down while another
/// subsystem still answers there would take a target down that nobody asked
/// about. A HOST object is removed on the same rule — it may be on the
/// allowlist of a second subsystem.
pub fn remove_nvmet(root: &Path, nqn: &str) -> Result<Vec<String>, String> {
    validate_nqn(nqn).map_err(|e| e.to_string())?;
    let sub = root.join("subsystems").join(nqn);
    let mut log = Vec::new();
    if !sub.is_dir() {
        log.push(format!("{nqn}: not present in configfs"));
        return Ok(log);
    }

    for port in entries(&root.join("ports")) {
        let subsystems = port.join("subsystems");
        let ours = subsystems.join(nqn);
        if !ours.exists() {
            continue;
        }
        let _ = std::fs::remove_file(&ours);
        if entries(&subsystems).is_empty() {
            for group in entries(&port.join("ana_groups")) {
                if group.file_name().is_some_and(|n| n == "1") {
                    continue;
                }
                rmdir(&group, &mut log);
            }
            rmdir(&port, &mut log);
            log.push(format!("nvmet port {} removed", port.display()));
        }
    }

    let mut hosts: Vec<PathBuf> = Vec::new();
    for allowed in entries(&sub.join("allowed_hosts")) {
        if let Ok(host) = std::fs::read_link(&allowed) {
            hosts.push(host);
        }
        let _ = std::fs::remove_file(&allowed);
    }

    for ns in entries(&sub.join("namespaces")) {
        if let Err(e) = write_attr(&ns.join("enable"), "0") {
            log.push(format!("{}: not disabled: {e}", ns.display()));
        }
        rmdir(&ns, &mut log);
    }
    rmdir(&sub, &mut log);

    for host in hosts {
        let name = host.file_name().map(|n| n.to_owned()).unwrap_or_default();
        // `symlink_metadata`, not `exists()`, for the reason `observe_nvmet`
        // gives at its own walk: the question is "does this subsystem still
        // hold a link to that host", and a DANGLING link still means it does.
        // `exists()` follows the link and answers "no" for it, which would
        // make this reap an object another subsystem is attached to.
        let still_used = entries(&root.join("subsystems")).into_iter().any(|s| {
            std::fs::symlink_metadata(s.join("allowed_hosts").join(&name)).is_ok()
        });
        if still_used {
            continue;
        }
        rmdir(&host, &mut log);
    }
    log.push(format!("NVMe-oF subsystem {nqn} removed"));
    Ok(log)
}

// =============================================================================
// NVMe-oF sessions (debugfs, not configfs)
// =============================================================================

/// Where the kernel publishes the controllers attached to an nvmet subsystem.
///
/// TRAP — this is NOT configfs, and that is the whole reason reading it is a
/// catalog entry instead of an ordinary read the way LIO's `dynamic_sessions`
/// is. configfs carries the DESIRED state of a subsystem and says nothing
/// about who is attached to it; nvmet keeps its live controllers in debugfs,
/// which needs `CONFIG_NVME_TARGET_DEBUGFS` (kernel 6.11 and newer — the first
/// commit of `drivers/nvme/target/debugfs.c` is from June 2024) and sits under
/// a mountpoint that is `0700 root:root`.
///
/// Layout, MEASURED with a controller attached over TCP:
/// `<root>/<subsystem nqn>/ctrl1/` holding exactly `hostnqn`, `host_traddr`,
/// `kato`, `port`, `state`, `tls_concat`, `tls_key`. The controller directory
/// carries the `ctrl` prefix — `snprintf(name, sizeof(name), "ctrl%d",
/// ctrl->cntlid)` — and since 6.16 a subsystem also has `ns<N>` directories,
/// which are not associations and are skipped by the same rule.
pub const NVMET_DEBUGFS: &str = "/sys/kernel/debug/nvmet";

/// One controller attached to a subsystem right now — an NVMe-oF association,
/// which is what "session" means for this protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NvmetController {
    /// The controller id the kernel gave this association — the digits of the
    /// `ctrl<N>` directory.
    pub cntlid: String,
    /// The NQN the host declared, with the same caveat as every other NQN in
    /// this file: the client picks it for itself, so it names a session and
    /// authenticates nothing.
    pub hostnqn: String,
    /// The transport address the controller came FROM (`host_traddr`), which
    /// is the one thing about a session the client cannot simply assert.
    /// Empty when the kernel published none.
    pub host_traddr: String,
    /// nvmet's own port index the controller came in on.
    pub port: String,
    /// The controller state VERBATIM. Measured on a live association it reads
    /// `ready`; no other value is documented anywhere, so nothing in this
    /// codebase may filter or branch on the string — it is shown, and that is
    /// all. A controller in a state this build has never seen is still a
    /// controller, and dropping it would understate the blast radius.
    pub state: String,
}

/// What one read of the debugfs tree found.
///
/// `available: false` is the honest answer for "this node cannot know", and it
/// is NOT the same as an empty map: a kernel built without
/// `CONFIG_NVME_TARGET_DEBUGFS`, or a node whose debugfs is not mounted, must
/// never be reported as a subsystem that simply has nobody connected to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NvmetSessions {
    pub available: bool,
    /// Why not, when `available` is false. The UI shows it as the reason the
    /// count is a dash rather than a number.
    #[serde(default)]
    pub reason: String,
    /// Subsystem NQN -> its controllers, in controller-id order. Empty unless
    /// `available`.
    #[serde(default)]
    pub controllers: BTreeMap<String, Vec<NvmetController>>,
}

impl NvmetSessions {
    /// The "we cannot know" answer, with the sentence that explains it.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: reason.into(),
            controllers: BTreeMap::new(),
        }
    }
}

/// Reads every attached controller out of the nvmet debugfs tree.
///
/// Takes the root as an argument so the walk is testable on a directory tree a
/// test builds — the same reason every other function in this file renders a
/// plan instead of touching the kernel directly.
///
/// A tree that is not there is reported as unavailable, never as "no sessions".
pub fn read_nvmet_sessions(root: &Path) -> NvmetSessions {
    if !root.is_dir() {
        return NvmetSessions::unavailable(format!(
            "{} does not exist — this kernel has no CONFIG_NVME_TARGET_DEBUGFS, or debugfs is not mounted",
            root.display()
        ));
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) => {
            return NvmetSessions::unavailable(format!("{} cannot be read: {e}", root.display()))
        }
    };
    let mut controllers: BTreeMap<String, Vec<NvmetController>> = BTreeMap::new();
    for subsystem in entries.flatten() {
        let dir = subsystem.path();
        if !dir.is_dir() {
            continue;
        }
        // A subsystem directory is named by its NQN, and the NQN is also the
        // key the caller matches a target's `wwn` against. A name this process
        // cannot represent would silently never match, and the answer would be
        // a confident "nobody is connected" — so it makes the whole read
        // unknown instead. Unreachable today (an NQN is restricted ASCII); it
        // is the same class of bug as reading the wrong directory name.
        let Some(nqn) = subsystem.file_name().to_str().map(str::to_owned) else {
            return NvmetSessions::unavailable(format!(
                "{} holds a subsystem directory whose name is not valid UTF-8, so its \
                 controllers cannot be matched to a target",
                root.display()
            ));
        };
        // A subsystem directory this node could not read is UNKNOWN, not
        // empty — the same rule as the non-UTF8 name above, and for the same
        // reason: the alternative answer is a confident "nobody is connected",
        // printed as a `0` in the delete dialog's blast radius, for a
        // subsystem that may have three hosts writing to it. A read that
        // failed is not a read that found nothing.
        let children = match std::fs::read_dir(&dir) {
            Ok(children) => children,
            Err(e) => {
                return NvmetSessions::unavailable(format!(
                    "{} cannot be read: {e} — this node cannot tell how many hosts are \
                     attached to {nqn}",
                    dir.display()
                ))
            }
        };
        let mut found = Vec::new();
        for child in children.flatten() {
            let name = child.file_name().to_string_lossy().into_owned();
            // `debugfs.c`: `snprintf(name, sizeof(name), "ctrl%d",
            // ctrl->cntlid)`. Everything else under a subsystem —
            // nvmet's own attribute files, and the `ns<N>` directories a
            // 6.16 kernel adds — is not an association, and counting one
            // would inflate the number the delete dialog states its blast
            // radius with.
            let Some(cntlid) = name.strip_prefix("ctrl") else {
                continue;
            };
            if !child.path().is_dir()
                || cntlid.is_empty()
                || !cntlid.bytes().all(|b| b.is_ascii_digit())
            {
                continue;
            }
            found.push(NvmetController {
                cntlid: cntlid.to_string(),
                hostnqn: debugfs_attr(&child.path(), "hostnqn"),
                host_traddr: debugfs_attr(&child.path(), "host_traddr"),
                port: debugfs_attr(&child.path(), "port"),
                state: debugfs_attr(&child.path(), "state"),
            });
        }
        // By id as a NUMBER: controller 10 comes after controller 2.
        found.sort_by_key(|c| c.cntlid.parse::<u64>().unwrap_or(u64::MAX));
        controllers.insert(nqn, found);
    }
    NvmetSessions {
        available: true,
        reason: String::new(),
        controllers,
    }
}

/// One debugfs attribute, or an empty string. A file the running kernel does
/// not publish is a missing field, not a failed read: this tree grew its
/// attributes over several releases.
fn debugfs_attr(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u32, state: &str) -> BlockPortGroup {
        BlockPortGroup {
            group_id: id,
            state: state.to_string(),
            preferred: false,
        }
    }

    fn zvol_lun(index: u32) -> BlockLun {
        BlockLun {
            index,
            name: format!("tentanas_vm_store_lun{index}"),
            device_path: "/dev/zvol/tank/vm-store".to_string(),
            uuid: "0191f2c0-0000-7000-8000-000000000001".to_string(),
            group_id: 1,
        }
    }

    fn iscsi(auth: IscsiAuth, initiators: Vec<String>) -> IscsiTargetSpec {
        IscsiTargetSpec {
            iqn: "iqn.2026-09.pl.euvic:helios.vm-store".to_string(),
            luns: vec![zvol_lun(0)],
            portals: vec![BlockPortal {
                address: "10.10.0.5".to_string(),
                port: ISCSI_PORT,
                transport: "tcp".to_string(),
            }],
            port_groups: vec![group(1, "optimized")],
            auth,
            initiators,
        }
    }

    fn nvmet(hosts: Vec<NvmetHost>, allow_any: bool) -> NvmetSubsystemSpec {
        NvmetSubsystemSpec {
            nqn: "nqn.2026-09.pl.euvic:helios.scratch".to_string(),
            serial: "TN0000000001".to_string(),
            namespaces: vec![BlockLun {
                index: 1,
                device_path: "/dev/zvol/fast/scratch".to_string(),
                ..zvol_lun(1)
            }],
            portals: vec![BlockPortal {
                address: "10.10.0.5".to_string(),
                port: NVME_PORT,
                transport: "tcp".to_string(),
            }],
            port_groups: vec![group(1, "optimized")],
            hosts,
            allow_any_host: allow_any,
        }
    }

    /// A node where nothing of this target exists yet — the FIRST apply: no
    /// backstore, no ACL, no portal, no LUN.
    fn fresh() -> IscsiObserved {
        fresh_for(&iscsi(IscsiAuth::default(), vec![]))
    }

    /// The same, shaped for a spec with any number of LUNs.
    fn fresh_for(spec: &IscsiTargetSpec) -> IscsiObserved {
        IscsiObserved {
            devices: spec
                .luns
                .iter()
                .map(|lun| IscsiDeviceObserved {
                    name: lun.name.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// `ports` new port slots numbered from 1 and `namespaces` namespaces that
    /// do not exist yet — the first apply of an NVMe-oF subsystem.
    fn fresh_nvmet(ports: u32, namespaces: u32) -> NvmetObserved {
        NvmetObserved {
            ports: (1..=ports)
                .map(|id| NvmetPortObserved {
                    id,
                    configured: false,
                })
                .collect(),
            namespaces: (1..=namespaces)
                .map(|nsid| NvmetNsObserved {
                    nsid,
                    enabled: false,
                    matches: false,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn mutual_chap() -> IscsiAuth {
        IscsiAuth {
            enabled: true,
            mutual: true,
            userid: "vmware01".to_string(),
            password: "sekret-inicjatora".to_string(),
            mutual_userid: "helios".to_string(),
            mutual_password: "sekret-targetu-1".to_string(),
        }
    }

    #[test]
    fn a_target_without_chap_renders_an_open_tpg_and_says_so_in_every_line() {
        let plan = plan_iscsi(&iscsi(IscsiAuth::default(), vec![]), &fresh()).expect("plan");
        let text = render(&plan);
        // No allowlist -> LIO builds the ACL itself; no CHAP -> AuthMethod None.
        assert!(text.contains("write /sys/kernel/config/target/iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/attrib/generate_node_acls = 1\n"), "{text}");
        assert!(text.contains("/tpgt_1/attrib/authentication = 0\n"), "{text}");
        assert!(text.contains("/tpgt_1/param/AuthMethod = None\n"), "{text}");
        // The credential files are CLEARED with LIO's own sentinel: switching
        // CHAP off must remove the old secret, and a zero-length write would
        // never reach the attribute at all.
        assert!(text.contains("/tpgt_1/auth/userid = NULL\n"), "{text}");
        assert!(text.contains("/tpgt_1/auth/password = NULL\n"), "{text}");
        // `authenticate_target` is CONFIGFS_ATTR_RO in LIO — measured: the
        // write fails with EACCES even as root, and the kernel derives the
        // flag from the mutual pair by itself. No plan may carry it.
        assert!(!text.contains("authenticate_target"), "{text}");
        // …and no ACL directory exists at all.
        assert!(!text.contains("/acls/"), "{text}");
    }

    #[test]
    fn mutual_chap_writes_both_credential_pairs_and_the_plan_never_prints_one() {
        let plan = plan_iscsi(&iscsi(mutual_chap(), vec![]), &fresh()).expect("plan");
        let text = render(&plan);
        assert!(text.contains("/tpgt_1/attrib/authentication = 1\n"), "{text}");
        assert!(text.contains("/tpgt_1/param/AuthMethod = CHAP\n"), "{text}");
        assert!(text.contains("/tpgt_1/auth/userid = vmware01\n"), "{text}");
        // Mutual CHAP is expressed by WRITING the mutual pair and nothing
        // else: `authenticate_target` is read-only and LIO sets it itself once
        // `userid_mutual` and `password_mutual` are in (measured on a node —
        // the attribute read 0 before and 1 after).
        assert!(!text.contains("authenticate_target"), "{text}");
        assert!(text.contains("/tpgt_1/auth/userid_mutual = helios\n"), "{text}");
        assert!(text.contains("/tpgt_1/auth/password = ***\n"), "{text}");
        assert!(text.contains("/tpgt_1/auth/password_mutual = ***\n"), "{text}");
        // The rendered plan is what the job log and the UI show. Neither
        // secret may appear in it in any form.
        assert!(!text.contains("sekret-inicjatora"), "{text}");
        assert!(!text.contains("sekret-targetu-1"), "{text}");
        // The values ARE in the plan the wrapper executes — the redaction is
        // in the rendering, not in the data.
        assert!(plan.iter().any(|s| matches!(
            s,
            ConfigfsStep::Write { value, secret: true, .. } if value == "sekret-inicjatora"
        )));
    }

    #[test]
    fn the_initiator_allowlist_is_a_filter_and_not_authentication() {
        let listed = vec!["iqn.1998-01.com.vmware:esx01".to_string()];
        // With an allowlist and no CHAP: LIO stops generating ACLs, so only
        // the listed IQN gets in — but nothing authenticates it, because an
        // IQN is a string the client picks for itself.
        let open = plan_iscsi(&iscsi(IscsiAuth::default(), listed.clone()), &fresh()).expect("plan");
        let text = render(&open);
        assert!(text.contains("/tpgt_1/attrib/generate_node_acls = 0\n"), "{text}");
        assert!(
            text.contains("mkdir /sys/kernel/config/target/iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/acls/iqn.1998-01.com.vmware:esx01\n"),
            "{text}"
        );
        assert!(text.contains("/acls/iqn.1998-01.com.vmware:esx01/auth/password = NULL\n"), "{text}");
        // Same on the ACL: `iscsi_nacl_auth_authenticate_target` is
        // CONFIGFS_ATTR_RO too, so a plan that carried it could never be
        // applied at all.
        assert!(!text.contains("authenticate_target"), "{text}");

        // The same allowlist WITH CHAP puts credentials on the ACL: that, and
        // only that, is what turns the list into a decision about identity.
        let authed = plan_iscsi(&iscsi(mutual_chap(), listed), &fresh()).expect("plan");
        let text = render(&authed);
        assert!(text.contains("/acls/iqn.1998-01.com.vmware:esx01/auth/userid = vmware01\n"), "{text}");
        assert!(text.contains("/acls/iqn.1998-01.com.vmware:esx01/auth/password = ***\n"), "{text}");
        // The LUN is mapped into the ACL, otherwise the initiator logs in and
        // sees nothing.
        assert!(
            text.contains("\nlink /sys/kernel/config/target/iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/acls/iqn.1998-01.com.vmware:esx01/lun_0/tentanas_vm_store_lun0 -> /sys/kernel/config/target/iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/lun/lun_0\n"),
            "{text}"
        );
    }

    #[test]
    fn the_portal_is_the_address_and_iser_is_a_flag_on_it() {
        let mut spec = iscsi(mutual_chap(), vec![]);
        spec.portals[0].transport = "iser".to_string();
        let text = render(&plan_iscsi(&spec, &fresh()).expect("plan"));
        assert!(text.contains("mkdir /sys/kernel/config/target/iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/np/10.10.0.5:3260\n"), "{text}");
        assert!(text.contains("/tpgt_1/np/10.10.0.5:3260/iser = 1\n"), "{text}");
        // TCP is not a second portal: the same np, with the flag off.
        spec.portals[0].transport = "tcp".to_string();
        let text = render(&plan_iscsi(&spec, &fresh()).expect("plan"));
        assert!(text.contains("/tpgt_1/np/10.10.0.5:3260/iser = 0\n"), "{text}");
        assert_eq!(text.matches("/tpgt_1/np/").count(), 2);
    }

    #[test]
    fn binding_every_interface_is_a_portal_like_any_other_and_the_tpg_is_enabled_last() {
        let mut spec = iscsi(mutual_chap(), vec![]);
        spec.portals[0].address = "0.0.0.0".to_string();
        let plan = plan_iscsi(&spec, &fresh()).expect("0.0.0.0 is a legal address, the wizard is what warns");
        let text = render(&plan);
        assert!(text.contains("/tpgt_1/np/0.0.0.0:3260\n"), "{text}");
        // `enable` closes the plan, and it is the LAST step for the obvious
        // reason rather than a kernel one: nothing may be exported before the
        // LUNs, the ACLs and the portals under it are in place.
        let last = plan.last().expect("a step");
        assert!(
            matches!(last, ConfigfsStep::Write { path, value, .. } if path.ends_with("/tpgt_1/enable") && value == "1"),
            "{last:?}"
        );
        // A hostname is refused: the address must be the one the UI showed.
        spec.portals[0].address = "storage0".to_string();
        assert!(plan_iscsi(&spec, &fresh()).is_err());
    }

    #[test]
    fn the_alua_group_state_reaches_the_backstore_and_the_lun() {
        let mut spec = iscsi(mutual_chap(), vec![]);
        spec.port_groups = vec![BlockPortGroup {
            group_id: 7,
            state: "non-optimized".to_string(),
            preferred: true,
        }];
        spec.luns[0].group_id = 7;
        let text = render(&plan_iscsi(&spec, &fresh()).expect("plan"));
        assert!(text.contains("mkdir /sys/kernel/config/target/core/iblock_0/tentanas_vm_store_lun0/alua/tentanas_gp7\n"), "{text}");
        assert!(text.contains("/alua/tentanas_gp7/tg_pt_gp_id = 7\n"), "{text}");
        assert!(text.contains("/alua/tentanas_gp7/alua_access_state = 1\n"), "{text}");
        assert!(text.contains("/alua/tentanas_gp7/preferred = 1\n"), "{text}");
        // Implicit only: an initiator may READ the state, never set it, or the
        // database would stop being the source of truth.
        assert!(text.contains("/alua/tentanas_gp7/alua_access_type = 1\n"), "{text}");
        assert!(text.contains("/tpgt_1/lun/lun_0/alua_tg_pt_gp = tg_pt_gp_name=tentanas_gp7\n"), "{text}");
        // A LUN pointing at a group nobody declared is refused, not defaulted.
        spec.luns[0].group_id = 9;
        assert!(plan_iscsi(&spec, &fresh()).is_err());
    }

    #[test]
    fn the_backstore_carries_the_identity_multipath_matches_two_paths_by() {
        let text = render(&plan_iscsi(&iscsi(mutual_chap(), vec![]), &fresh()).expect("plan"));
        assert!(text.contains("write /sys/kernel/config/target/core/iblock_0/tentanas_vm_store_lun0/control = udev_path=/dev/zvol/tank/vm-store\n"), "{text}");
        assert!(text.contains("/tentanas_vm_store_lun0/wwn/vpd_unit_serial = 0191f2c0-0000-7000-8000-000000000001\n"), "{text}");
        assert!(text.contains("/tentanas_vm_store_lun0/enable = 1\n"), "{text}");
    }

    #[test]
    fn a_subsystem_addresses_its_port_before_the_link_turns_the_listener_on() {
        let plan = plan_nvmet(&nvmet(vec![], true), &fresh_nvmet(1, 1)).expect("plan");
        let text = render(&plan);
        let order: Vec<usize> = [
            "/ports/1/addr_trtype = tcp\n",
            "\nlink /sys/kernel/config/nvmet/ports/1/subsystems/nqn.2026-09.pl.euvic:helios.scratch",
        ]
        .iter()
        .map(|needle| text.find(needle).unwrap_or_else(|| panic!("{needle} missing from {text}")))
        .collect();
        assert!(order[0] < order[1], "the address must precede the link:\n{text}");
        // Namespace attributes precede its `enable` for the same reason.
        assert!(
            text.find("/namespaces/1/device_path").unwrap() < text.find("/namespaces/1/enable").unwrap(),
            "{text}"
        );
        assert!(text.contains("/namespaces/1/ana_grpid = 1\n"), "{text}");
        assert!(text.contains("/ports/1/ana_groups/1/ana_state = optimized\n"), "{text}");
        // ANA group 1 comes with the port; only the others are created.
        assert!(!text.contains("mkdir /sys/kernel/config/nvmet/ports/1/ana_groups/1\n"), "{text}");
    }

    #[test]
    fn a_second_ana_group_is_created_on_every_port_of_the_subsystem() {
        let mut spec = nvmet(vec![], true);
        spec.port_groups.push(group(2, "non-optimized"));
        spec.portals.push(BlockPortal {
            address: "10.10.0.5".to_string(),
            port: NVME_PORT,
            transport: "rdma".to_string(),
        });
        let text = render(&plan_nvmet(&spec, &fresh_nvmet(2, 1)).expect("plan"));
        for port in [1, 2] {
            assert!(text.contains(&format!("mkdir /sys/kernel/config/nvmet/ports/{port}/ana_groups/2\n")), "{text}");
            assert!(text.contains(&format!("/ports/{port}/ana_groups/2/ana_state = non-optimized\n")), "{text}");
        }
        assert!(text.contains("/ports/2/addr_trtype = rdma\n"), "{text}");
        // ANA has no preferred bit; declaring one is an error, not a no-op.
        spec.port_groups[1].preferred = true;
        assert!(plan_nvmet(&spec, &fresh_nvmet(2, 1)).is_err());
    }

    #[test]
    fn dh_hmac_chap_lives_on_the_host_object_so_it_needs_the_allowlist() {
        let host = NvmetHost {
            nqn: "nqn.2014-08.org.nvmexpress:uuid:1b4e28ba-2fa1-11d2-883f-0016d3cca427".to_string(),
            dhchap_key: "DHHC-1:00:abcdefghijklmnopqrstuvwxyz0123456789ABCDEF+/=:".to_string(),
            dhchap_ctrl_key: "DHHC-1:00:ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210abcdef+/=:".to_string(),
            dhchap_hash: "hmac(sha256)".to_string(),
            dhchap_dhgroup: "ffdhe2048".to_string(),
        };
        let text = render(&plan_nvmet(&nvmet(vec![host.clone()], false), &fresh_nvmet(1, 1)).expect("plan"));
        assert!(text.contains("mkdir /sys/kernel/config/nvmet/hosts/nqn.2014-08.org.nvmexpress:uuid:1b4e28ba-2fa1-11d2-883f-0016d3cca427\n"), "{text}");
        assert!(text.contains("/dhchap_hash = hmac(sha256)\n"), "{text}");
        assert!(text.contains("/dhchap_dhgroup = ffdhe2048\n"), "{text}");
        assert!(text.contains("/dhchap_key = ***\n"), "{text}");
        assert!(text.contains("/dhchap_ctrl_key = ***\n"), "{text}");
        assert!(!text.contains("abcdefghijklmnop"), "{text}");
        assert!(text.contains("/attr_allow_any_host = 0\n"), "{text}");
        assert!(text.contains("\nlink /sys/kernel/config/nvmet/subsystems/nqn.2026-09.pl.euvic:helios.scratch/allowed_hosts/"), "{text}");

        // "allow any host" does not consult the host objects at all, so the
        // combination is refused instead of producing a subsystem that looks
        // authenticated and is not.
        let err = plan_nvmet(&nvmet(vec![host], true), &fresh_nvmet(1, 1)).expect_err("refused");
        assert!(err.to_string().contains("allow any host"), "{err}");
    }

    #[test]
    fn only_a_zvol_may_become_a_lun() {
        assert!(validate_backing_device("/dev/zvol/tank/vm-store").is_ok());
        // A pool member exported as a raw disk would destroy the pool.
        assert!(validate_backing_device("/dev/sda").is_err());
        assert!(validate_backing_device("/dev/disk/by-id/wwn-0x5000").is_err());
        assert!(validate_backing_device("/dev/zvol/tank").is_err());
        // §5.5 also names a file as a LUN source; this slice does not
        // implement it, so a file path is refused rather than half-handled.
        assert!(validate_backing_device("/mnt/tank/images/win.img").is_err());
        assert!(validate_backing_device("/etc/shadow").is_err());
        assert!(validate_backing_device("/mnt/../etc/shadow").is_err());
    }

    #[test]
    fn the_name_rules_are_the_ones_that_keep_a_name_out_of_the_filesystem() {
        assert!(validate_iqn("iqn.2026-09.pl.euvic:helios.vm-store").is_ok());
        assert!(validate_iqn("eui.02004567a425678d").is_ok());
        assert!(validate_nqn("nqn.2026-09.pl.euvic:helios.scratch").is_ok());
        // A name becomes a configfs directory, so a separator or a traversal
        // has to die here.
        assert!(validate_iqn("iqn.2026-09.pl/euvic:x").is_err());
        assert!(validate_nqn("nqn.2026-09..euvic:x").is_err());
        assert!(validate_iqn("IQN.2026-09.PL.EUVIC:X").is_err());
        assert!(validate_nqn("iqn.2026-09.pl.euvic:x").is_err());
        assert!(validate_iqn(&format!("iqn.{}", "a".repeat(230))).is_err());
    }

    #[test]
    fn a_chap_secret_shorter_than_the_rfc_asks_for_is_refused() {
        assert!(validate_chap_secret("krotkie").is_err());
        assert!(validate_chap_secret("dwanascie12x").is_ok());
        assert!(validate_chap_secret("ma spacje w srodku").is_err());
        assert!(validate_chap_user("vmware01").is_ok());
        assert!(validate_chap_user("ma spacje").is_err());
        // The two sides of mutual CHAP must not share one secret: reusing it
        // means either side can impersonate the other.
        let mut auth = mutual_chap();
        auth.mutual_password = auth.password.clone();
        assert!(validate_iscsi(&iscsi(auth, vec![])).is_err());
    }

    #[test]
    fn a_dh_hmac_chap_key_must_be_the_form_the_kernel_parses() {
        assert!(validate_dhchap_key("DHHC-1:00:abcd+/=:").is_ok());
        assert!(validate_dhchap_key("zwykle-haslo").is_err());
        assert!(validate_dhchap_key("DHHC-1:00:ab cd:").is_err());
    }

    // ----- the removal walkers, against a real directory tree ----------------
    //
    // configfs objects are directories, its attributes are files and its
    // references are symlinks, so a tree built with `std::fs` exercises the
    // walkers for real: the same readdir, the same unlink, the same rmdir.
    // What a temp directory cannot reproduce is the kernel creating an
    // object's attribute files on mkdir — so the fixtures below create them,
    // which is exactly what the plan expects to find.

    struct TempTree(PathBuf);

    impl TempTree {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "tentanas-block-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("temp tree");
            Self(path)
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).expect("dir");
            path
        }

        fn attr(&self, rel: &str, value: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
            std::fs::write(&path, value).expect("attr");
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_absent_debugfs_reads_as_unknown_and_never_as_zero_sessions() {
        // The distinction the whole feature rests on: a kernel without
        // CONFIG_NVME_TARGET_DEBUGFS, or a node with debugfs unmounted, must
        // not be reported as a subsystem nobody is connected to — that number
        // would end up in the delete dialog's blast radius.
        let missing = read_nvmet_sessions(Path::new("/proc/self/definitely-not-debugfs"));
        assert!(!missing.available);
        assert!(missing.controllers.is_empty());
        assert!(
            missing.reason.contains("CONFIG_NVME_TARGET_DEBUGFS"),
            "{}",
            missing.reason
        );

        let tree = TempTree::new("nvmet-debugfs");
        let nqn = "nqn.2026-09.local.tentaflow:helios.vm-store";
        // The layout is the KERNEL's, and it was MEASURED with a controller
        // attached over TCP: the subsystem directory held exactly `ctrl1`,
        // carrying `hostnqn`, `host_traddr`, `kato`, `port`, `state`,
        // `tls_concat`, `tls_key`, and `state` read `ready`. A bare `2` is not
        // a controller and never was — the previous fixture described what the
        // code assumed, which is how a filter that rejected every real
        // controller passed its own test and reported a confident zero.
        tree.attr(&format!("{nqn}/ctrl10/hostnqn"), "nqn.2014-08.org.nvmexpress:uuid:esx02\n");
        tree.attr(&format!("{nqn}/ctrl10/host_traddr"), "192.168.10.25\n");
        tree.attr(&format!("{nqn}/ctrl10/port"), "1\n");
        tree.attr(&format!("{nqn}/ctrl10/state"), "ready\n");
        tree.attr(&format!("{nqn}/ctrl2/hostnqn"), "nqn.2014-08.org.nvmexpress:uuid:esx01\n");
        tree.attr(&format!("{nqn}/ctrl2/host_traddr"), "192.168.10.24\n");
        tree.attr(&format!("{nqn}/ctrl2/port"), "1\n");
        tree.attr(&format!("{nqn}/ctrl2/state"), "ready\n");
        // Everything else the kernel puts under a subsystem: an attribute
        // file, the `ns<N>` directories a 6.16 kernel adds, and a directory
        // whose name merely looks numeric. None of them is an association.
        tree.attr(&format!("{nqn}/allow_any"), "0\n");
        tree.attr(&format!("{nqn}/ns1/reservation"), "\n");
        tree.dir(&format!("{nqn}/2"));
        tree.dir(&format!("{nqn}/passthru"));
        // A subsystem with no controller at all: present in the map, empty.
        tree.dir("nqn.2026-09.local.tentaflow:helios.idle");

        let found = read_nvmet_sessions(&tree.0);
        assert!(found.available && found.reason.is_empty());
        let live = found.controllers.get(nqn).expect("subsystem");
        // Numeric order, not lexicographic: controller 2 comes before 10, and
        // the id carries no `ctrl` prefix — `sessions_from` prints it as
        // "controller {id}" when the host has not named itself.
        assert_eq!(
            live.iter().map(|c| c.cntlid.as_str()).collect::<Vec<_>>(),
            vec!["2", "10"]
        );
        assert_eq!(live[0].hostnqn, "nqn.2014-08.org.nvmexpress:uuid:esx01");
        assert_eq!(live[0].host_traddr, "192.168.10.24");
        // The state is carried VERBATIM — measured as `ready`, and nothing
        // branches on it.
        assert_eq!(live[1].state, "ready");
        assert!(found
            .controllers
            .get("nqn.2026-09.local.tentaflow:helios.idle")
            .expect("idle subsystem")
            .is_empty());
    }

    #[test]
    fn a_subsystem_directory_that_cannot_be_read_is_unknown_and_never_zero_sessions() {
        // The same rule as the missing tree, one level down. A `read_dir` that
        // FAILS is not a `read_dir` that found nothing: swallowing the error
        // leaves `available: true` with an empty list, which the delete dialog
        // prints as a confident "0 sessions lose the disk" for a subsystem
        // that may have three hosts writing to it. That is the measured zero
        // this whole path exists to avoid.
        let tree = TempTree::new("nvmet-debugfs-unreadable");
        let nqn = "nqn.2026-09.local.tentaflow:helios.vm-store";
        let dir = tree.dir(nqn);
        // 0000: the walk can still see the directory (the parent is readable)
        // and still cannot list it. Root ignores the mode, so the assertion is
        // only meaningful unprivileged — where the check matters, since the
        // core reads this through the helper but the walk itself is ordinary.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("mode");
        let found = read_nvmet_sessions(&tree.0);
        let readable_anyway = std::fs::read_dir(&dir).is_ok();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
        if readable_anyway {
            // Running as root, where a 0000 directory is still listable — most
            // CI images. The mode cannot make the read fail here, so the same
            // property is asserted through a path that CANNOT be read by
            // anyone: a subsystem entry that is not a directory at all.
            let file = tree.0.join("nqn.2026-09.local.tentaflow:helios.not-a-dir");
            std::fs::write(&file, b"x").expect("file");
            let found = read_nvmet_sessions(&tree.0);
            assert!(
                found.available,
                "a plain file among the subsystems is skipped, not treated as unknown"
            );
            assert!(!found.controllers.contains_key("nqn.2026-09.local.tentaflow:helios.not-a-dir"));
            return;
        }
        assert!(!found.available, "an unreadable subsystem is not an empty one");
        assert!(found.reason.contains("cannot be read"), "{}", found.reason);
        assert!(found.controllers.is_empty());
    }

    #[test]
    fn removing_a_live_iscsi_target_takes_its_acls_portals_luns_and_backstores() {
        let tree = TempTree::new("iscsi");
        let iqn = "iqn.2026-09.pl.euvic:helios.vm-store";
        let other = "iqn.2026-09.pl.euvic:helios.recznie";
        let dev = tree.dir("core/iblock_0/tentanas_vm_store_lun0");
        tree.dir("core/iblock_0/tentanas_vm_store_lun0/alua/default_tg_pt_gp");
        tree.dir("core/iblock_0/tentanas_vm_store_lun0/alua/tentanas_gp1");
        let tpg = tree.dir(&format!("iscsi/{iqn}/tpgt_1"));
        tree.attr(&format!("iscsi/{iqn}/tpgt_1/enable"), "1");
        let lun = tree.dir(&format!("iscsi/{iqn}/tpgt_1/lun/lun_0"));
        std::os::unix::fs::symlink(&dev, lun.join("tentanas_vm_store_lun0")).expect("lun link");
        let acl = tree.dir(&format!("iscsi/{iqn}/tpgt_1/acls/iqn.1998-01.com.vmware:esx01"));
        let mapped = tree.dir(&format!(
            "iscsi/{iqn}/tpgt_1/acls/iqn.1998-01.com.vmware:esx01/lun_0"
        ));
        std::os::unix::fs::symlink(&lun, mapped.join("tentanas_vm_store_lun0")).expect("acl link");
        tree.dir(&format!("iscsi/{iqn}/tpgt_1/np/10.10.0.5:3260"));
        // A target this app did not make. It must survive untouched.
        tree.dir(&format!("iscsi/{other}/tpgt_1/np/192.168.1.5:3260"));

        let log = remove_iscsi(&tree.0, iqn).expect("removed");
        assert!(log.iter().any(|l| l.contains("iSCSI target") && l.contains(iqn)), "{log:?}");
        assert!(!tpg.exists(), "the TPG is gone");
        assert!(!acl.exists(), "the ACL is gone");
        assert!(!tree.0.join("iscsi").join(iqn).exists(), "the target is gone");
        assert!(!dev.exists(), "the backstore is gone");
        // The HBA directory is shared with every other backstore and stays.
        assert!(tree.0.join("core/iblock_0").is_dir());
        assert!(tree.0.join("iscsi").join(other).join("tpgt_1/np/192.168.1.5:3260").is_dir(),
            "a target we did not create is untouched");
        // The teardown disabled the TPG before detaching anything from it.
        assert!(!log.iter().any(|l| l.contains("not disabled")), "{log:?}");

        // Running it again on a node where nothing is left is not an error:
        // the teardown may run after a reboot cleared configfs.
        let again = remove_iscsi(&tree.0, iqn).expect("idempotent");
        assert!(again.iter().any(|l| l.contains("not present in configfs")), "{again:?}");
    }

    #[test]
    fn removing_an_nvmet_subsystem_keeps_a_port_and_a_host_another_subsystem_still_uses() {
        let tree = TempTree::new("nvmet");
        let mine = "nqn.2026-09.pl.euvic:helios.scratch";
        let theirs = "nqn.2026-09.pl.euvic:helios.inne";
        let host = tree.dir("hosts/nqn.2014-08.org.nvmexpress:uuid:1b4e28ba");
        let lonely = tree.dir("hosts/nqn.2014-08.org.nvmexpress:uuid:ffffffff");
        for nqn in [mine, theirs] {
            tree.dir(&format!("subsystems/{nqn}/allowed_hosts"));
            std::os::unix::fs::symlink(&host, tree.0.join(format!("subsystems/{nqn}/allowed_hosts/nqn.2014-08.org.nvmexpress:uuid:1b4e28ba"))).expect("host link");
        }
        std::os::unix::fs::symlink(&lonely, tree.0.join(format!("subsystems/{mine}/allowed_hosts/nqn.2014-08.org.nvmexpress:uuid:ffffffff"))).expect("host link");
        tree.attr(&format!("subsystems/{mine}/namespaces/1/enable"), "1");
        // Port 1 carries both subsystems, port 2 only ours.
        tree.dir("ports/1/subsystems");
        tree.dir("ports/1/ana_groups/1");
        tree.dir("ports/2/subsystems");
        tree.dir("ports/2/ana_groups/1");
        tree.dir("ports/2/ana_groups/2");
        for (port, nqn) in [(1, mine), (1, theirs), (2, mine)] {
            std::os::unix::fs::symlink(
                tree.0.join(format!("subsystems/{nqn}")),
                tree.0.join(format!("ports/{port}/subsystems/{nqn}")),
            )
            .expect("port link");
        }

        let log = remove_nvmet(&tree.0, mine).expect("removed");
        assert!(!tree.0.join("subsystems").join(mine).exists(), "the subsystem is gone");
        assert!(tree.0.join("subsystems").join(theirs).is_dir(), "the other subsystem stays");
        // Port 1 still answers for the other subsystem; port 2 had only ours.
        assert!(tree.0.join("ports/1").is_dir(), "a shared port stays: {log:?}");
        assert!(!tree.0.join("ports/1/subsystems").join(mine).exists());
        assert!(!tree.0.join("ports/2").exists(), "the port we emptied is gone: {log:?}");
        // The host on two allowlists stays, the one only we used goes.
        assert!(host.is_dir(), "a host another subsystem allows stays");
        assert!(!lonely.exists(), "a host nothing allows any more is removed");
    }

    #[test]
    fn an_existing_port_is_reused_and_never_rewritten() {
        let tree = TempTree::new("ports");
        // A port this node already serves something on, and a second one.
        tree.attr("ports/1/addr_trtype", "tcp\n");
        tree.attr("ports/1/addr_traddr", "10.10.0.5\n");
        tree.attr("ports/1/addr_trsvcid", "4420\n");
        tree.attr("ports/4/addr_trtype", "rdma\n");
        tree.attr("ports/4/addr_traddr", "10.10.0.5\n");
        tree.attr("ports/4/addr_trsvcid", "4420\n");

        let mut spec = nvmet(vec![], true);
        spec.portals = vec![
            BlockPortal {
                address: "10.10.0.5".into(),
                port: NVME_PORT,
                transport: "rdma".into(),
            },
            BlockPortal {
                address: "10.10.0.5".into(),
                port: NVME_PORT,
                transport: "tcp".into(),
            },
            BlockPortal {
                address: "192.168.1.5".into(),
                port: NVME_PORT,
                transport: "tcp".into(),
            },
        ];
        let observed = observe_nvmet(&tree.0, &spec);
        // The two that exist are reused whatever order they are asked for in;
        // the new one takes the lowest FREE index, not "the next after 4".
        assert_eq!(
            observed.ports,
            vec![
                NvmetPortObserved { id: 4, configured: true },
                NvmetPortObserved { id: 1, configured: true },
                NvmetPortObserved { id: 2, configured: false },
            ]
        );

        // …and the plan must not rewrite the two it reused. nvmet returns
        // -EACCES for any `addr_*` write on a port a subsystem is linked into,
        // so a plan that touched them could never bring up a SECOND subsystem
        // on an address the node is already serving — which is the whole point
        // of sharing a port.
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        assert!(!text.contains("/ports/1/addr_"), "port 1 was rewritten:\n{text}");
        assert!(!text.contains("/ports/4/addr_"), "port 4 was rewritten:\n{text}");
        assert!(text.contains("/ports/2/addr_traddr = 192.168.1.5\n"), "{text}");
        // The subsystem is still linked into all three.
        for port in [1, 2, 4] {
            assert!(
                text.contains(&format!("\nlink /sys/kernel/config/nvmet/ports/{port}/subsystems/")),
                "{text}"
            );
        }
        // A node with no ports at all numbers from one and configures each.
        let empty = TempTree::new("ports-empty");
        let observed = observe_nvmet(&empty.0, &spec);
        assert_eq!(observed.ports.iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(observed.ports.iter().all(|p| !p.configured));
    }

    #[test]
    fn re_picking_the_interface_stops_the_subsystem_answering_on_the_old_port() {
        // The nvmet half of the prune, and the reason it is not optional: the
        // portal-drift alert tells the admin "the target stays as it is until
        // an admin re-picks the interface". If re-picking merely ADDS a port,
        // the subsystem keeps answering on the address the admin just stopped
        // choosing — a raw disk served on a network nobody picked, which is
        // the exact exposure the drift policy exists to prevent.
        let tree = TempTree::new("nvmet-port-prune");
        let mut spec = nvmet(vec![], true);
        // The old address (port 1) and an RDMA port (port 3) the subsystem is
        // linked into today; the new address is port 2.
        for (id, trtype, addr) in [(1u32, "tcp", "10.10.0.5"), (2, "tcp", "10.10.0.9"), (3, "rdma", "10.10.0.5")] {
            tree.attr(&format!("ports/{id}/addr_trtype"), &format!("{trtype}\n"));
            tree.attr(&format!("ports/{id}/addr_traddr"), &format!("{addr}\n"));
            tree.attr(&format!("ports/{id}/addr_trsvcid"), "4420\n");
            let links = tree.dir(&format!("ports/{id}/subsystems"));
            std::os::unix::fs::symlink(
                tree.0.join("subsystems").join(&spec.nqn),
                links.join(&spec.nqn),
            )
            .expect("subsystem link");
        }
        spec.portals = vec![BlockPortal {
            address: "10.10.0.9".into(),
            port: NVME_PORT,
            transport: "tcp".into(),
        }];

        let observed = observe_nvmet(&tree.0, &spec);
        assert_eq!(observed.linked_ports, vec![1, 2, 3]);
        assert_eq!(observed.ports, vec![NvmetPortObserved { id: 2, configured: true }]);
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        let link_new = text
            .find(&format!("\nlink /sys/kernel/config/nvmet/ports/2/subsystems/{}", spec.nqn))
            .expect("the new port keeps its link");
        // Both the old address AND the transport that was dropped stop
        // answering — a `tcp+rdma` → `tcp` edit is the same bug wearing
        // another hat.
        for id in [1, 3] {
            let unlink = text
                .find(&format!("unlink /sys/kernel/config/nvmet/ports/{id}/subsystems/{}", spec.nqn))
                .unwrap_or_else(|| panic!("port {id} still answers:\n{text}"));
            // After the new link, so a re-picked portal never has a window
            // with nothing listening.
            assert!(link_new < unlink, "{text}");
        }
        // The PORT object stays: it is node-wide and may carry other
        // subsystems. Deciding it is unused is `remove_nvmet`'s business,
        // which is the only place that sees the whole tree.
        assert!(!text.contains("rmdir /sys/kernel/config/nvmet/ports/1"), "{text}");
    }

    #[test]
    fn a_live_namespace_is_cycled_only_when_its_device_changed() {
        let tree = TempTree::new("ns");
        let spec = nvmet(vec![], true);
        let ns = &spec.namespaces[0];
        let dir = format!("subsystems/{}/namespaces/1", spec.nqn);
        tree.attr(&format!("{dir}/device_path"), &format!("{}\n", ns.device_path));
        tree.attr(&format!("{dir}/device_uuid"), &format!("{}\n", ns.uuid));
        tree.attr(&format!("{dir}/enable"), "1\n");

        // Unchanged and live: nvmet returns -EBUSY for `device_path` on an
        // enabled namespace, so the plan leaves it completely alone.
        let observed = observe_nvmet(&tree.0, &spec);
        assert_eq!(
            observed.namespaces,
            vec![NvmetNsObserved { nsid: 1, enabled: true, matches: true }]
        );
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        assert!(!text.contains("/namespaces/1/device_path"), "{text}");
        assert!(!text.contains("/namespaces/1/enable"), "{text}");

        // The zvol behind it changed: disable, rewrite, enable — in that order.
        let mut moved = spec.clone();
        moved.namespaces[0].device_path = "/dev/zvol/fast/inny".into();
        let observed = observe_nvmet(&tree.0, &moved);
        assert!(observed.namespaces[0].enabled && !observed.namespaces[0].matches);
        let text = render(&plan_nvmet(&moved, &observed).expect("plan"));
        let off = text.find("/namespaces/1/enable = 0").expect("disabled first");
        let path = text.find("/namespaces/1/device_path = /dev/zvol/fast/inny").expect("rewritten");
        let on = text.find("/namespaces/1/enable = 1").expect("enabled again");
        assert!(off < path && path < on, "{text}");
    }

    #[test]
    fn re_applying_a_live_iscsi_target_never_takes_the_tpg_down_and_rewrites_nothing_it_holds() {
        let tree = TempTree::new("reapply");
        let spec = iscsi(mutual_chap(), vec![]);
        let lun = &spec.luns[0];
        let dev_rel = format!("core/iblock_0/{}", lun.name);

        // First apply: nothing of this target exists, so everything is written
        // and `enable = 1` closes the plan.
        let observed = observe_iscsi(&tree.0, &spec);
        let first = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(first.contains("/control = udev_path=/dev/zvol/tank/vm-store\n"), "{first}");
        assert!(first.contains("/wwn/vpd_unit_serial = "), "{first}");
        assert!(first.contains(&format!("{dev_rel}/enable = 1\n")), "{first}");
        assert!(first.contains("/alua/tentanas_gp1/tg_pt_gp_id = 1\n"), "{first}");
        assert_eq!(first.matches("/tpgt_1/enable = 1").count(), 1);

        // The node now holds exactly what the first apply wrote: the TPG is
        // live and the backstore is configured with the right serial and group
        // id. This is the SECOND apply — the normal case, because `apply`
        // re-renders every target on every mutation of any target.
        tree.attr("iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/enable", "1\n");
        tree.attr(&format!("{dev_rel}/enable"), "1\n");
        tree.attr(
            &format!("{dev_rel}/wwn/vpd_unit_serial"),
            &format!("T10 VPD Unit Serial Number: {}\n", lun.uuid),
        );
        tree.attr(&format!("{dev_rel}/alua/tentanas_gp1/tg_pt_gp_id"), "1\n");

        let observed = observe_iscsi(&tree.0, &spec);
        let second = render(&plan_iscsi(&spec, &observed).expect("plan"));
        // THE regression this test exists for: LIO's disable branch is
        // `iscsit_tpg_disable_portal_group(tpg, 1)` — force=1 — which logs
        // every initiator of the target out. A reconcile must never emit it,
        // and it never needed to: `param/*` and `attrib/*` are writable on a
        // live TPG (measured).
        assert!(!second.contains("/tpgt_1/enable = 0"), "no session may be dropped:\n{second}");
        assert!(second.contains("/tpgt_1/param/AuthMethod = CHAP\n"), "{second}");
        assert!(second.contains("/tpgt_1/attrib/authentication = 1\n"), "{second}");
        // …and the FOUR writes MEASURED to be refused on an object the kernel
        // already holds are simply not there: `control` (EEXIST, "Unable to
        // set udev_path= while ib_dev->ibd_bd exists"), `vpd_unit_serial`
        // (EINVAL, "…while active 1 $FABRIC_MOD exports exist"), the device's
        // own `enable` (EEXIST) and `tg_pt_gp_id` (EINVAL, "already has a
        // valid ID").
        assert!(!second.contains("/control = udev_path="), "{second}");
        assert!(!second.contains("vpd_unit_serial"), "{second}");
        assert!(!second.contains(&format!("{dev_rel}/enable = 1")), "{second}");
        assert!(!second.contains("tg_pt_gp_id"), "{second}");
    }

    #[test]
    fn a_second_apply_writes_only_what_the_kernel_does_not_already_hold() {
        // "Skips the writes the kernel refuses" is not the same claim as
        // "writes only what differs", and for two rounds this file only did
        // the first. Everything else — the four `attrib/*`, `param/AuthMethod`,
        // the three ALUA attributes, `np/iser` and `{tpg}/enable` — went out
        // unconditionally on every apply of every target, which is ~15 writes
        // per target that nobody asked for and a job log in which the lines
        // that DID change cannot be found.
        let tree = TempTree::new("second-apply");
        let spec = iscsi(IscsiAuth::default(), vec![]);
        let lun = &spec.luns[0];
        let dev_rel = format!("core/iblock_0/{}", lun.name);
        let tpg_rel = "iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1";

        // Everything the first apply wrote, as the kernel now holds it.
        tree.attr(&format!("{tpg_rel}/enable"), "1\n");
        tree.attr(&format!("{tpg_rel}/attrib/generate_node_acls"), "1\n");
        tree.attr(&format!("{tpg_rel}/attrib/cache_dynamic_acls"), "0\n");
        tree.attr(&format!("{tpg_rel}/attrib/demo_mode_write_protect"), "0\n");
        tree.attr(&format!("{tpg_rel}/attrib/authentication"), "0\n");
        tree.attr(&format!("{tpg_rel}/param/AuthMethod"), "None\n");
        tree.attr(&format!("{tpg_rel}/np/10.10.0.5:3260/iser"), "0\n");
        tree.attr(&format!("{dev_rel}/enable"), "1\n");
        tree.attr(
            &format!("{dev_rel}/wwn/vpd_unit_serial"),
            &format!("T10 VPD Unit Serial Number: {}\n", lun.uuid),
        );
        tree.attr(&format!("{dev_rel}/alua/tentanas_gp1/tg_pt_gp_id"), "1\n");
        // `Implicit`, not `1` — MEASURED (run 5). The plan WRITES `1` here and
        // the kernel PRINTS the word, so a fixture carrying `1` pins a reading
        // no real LIO produces and hides a rewrite happening on every apply.
        tree.attr(&format!("{dev_rel}/alua/tentanas_gp1/alua_access_type"), "Implicit\n");
        tree.attr(&format!("{dev_rel}/alua/tentanas_gp1/alua_access_state"), "0\n");
        tree.attr(&format!("{dev_rel}/alua/tentanas_gp1/preferred"), "0\n");

        let observed = observe_iscsi(&tree.0, &spec);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        for skipped in [
            "generate_node_acls",
            "cache_dynamic_acls",
            "demo_mode_write_protect",
            "attrib/authentication",
            "param/AuthMethod",
            "alua_access_type",
            "alua_access_state",
            "preferred",
            "/iser",
            "/tpgt_1/enable",
        ] {
            assert!(!text.contains(skipped), "{skipped} was rewritten for nothing:\n{text}");
        }

        // A value that DID change is still written — the skip is a comparison,
        // not a "the object exists" shortcut.
        let mut chapped = iscsi(mutual_chap(), vec![]);
        chapped.portals[0].transport = "iser".to_string();
        let observed = observe_iscsi(&tree.0, &chapped);
        let text = render(&plan_iscsi(&chapped, &observed).expect("plan"));
        assert!(text.contains("/attrib/authentication = 1\n"), "{text}");
        assert!(text.contains("/param/AuthMethod = CHAP\n"), "{text}");
        assert!(text.contains("/np/10.10.0.5:3260/iser = 1\n"), "{text}");

        // An attribute this node could NOT read is not "held": the write goes
        // out. Skipping on a failed read is how a target ends up trusting a
        // value nobody looked at.
        std::fs::remove_file(tree.0.join(format!("{tpg_rel}/param/AuthMethod"))).expect("rm");
        let observed = observe_iscsi(&tree.0, &spec);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(text.contains("/param/AuthMethod = None\n"), "{text}");

        // …and a TPG somebody disabled by hand comes back on the next apply.
        tree.attr(&format!("{tpg_rel}/enable"), "0\n");
        let observed = observe_iscsi(&tree.0, &spec);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(text.contains("/tpgt_1/enable = 1\n"), "{text}");
    }

    #[test]
    fn dropping_an_initiator_or_re_picking_a_portal_removes_it_from_the_kernel() {
        // Reporting success for a revocation that never reached the kernel is
        // the worst failure mode this file has: the ACL keeps its CHAP
        // credentials and the initiator keeps logging in, while the UI says
        // access was taken away. Same for a portal: after the admin answers a
        // drift alert by re-picking the interface, the OLD address must stop
        // listening, or the export answers on a NIC nobody chose.
        let tree = TempTree::new("prune");
        let spec = iscsi(mutual_chap(), vec!["iqn.1998-01.com.vmware:esx01".to_string()]);
        let tpg_rel = "iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1";
        let stale = "iqn.1998-01.com.vmware:esx99";
        // An ACL the allowlist no longer names, with a mapped LUN and its link.
        let mapped = tree.dir(&format!("{tpg_rel}/acls/{stale}/lun_0"));
        let target_lun = tree.dir(&format!("{tpg_rel}/lun/lun_0"));
        std::os::unix::fs::symlink(&target_lun, mapped.join("tentanas_vm_store_lun0"))
            .expect("mapped link");
        // The ACL that stays.
        tree.dir(&format!("{tpg_rel}/acls/iqn.1998-01.com.vmware:esx01"));
        // The portal the target used to answer on, next to the current one.
        tree.dir(&format!("{tpg_rel}/np/10.10.0.5:3260"));
        tree.dir(&format!("{tpg_rel}/np/192.168.1.9:3260"));

        let observed = observe_iscsi(&tree.0, &spec);
        assert_eq!(observed.acls.len(), 2);
        assert_eq!(observed.portals, vec!["10.10.0.5:3260", "192.168.1.9:3260"]);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));

        // The mapped LUN's link goes before the mapped LUN, which goes before
        // the ACL: configfs refuses `rmdir` on a group that still has
        // children. MEASURED on a LIVE target: the unlink and all three
        // `rmdir`s succeed while `{tpg}/enable` reads 1, and it still reads 1
        // afterwards — revoking an initiator does not take the target down.
        let unlink = text
            .find(&format!("unlink /sys/kernel/config/target/{tpg_rel}/acls/{stale}/lun_0/tentanas_vm_store_lun0"))
            .expect("the mapped link is removed");
        let rm_mapped = text
            .find(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/{stale}/lun_0\n"))
            .expect("the mapped LUN is removed");
        let rm_acl = text
            .find(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/{stale}\n"))
            .expect("the ACL is removed");
        assert!(unlink < rm_mapped && rm_mapped < rm_acl, "{text}");
        // The old portal stops listening; the one the spec names does not.
        assert!(text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/np/192.168.1.9:3260\n")), "{text}");
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/np/10.10.0.5:3260")), "{text}");
        // The allowlisted initiator keeps everything it has.
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/iqn.1998-01.com.vmware:esx01")), "{text}");
        // Removals come after the creates and before the target comes up.
        assert!(rm_acl < text.find("/tpgt_1/enable = 1").expect("enable"), "{text}");
    }

    #[test]
    fn without_an_allowlist_only_an_acl_with_no_session_is_removed() {
        // With `generate_node_acls = 1` LIO creates an ACL of its own for every
        // initiator that logs in, and configfs publishes NOTHING that tells one
        // of those from a leftover of ours: `lio_target_initiator_attrs` is
        // `info`, `cmdsn_depth`, `tag` — there is no `dynamic_node_acl`. So the
        // only safe reading is the session: removing a connected ACL would
        // force-log-out a live client on every reconcile, while a stale one
        // still carries credentials that would let its initiator in after the
        // secret was changed.
        let tree = TempTree::new("dynamic-acls");
        let spec = iscsi(mutual_chap(), vec![]);
        let tpg_rel = "iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1";
        let live = "iqn.1998-01.com.vmware:esx01";
        let stale = "iqn.1998-01.com.vmware:esx99";
        tree.attr(
            &format!("{tpg_rel}/acls/{live}/info"),
            "InitiatorName: iqn.1998-01.com.vmware:esx01\nSession State: TARG_SESS_STATE_LOGGED_IN\n",
        );
        tree.attr(
            &format!("{tpg_rel}/acls/{stale}/info"),
            "No active iSCSI Session for Initiator Endpoint: iqn.1998-01.com.vmware:esx99\n",
        );
        tree.dir(&format!("{tpg_rel}/np/10.10.0.5:3260"));

        let observed = observe_iscsi(&tree.0, &spec);
        assert!(observed.acls.iter().any(|a| a.initiator == live && a.session == Some(true)));
        assert!(observed.acls.iter().any(|a| a.initiator == stale && a.session == Some(false)));
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(text.contains("/tpgt_1/attrib/generate_node_acls = 1\n"), "{text}");
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/{live}")), "{text}");
        assert!(text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/{stale}\n")), "{text}");
        // And a default group of an ACL is never a removal candidate: `auth/`,
        // `attrib/` and `param/` are directories too.
        assert!(!text.contains("/auth\n") && !text.contains("/attrib\n"), "{text}");
    }

    #[test]
    fn an_acl_whose_info_cannot_be_read_is_left_alone_rather_than_revoked() {
        // The failure this is written against: with no allowlist, three
        // clients are logged in through ACLs LIO generated. A kernel that
        // renames `info`, or one read that fails with EIO, used to read as
        // "no session" for all three — and `rmdir` of an ACL is
        // `core_tpg_del_initiator_node_acl`, which force-logs-out whoever is
        // on it. A failed read is not a measurement of an empty one.
        let tree = TempTree::new("acl-info-unreadable");
        let spec = iscsi(mutual_chap(), vec![]);
        let tpg_rel = "iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1";
        let unreadable = "iqn.1998-01.com.vmware:esx07";
        let blank = "iqn.1998-01.com.vmware:esx08";
        // No `info` at all — the attribute the kernel is expected to publish
        // is simply not there.
        tree.dir(&format!("{tpg_rel}/acls/{unreadable}"));
        // Present but empty: LIO's idle answer is a whole sentence, so an ACL
        // that prints nothing is a shape nobody has measured.
        tree.attr(&format!("{tpg_rel}/acls/{blank}/info"), "\n");
        tree.dir(&format!("{tpg_rel}/np/10.10.0.5:3260"));

        let observed = observe_iscsi(&tree.0, &spec);
        assert!(observed.acls.iter().all(|a| a.session.is_none()), "{:?}", observed.acls);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        for acl in [unreadable, blank] {
            assert!(
                !text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/{acl}")),
                "an ACL this node knows nothing about must not be revoked:\n{text}"
            );
        }

        // With an ALLOWLIST the rule is the other one and stays that way: an
        // ACL the admin did not name is ours, whatever `info` says or fails to
        // say, because LIO generates none while `generate_node_acls = 0`.
        let named = iscsi(mutual_chap(), vec!["iqn.1998-01.com.vmware:esx01".to_string()]);
        let observed = observe_iscsi(&tree.0, &named);
        let text = render(&plan_iscsi(&named, &observed).expect("plan"));
        assert!(
            text.contains(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/acls/{unreadable}\n")),
            "{text}"
        );
    }

    #[test]
    fn a_lun_that_leaves_the_target_takes_its_backstore_with_it() {
        // §5.8 forbids orphans, and this is the one that bites: an iblock
        // backstore holds the zvol OPEN, so a device left behind after its LUN
        // is gone makes `zfs destroy` and `zpool export` fail on a volume
        // nobody is exporting any more.
        let tree = TempTree::new("stale-backstore");
        let spec = iscsi(IscsiAuth::default(), vec![]);
        let tpg_rel = "iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1";
        let gone = "tentanas_vm_store_lun7";
        // A LUN the spec no longer names, linking a backstore of ours.
        let lun_dir = tree.dir(&format!("{tpg_rel}/lun/lun_7"));
        let dev = tree.dir(&format!("core/iblock_0/{gone}"));
        tree.dir(&format!("core/iblock_0/{gone}/alua/tentanas_gp2"));
        tree.dir(&format!("core/iblock_0/{gone}/alua/default_tg_pt_gp"));
        std::os::unix::fs::symlink(&dev, lun_dir.join(gone)).expect("backstore link");
        // The LUN the spec keeps, with its own backstore — which must not be
        // touched by any of this.
        let kept = &spec.luns[0].name;
        let kept_dir = tree.dir(&format!("{tpg_rel}/lun/lun_0"));
        let kept_dev = tree.dir(&format!("core/iblock_0/{kept}"));
        std::os::unix::fs::symlink(&kept_dev, kept_dir.join(kept)).expect("kept link");

        let observed = observe_iscsi(&tree.0, &spec);
        assert_eq!(
            observed.stale_devices,
            vec![(gone.to_string(), vec!["tentanas_gp2".to_string()])]
        );
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        let rm_lun = text
            .find(&format!("rmdir /sys/kernel/config/target/{tpg_rel}/lun/lun_7\n"))
            .expect("the LUN directory goes");
        let rm_group = text
            .find(&format!("rmdir /sys/kernel/config/target/core/iblock_0/{gone}/alua/tentanas_gp2\n"))
            .expect("the user-created ALUA group goes with it");
        let rm_dev = text
            .find(&format!("rmdir /sys/kernel/config/target/core/iblock_0/{gone}\n"))
            .expect("and then the backstore");
        // configfs refuses `rmdir` on an object something still links to
        // (-EBUSY) or one that still has children (-ENOTEMPTY), so the order
        // is the plan's job: LUN directory, then the group, then the device.
        assert!(rm_lun < rm_group && rm_group < rm_dev, "{text}");
        // `default_tg_pt_gp` belongs to the device and goes with it — a
        // `rmdir` of it would be EPERM.
        assert!(!text.contains("default_tg_pt_gp"), "{text}");
        // Nothing the spec still exports is ever a removal candidate.
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/target/core/iblock_0/{kept}")), "{text}");
    }

    #[test]
    fn the_unit_serial_is_never_rewritten_on_a_lun_the_kernel_is_exporting() {
        // MEASURED: `{dev}/wwn/vpd_unit_serial` is EINVAL — "Unable to set VPD
        // Unit Serial while active 1 $FABRIC_MOD exports exist" — from the
        // moment a LUN links to the backstore, and `apply_plan` stops at the
        // first failed step. So this write may never be emitted for an
        // exported device, whatever the serial reads as.
        //
        // NOT measured: the LABEL the show method prints. That is why the
        // comparison is a suffix match — a strict parse of a label that
        // changed would yield an empty serial, and an empty serial never
        // equals the uuid, so the strict version would emit exactly the write
        // the kernel refuses and break the second apply of every target.
        let tree = TempTree::new("vpd");
        let spec = iscsi(IscsiAuth::default(), vec![]);
        let lun = &spec.luns[0];
        let tpg_rel = "iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1";
        let dev = tree.dir(&format!("core/iblock_0/{}", lun.name));
        let lun_dir = tree.dir(&format!("{tpg_rel}/lun/lun_0"));
        std::os::unix::fs::symlink(&dev, lun_dir.join(&lun.name)).expect("export link");
        // A label this build has never seen, over the right value.
        tree.attr(
            &format!("core/iblock_0/{}/wwn/vpd_unit_serial", lun.name),
            &format!("Unit Serial Number (T10): {}\n", lun.uuid),
        );
        let observed = observe_iscsi(&tree.0, &spec);
        assert!(observed.devices[0].exported, "the LUN links the backstore");
        assert!(serial_holds(&observed.devices[0].unit_serial, &lun.uuid));
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(!text.contains("vpd_unit_serial"), "{text}");

        // A label nobody can parse over a serial that is genuinely WRONG is
        // still not written while the LUN is exported: the kernel would refuse
        // it and take the whole reconcile down. There is nothing to be done
        // about it here — the export is what has to go first.
        tree.attr(
            &format!("core/iblock_0/{}/wwn/vpd_unit_serial", lun.name),
            "something else entirely\n",
        );
        let observed = observe_iscsi(&tree.0, &spec);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(!text.contains("vpd_unit_serial"), "{text}");

        // And on a backstore nothing exports yet — the first apply — it IS
        // written, because that is the only moment the kernel accepts it.
        std::fs::remove_file(lun_dir.join(&lun.name)).expect("unlink");
        let observed = observe_iscsi(&tree.0, &spec);
        assert!(!observed.devices[0].exported);
        let text = render(&plan_iscsi(&spec, &observed).expect("plan"));
        assert!(text.contains(&format!("vpd_unit_serial = {}\n", lun.uuid)), "{text}");
    }

    #[test]
    fn dropping_an_nvme_host_unlinks_it_before_allow_any_host_is_written() {
        // MEASURED in both directions on a node: with a host still linked,
        // `attr_allow_any_host = 1` is EINVAL ("Can't set allow_any_host when
        // explicit hosts are set!"); after the unlink the same write succeeds.
        // Switching an authenticated subsystem to "no authentication"
        // therefore fails halfway unless the links go first — and the host
        // that lost its place keeps its access.
        let tree = TempTree::new("nvmet-prune");
        let open = nvmet(vec![], true);
        let sub_rel = format!("subsystems/{}", open.nqn);
        let host = "nqn.2014-08.org.nvmexpress:uuid:esx01";
        tree.dir(&format!("{sub_rel}/allowed_hosts"));
        std::os::unix::fs::symlink(
            tree.dir(&format!("hosts/{host}")),
            tree.0.join(&sub_rel).join("allowed_hosts").join(host),
        )
        .expect("allowed link");
        // A namespace the spec no longer names.
        tree.attr(&format!("{sub_rel}/namespaces/7/enable"), "1\n");

        let observed = observe_nvmet(&tree.0, &open);
        assert_eq!(observed.allowed_hosts, vec![host]);
        assert_eq!(observed.existing_namespaces, vec![7]);
        let text = render(&plan_nvmet(&open, &observed).expect("plan"));
        let unlink = text
            .find(&format!("unlink /sys/kernel/config/nvmet/{sub_rel}/allowed_hosts/{host}"))
            .expect("the host is unlinked");
        let allow = text
            .find("attr_allow_any_host = 1")
            .expect("allow_any_host is written");
        assert!(unlink < allow, "the unlink must come first:\n{text}");
        // And the host it dropped does not go on holding the key of an
        // allowlist it is no longer on. The key cannot be WRITTEN away —
        // obs. 37: every sentinel is EINVAL and the empty string is a silent
        // no-op — so the OBJECT goes, which obs. 24 showed is what makes the
        // attribute fresh again. Nothing else references it here.
        assert!(!text.contains(&format!("clear /sys/kernel/config/nvmet/hosts/{host}")), "{text}");
        assert!(
            text.contains(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host}")),
            "{text}"
        );
        // The stale namespace is disabled and then removed, in that order.
        let off = text.find(&format!("{sub_rel}/namespaces/7/enable = 0")).expect("disabled");
        let rm = text.find(&format!("rmdir /sys/kernel/config/nvmet/{sub_rel}/namespaces/7\n")).expect("removed");
        assert!(off < rm, "{text}");

        // THE OTHER DIRECTION, measured separately (obs. 31): linking a host
        // while `attr_allow_any_host` still reads 1 is refused too — "nvmet:
        // can't add hosts when allow_any_host is set!". So turning
        // authentication ON needs the flag cleared BEFORE the link, or the
        // subsystem stays open to everyone while the apply reports a host it
        // never attached.
        let authenticated = nvmet(
            vec![NvmetHost {
                nqn: host.to_string(),
                dhchap_key: "DHHC-1:00:abcd+/=:".to_string(),
                dhchap_ctrl_key: String::new(),
                dhchap_hash: "hmac(sha256)".to_string(),
                dhchap_dhgroup: "null".to_string(),
            }],
            false,
        );
        let mut observed = observe_nvmet(&tree.0, &authenticated);
        // The node currently allows anyone — the state this edit leaves.
        observed.allow_any_host = "1".to_string();
        let text = render(&plan_nvmet(&authenticated, &observed).expect("plan"));
        let clear = text
            .find("attr_allow_any_host = 0")
            .expect("the flag is cleared");
        // `\n`-anchored: `unlink …` contains `link …`, so an unanchored search
        // finds the removal and calls it the creation.
        let link = text
            .find(&format!("\nlink /sys/kernel/config/nvmet/{sub_rel}/allowed_hosts/{host}"))
            .expect("the host is linked");
        assert!(clear < link, "the flag must be cleared before the link:\n{text}");
    }

    #[test]
    fn dropping_a_host_never_removes_an_object_another_subsystem_still_authenticates_with() {
        // `hosts/<nqn>/` is NODE-WIDE and the DH-HMAC-CHAP key lives on it; a
        // subsystem only symlinks to it. So "this host left MY allowlist" says
        // nothing about whether the key is still authenticating somebody else.
        //
        // The topology is ordinary, not exotic: the UI exports one LUN per
        // target (§6.1), so two zvols handed to the same VMware host are two
        // targets carrying the same host NQN. Taking the object away because
        // it left one allowlist took the other one's client offline at its
        // next reconnect — silently, with that row still green.
        let tree = TempTree::new("nvmet-shared-host");
        let host = "nqn.2014-08.org.nvmexpress:uuid:esx01";
        let spec = nvmet(vec![], true);
        let other = "nqn.2026-09.local.tentaflow:helios.vm-b";

        // Our subsystem allows the host…
        let ours = tree.dir(&format!("subsystems/{}/allowed_hosts", spec.nqn));
        let host_dir = tree.dir(&format!("hosts/{host}"));
        std::os::unix::fs::symlink(&host_dir, ours.join(host)).expect("our link");
        // …and so does a SECOND subsystem on this node.
        let theirs = tree.dir(&format!("subsystems/{other}/allowed_hosts"));
        std::os::unix::fs::symlink(&host_dir, theirs.join(host)).expect("their link");

        let observed = observe_nvmet(&tree.0, &spec);
        assert_eq!(observed.shared_hosts, vec![host.to_string()]);
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        // The host leaves OUR allowlist…
        assert!(
            text.contains(&format!("unlink /sys/kernel/config/nvmet/subsystems/{}/allowed_hosts/{host}", spec.nqn)),
            "{text}"
        );
        // …and its object survives, because the key on it is not ours to take.
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host}")), "{text}");
        assert!(text.contains("still allows this host"), "the job log says what it left alone:\n{text}");

        // With the OTHER subsystem gone, the same plan removes the object —
        // which IS the clear, because a key cannot be written away (obs. 37)
        // and a recreated object comes back empty (obs. 24). This is the
        // counter-example that proves the guard is a condition and not a
        // blanket refusal.
        std::fs::remove_file(theirs.join(host)).expect("drop the second link");
        let observed = observe_nvmet(&tree.0, &spec);
        assert!(observed.shared_hosts.is_empty());
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        let unlink = text
            .find(&format!("unlink /sys/kernel/config/nvmet/subsystems/{}/allowed_hosts/{host}", spec.nqn))
            .expect("the unlink");
        let rmdir = text
            .find(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host}"))
            .expect("the object goes");
        // configfs will not remove an object something still links to, so the
        // link has to go first.
        assert!(unlink < rmdir, "the link goes before the object:\n{text}");
        // And no attempt to WRITE the key away: there is no value that does
        // that (obs. 37), and every candidate is a fatal step that would take
        // the plan down before `attr_allow_any_host`.
        assert!(!text.contains(&format!("clear /sys/kernel/config/nvmet/hosts/{host}")), "{text}");
    }

    #[test]
    fn writing_a_different_key_to_a_shared_host_is_refused_rather_than_silently_winning() {
        // The write side of the node-wide host object, and the one this used
        // to get wrong in the loudest possible way: the plan wrote the key
        // unconditionally, so saving target A changed the credential target
        // B's client was authenticating with — and the wizard promised in five
        // languages that it would not.
        //
        // One object holds one key. There is no apply that serves both, so the
        // node refuses instead of picking a loser and telling only the winner.
        let tree = TempTree::new("nvmet-key-conflict");
        let host_nqn = "nqn.2014-08.org.nvmexpress:uuid:esx01";
        let mine = NvmetHost {
            nqn: host_nqn.to_string(),
            dhchap_key: "DHHC-1:00:bbbb+/=:".to_string(),
            dhchap_ctrl_key: String::new(),
            dhchap_hash: "hmac(sha256)".to_string(),
            dhchap_dhgroup: "null".to_string(),
        };
        let spec = nvmet(vec![mine.clone()], false);
        // The object already holds ANOTHER key, and a second target links it.
        tree.attr(&format!("hosts/{host_nqn}/dhchap_key"), "DHHC-1:00:aaaa+/=:\n");
        let other = "nqn.2026-09.local.tentaflow:helios.vm-a";
        let theirs = tree.dir(&format!("subsystems/{other}/allowed_hosts"));
        std::os::unix::fs::symlink(tree.0.join("hosts").join(host_nqn), theirs.join(host_nqn))
            .expect("their link");

        let observed = observe_nvmet(&tree.0, &spec);
        assert_eq!(observed.shared_hosts, vec![host_nqn.to_string()]);
        assert!(observed.hosts_matching_spec.is_empty(), "the keys differ");
        assert_eq!(host_verdict(&observed, host_nqn), HostVerdict::SharedAndConflicts);
        let refused = plan_nvmet(&spec, &observed).expect_err("a key rotation cannot be one-sided");
        assert!(refused.to_string().contains(host_nqn), "{refused}");
        assert!(refused.to_string().contains("same key"), "{refused}");

        // The SAME key on the same shared host is the ordinary topology — two
        // zvols, one VMware client — and it applies, touching nothing.
        tree.attr(&format!("hosts/{host_nqn}/dhchap_key"), "DHHC-1:00:bbbb+/=:\n");
        let observed = observe_nvmet(&tree.0, &spec);
        assert_eq!(host_verdict(&observed, host_nqn), HostVerdict::SharedAndAgrees);
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        assert!(text.contains("already holds exactly this key"), "{text}");
        assert!(!text.contains(&format!("{host_nqn}/dhchap_key = ")), "the key is not rewritten:\n{text}");
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host_nqn}")), "{text}");
        // …and this target's own link is still made: it does allow the host.
        assert!(
            text.contains(&format!("\nlink /sys/kernel/config/nvmet/subsystems/{}/allowed_hosts/{host_nqn}", spec.nqn)),
            "{text}"
        );

        // With nobody else linking it, the key IS rewritten — the refusal is a
        // condition, not a blanket refusal to touch host objects.
        std::fs::remove_file(theirs.join(host_nqn)).expect("drop the other link");
        tree.attr(&format!("hosts/{host_nqn}/dhchap_key"), "DHHC-1:00:aaaa+/=:\n");
        let observed = observe_nvmet(&tree.0, &spec);
        assert_eq!(host_verdict(&observed, host_nqn), HostVerdict::Sole);
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        assert!(text.contains(&format!("{host_nqn}/dhchap_key = ***")), "{text}");
    }

    #[test]
    fn turning_authentication_off_recreates_the_host_object_unless_it_is_shared() {
        // "The admin turned authentication off" has to be true in the KERNEL,
        // not only in the UI: a host object that keeps its key keeps demanding
        // it, so a subsystem the wizard calls unauthenticated would refuse
        // every client that stopped sending one.
        //
        // And it cannot be written away — obs. 37: `"\n"`, `"NULL"`, `"0"`,
        // `" "` are each EINVAL and `""` is a silent no-op. So the object is
        // removed and recreated, which obs. 24 showed comes back with fresh
        // attributes.
        let tree = TempTree::new("nvmet-auth-off");
        let host_nqn = "nqn.2014-08.org.nvmexpress:uuid:esx01";
        let open = nvmet(
            vec![NvmetHost {
                nqn: host_nqn.to_string(),
                dhchap_key: String::new(),
                dhchap_ctrl_key: String::new(),
                dhchap_hash: String::new(),
                dhchap_dhgroup: String::new(),
            }],
            false,
        );
        // The host object as an earlier, authenticated apply left it.
        tree.attr(&format!("hosts/{host_nqn}/dhchap_key"), "DHHC-1:00:abcd+/=:\n");
        let ours = tree.dir(&format!("subsystems/{}/allowed_hosts", open.nqn));
        std::os::unix::fs::symlink(tree.0.join("hosts").join(host_nqn), ours.join(host_nqn))
            .expect("our link");

        let observed = observe_nvmet(&tree.0, &open);
        assert_eq!(observed.hosts_with_key, vec![host_nqn.to_string()]);
        let text = render(&plan_nvmet(&open, &observed).expect("plan"));
        let unlink = text
            .find(&format!("unlink /sys/kernel/config/nvmet/subsystems/{}/allowed_hosts/{host_nqn}", open.nqn))
            .expect("the stale link goes");
        let rmdir = text
            .find(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host_nqn}"))
            .expect("and the object with it");
        let mkdir = text
            .find(&format!("mkdir /sys/kernel/config/nvmet/hosts/{host_nqn}"))
            .expect("recreated empty");
        let link = text
            .find(&format!("\nlink /sys/kernel/config/nvmet/subsystems/{}/allowed_hosts/{host_nqn}", open.nqn))
            .expect("and allowed again");
        assert!(unlink < rmdir && rmdir < mkdir && mkdir < link, "{text}");
        assert!(!text.contains("clear /sys/kernel/config/nvmet/hosts/"), "{text}");

        // A host that holds NO key is left exactly alone — no churn on every
        // apply of an ordinary unauthenticated target.
        std::fs::remove_dir_all(tree.0.join("hosts").join(host_nqn)).expect("reset");
        tree.dir(&format!("hosts/{host_nqn}"));
        let observed = observe_nvmet(&tree.0, &open);
        assert!(observed.hosts_with_key.is_empty());
        let text = render(&plan_nvmet(&open, &observed).expect("plan"));
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host_nqn}")), "{text}");

        // And when the object IS shared, the apply is REFUSED rather than
        // reconciled. There is no correct answer: removing the object takes
        // the other target's client offline, and leaving it means this target
        // is not what its own row says ("no authentication" over a subsystem
        // the kernel still demands a key for). The node declines instead of
        // picking a loser and telling nobody.
        tree.attr(&format!("hosts/{host_nqn}/dhchap_key"), "DHHC-1:00:abcd+/=:\n");
        let other = "nqn.2026-09.local.tentaflow:helios.vm-b";
        let theirs = tree.dir(&format!("subsystems/{other}/allowed_hosts"));
        std::os::unix::fs::symlink(tree.0.join("hosts").join(host_nqn), theirs.join(host_nqn))
            .expect("their link");
        let observed = observe_nvmet(&tree.0, &open);
        let refused = plan_nvmet(&open, &observed).expect_err("a shared key cannot be dropped");
        assert!(refused.to_string().contains(host_nqn), "{refused}");
        assert!(refused.to_string().contains("shared node-wide"), "{refused}");

        // …but a shared host whose key ALREADY matches what this target wants
        // is fine, and is left alone. That is the ordinary VMware topology:
        // two zvols, two targets, one host, one key.
        std::fs::remove_dir_all(tree.0.join("hosts").join(host_nqn)).expect("reset");
        tree.dir(&format!("hosts/{host_nqn}"));
        let observed = observe_nvmet(&tree.0, &open);
        assert_eq!(observed.hosts_matching_spec, vec![host_nqn.to_string()]);
        let text = render(&plan_nvmet(&open, &observed).expect("plan"));
        assert!(text.contains("already holds exactly this key"), "{text}");
        assert!(!text.contains(&format!("rmdir /sys/kernel/config/nvmet/hosts/{host_nqn}")), "{text}");
    }

    #[test]
    fn a_subsystem_the_kernel_already_holds_is_not_rewritten() {
        // `attr_serial` and `attr_model` are -EINVAL once a controller has
        // connected (`subsys->subsys_discovered`), so a second apply of a
        // subsystem somebody is USING must not touch them.
        let tree = TempTree::new("nvmet-idempotent");
        let spec = nvmet(vec![], true);
        let sub_rel = format!("subsystems/{}", spec.nqn);
        tree.attr(&format!("{sub_rel}/attr_serial"), &format!("{}\n", spec.serial));
        tree.attr(&format!("{sub_rel}/attr_model"), "TentaNas\n");
        tree.attr(&format!("{sub_rel}/attr_allow_any_host"), "1\n");

        let observed = observe_nvmet(&tree.0, &spec);
        let text = render(&plan_nvmet(&spec, &observed).expect("plan"));
        assert!(!text.contains("attr_serial"), "{text}");
        assert!(!text.contains("attr_model"), "{text}");
        assert!(!text.contains("attr_allow_any_host"), "{text}");
    }

    #[test]
    fn a_credential_is_cleared_with_the_sentinel_its_subsystem_understands() {
        // The trap this exists for: a zero-length write NEVER reaches a
        // configfs store method (`flush_write_buffer` runs only for len > 0),
        // so writing "" would leave the old secret in the kernel while the plan
        // claimed to have removed it. LIO only accepts the literal "NULL";
        // nvmet needs an ordinary empty value that is still a non-empty write.
        let off = render(&plan_iscsi(&iscsi(IscsiAuth::default(), vec![]), &fresh()).expect("plan"));
        assert!(off.contains("clear /sys/kernel/config/target/iscsi/iqn.2026-09.pl.euvic:helios.vm-store/tpgt_1/auth/userid = NULL\n"), "{off}");
        assert!(off.contains("/tpgt_1/auth/password = NULL\n"), "{off}");
        assert!(off.contains("/tpgt_1/auth/password_mutual = NULL\n"), "{off}");
        // Never an empty write, anywhere in the plan.
        assert!(!off.contains("= \n"), "an empty write is in the plan:\n{off}");

        // One-way CHAP still clears the mutual half it does not use.
        let mut one_way = mutual_chap();
        one_way.mutual = false;
        let text = render(&plan_iscsi(&iscsi(one_way, vec![]), &fresh()).expect("plan"));
        assert!(text.contains("/tpgt_1/auth/userid_mutual = NULL\n"), "{text}");
        assert!(text.contains("/tpgt_1/auth/password_mutual = NULL\n"), "{text}");

        // nvmet, the other subsystem — and the case round 1 got backwards.
        let host = NvmetHost {
            nqn: "nqn.2014-08.org.nvmexpress:uuid:1b4e28ba".into(),
            ..Default::default()
        };
        // nvmet has NO such sentinel — MEASURED (obs. 37): `"\n"`, `"NULL"`,
        // `"0"` and `" "` are each EINVAL on `hosts/<nqn>/dhchap_key`, and the
        // empty string is accepted while changing nothing. Round 1 asserted
        // the opposite and nobody had measured it. So the plan never emits a
        // clear on an nvmet host at all: a key stops existing by its OBJECT
        // ceasing to exist.
        let mut spec = nvmet(vec![host], false);
        spec.allow_any_host = false;
        let text = render(&plan_nvmet(&spec, &fresh_nvmet(1, 1)).expect("plan"));
        assert!(!text.contains("clear /sys/kernel/config/nvmet/"), "{text}");

        // And the applier refuses an empty write outright, so the rule cannot
        // be bypassed by a future caller reaching for `Write` with "".
        let tree = TempTree::new("attr");
        tree.attr("auth/password", "stary-sekret");
        let path = tree.0.join("auth/password");
        let err = write_attr(&path, "").expect_err("an empty write is refused");
        assert!(err.contains("Clear step"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "stary-sekret");
        // A real value arrives verbatim: LIO stores the bytes as they come, so
        // a trailing newline would end up inside the credential.
        write_attr(&path, "vmware01").expect("written");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "vmware01");
        // The sentinel is an ordinary non-empty write.
        apply_plan(&[ConfigfsStep::clear(path.display().to_string(), LIO_CLEAR)]).expect("cleared");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "NULL");
    }

    #[test]
    fn a_note_is_not_a_configfs_step_and_has_no_path() {
        // Two things the job log gets wrong if `Note` is treated like the
        // others. "(N configfs steps)" is the number an admin reads as "what
        // the node did in the kernel" — a note did nothing — and `path()` is
        // what an error message names, where a sentence of prose is not a
        // path.
        let plan = vec![
            ConfigfsStep::Note("this host is shared".to_string()),
            ConfigfsStep::Mkdir("/sys/kernel/config/nvmet/hosts/x".to_string()),
            ConfigfsStep::Protect("/sys/kernel/config/nvmet/hosts/x/dhchap_key".to_string()),
        ];
        assert_eq!(kernel_step_count(&plan), 2, "the note is not a step");
        assert_eq!(plan[0].path(), "");
        assert_eq!(plan[1].path(), "/sys/kernel/config/nvmet/hosts/x");
        // …and it still reaches the log, which is the whole point of it.
        assert!(render(&plan).contains("this host is shared"), "{}", render(&plan));
    }

    #[test]
    fn no_assertion_in_this_file_searches_for_an_unanchored_link_verb() {
        // `render` has exactly two verbs where one is a PREFIX of the other:
        // `link {a} -> {b}` and `unlink {p}`. So `text.contains("link /sys/…")`
        // matches the REMOVAL of the very thing it claims to assert the
        // creation of — and every such assertion passes as long as the fixture
        // happens to produce no unlink for that path.
        //
        // Three rounds in a row shipped this shape and three rounds in a row
        // an audit missed some of it, so it stops being an audit item and
        // becomes a test. Anchor with `"\nlink "` (or assert the whole
        // rendered line) and this passes.
        let src = include_str!("block.rs");
        let mut unanchored = Vec::new();
        for (n, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            // A string literal that starts the word `link` right after the
            // quote (or after `format!(`), i.e. not preceded by `\n` or `un`.
            for opener in ["contains(\"link ", "find(\"link ", "contains(&format!(\"link ", "find(&format!(\"link "] {
                if line.contains(opener) {
                    unanchored.push(format!("block.rs:{}: {}", n + 1, trimmed));
                }
            }
        }
        assert!(
            unanchored.is_empty(),
            "these assertions also match a rendered `unlink …`:\n{}",
            unanchored.join("\n")
        );
        // …and the reason they would: proven here rather than asserted by
        // prose, so the test explains itself if it ever fires. The needle is
        // BUILT rather than written as a literal, because a literal would be
        // caught by the scan above — which is itself the point.
        let path = "/sys/kernel/config/nvmet/ports/1/subsystems/x";
        let rendered = render(&[ConfigfsStep::Unlink(path.to_string())]);
        let bare = format!("link {path}");
        assert!(rendered.contains(&bare), "an unlink line contains a bare `link …`");
        assert!(!rendered.contains(&format!("\n{bare}")), "and anchoring tells them apart");
    }

    #[test]
    fn a_secret_attribute_is_locked_down_and_the_apply_says_so_when_it_is_not() {
        // nvmet's `nvmet_host_dhchap_key_show` has no CAP_SYS_ADMIN gate (LIO's
        // auth attributes do), and configfs attributes are 0644 — MEASURED by
        // reading the key out of a live node from an unprivileged account, so
        // this chmod is the only thing between the key and every local user.
        use std::os::unix::fs::PermissionsExt;
        let tree = TempTree::new("secret");
        tree.attr("hosts/x/dhchap_key", "placeholder");
        let path = tree.0.join("hosts/x/dhchap_key");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
        let warnings = apply_plan(&[
            ConfigfsStep::secret(path.display().to_string(), "DHHC-1:00:abcd+/=:"),
            ConfigfsStep::Protect(path.display().to_string()),
        ])
        .expect("written");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a written secret is not world-readable");
        assert!(warnings.is_empty(), "a chmod that worked says nothing: {warnings:?}");

        // The mode is CHECKED, not assumed. The UI tells the admin in five
        // languages that the key is protected, so an apply that could not
        // deliver that has to say so instead of returning green — which is
        // what `let _ = set_permissions` used to do.
        let missing = tree.0.join("hosts/x/nonexistent_key");
        let warnings =
            apply_plan(&[ConfigfsStep::Protect(missing.display().to_string())]).expect("no error");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("0600"), "{warnings:?}");

        // A non-secret attribute keeps whatever mode the kernel gave it.
        tree.attr("hosts/x/dhchap_hash", "x");
        let plain = tree.0.join("hosts/x/dhchap_hash");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).expect("mode");
        apply_plan(&[ConfigfsStep::write(plain.display().to_string(), "hmac(sha256)")]).expect("written");
        let mode = std::fs::metadata(&plain).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn every_secret_the_plan_writes_is_protected_even_when_nothing_about_it_changed() {
        // MEASURED (run 4): the 0600 SURVIVES the next write to the attribute,
        // but NOT the object being recreated — a fresh `mkdir` of the host
        // makes a fresh 0644 attribute. So the chmod belongs to "this object
        // holds a secret", never to "this secret changed": if it rode along on
        // the write, a host object created before this step existed would keep
        // its 0644 for as long as its key stayed the same, which is forever.
        //
        // This is the case the observation layer makes possible and therefore
        // the case that has to be pinned: a subsystem the kernel ALREADY holds,
        // where the diff has nothing to write, still protects the key.
        let host = NvmetHost {
            nqn: "nqn.2014-08.org.nvmexpress:uuid:9f8a".to_string(),
            dhchap_key: "DHHC-1:00:abcd+/=:".to_string(),
            dhchap_ctrl_key: "DHHC-1:00:efgh+/=:".to_string(),
            dhchap_hash: "hmac(sha256)".to_string(),
            dhchap_dhgroup: "null".to_string(),
        };
        let spec = nvmet(vec![host.clone()], false);
        let mut observed = fresh_nvmet(1, 1);
        // Everything the plan could compare already matches.
        observed.serial = spec.serial.clone();
        observed.model = NVMET_MODEL.to_string();
        observed.allow_any_host = "0".to_string();
        observed.ports[0].configured = true;
        observed.linked_ports = vec![1];
        observed.namespaces[0] = NvmetNsObserved {
            nsid: 1,
            enabled: true,
            matches: true,
        };
        observed.allowed_hosts = vec![host.nqn.clone()];
        let plan = plan_nvmet(&spec, &observed).expect("plan");
        let text = render(&plan);
        assert!(!text.contains("attr_serial"), "nothing that matches is rewritten:\n{text}");
        assert!(
            text.contains(&format!("protect {NVMET_CONFIGFS}/hosts/{}/dhchap_key = 0600\n", host.nqn)),
            "{text}"
        );

        // And the rule itself: no plan may write a secret without protecting it.
        let mut specs: Vec<Vec<ConfigfsStep>> = vec![plan];
        specs.push(plan_nvmet(&nvmet(vec![host], false), &fresh_nvmet(1, 1)).expect("plan"));
        specs.push(
            plan_iscsi(
                &iscsi(mutual_chap(), vec!["iqn.1998-01.com.vmware:esx01".to_string()]),
                &fresh(),
            )
            .expect("plan"),
        );
        let mut secrets_seen = 0;
        for plan in specs {
            let protected: Vec<&str> = plan
                .iter()
                .filter_map(|s| match s {
                    ConfigfsStep::Protect(p) => Some(p.as_str()),
                    _ => None,
                })
                .collect();
            for step in &plan {
                if let ConfigfsStep::Write { path, secret: true, .. } = step {
                    secrets_seen += 1;
                    assert!(
                        protected.contains(&path.as_str()),
                        "{path} is written as a secret and never protected"
                    );
                }
            }
        }
        // The guard the loop needs: "every secret is protected" is vacuously
        // true of a plan with no secrets, so a change that stopped marking
        // credentials as secrets would have passed this silently. Three plans,
        // both protocols, at least one credential each.
        assert!(
            secrets_seen >= 3,
            "the rule was checked against {secrets_seen} secret writes — it has to see some"
        );
    }
}
