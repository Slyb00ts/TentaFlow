// =============================================================================
// File: tentanas/disks.rs — disk inventory, live I/O, SMART/NVMe health and
//       the sampler (plan-02 §5.4, tab "Dyski"). Sources, all JSON or /proc:
//
//       lsblk -J -b        identity, size, transport, partitions, mounts
//       /proc/diskstats    I/O counters every tick → rates, latency, util
//       smartctl --json=c  health, temperature, attributes, self-test log
//                          (privileged: goes through the broker)
//
//       The live picture lives in memory (one sampler per node); minute
//       samples and the raw SMART document go to tentanas.db so the detail
//       view can show 24 h / 7 d history and the attribute trend.
// =============================================================================

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use serde_json::Value;
use tentaflow_protocol::tentanas::{
    NasDisk, NasDiskIo, NasDiskWipeJournalClaim, NasDiskWipePlan, NasDiskWipeRefusal,
    NasHealthReason, NasReplacementAdvice, NasSmartAttribute, NasSmartSelfTest, NasTelemetryState,
};
use tentanas_helper::HelperCommand;

use super::broker;
use super::db::{self as store, DiskIdentity, SampleInsert};
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;

const TICK: Duration = Duration::from_secs(5);
const INVENTORY_EVERY: Duration = Duration::from_secs(30);
const SAMPLE_EVERY: Duration = Duration::from_secs(60);
const SMART_EVERY: Duration = Duration::from_secs(30 * 60);
const PRUNE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const SUMMARY_EVERY: Duration = Duration::from_secs(60);
/// A `wipefs` of one disk is a handful of small writes and a re-read, so the
/// only way it takes minutes is a device that has stopped answering — which
/// is precisely the case the timeout has to end rather than wait out.
const WIPE_TIMEOUT: Duration = Duration::from_secs(180);
/// Points of the per-row sparkline (one per tick → five minutes).
const HISTORY_POINTS: usize = 60;
/// Ticks the IOPS baseline of the Overview tile averages over — one hour.
/// It is a live indicator, not history, so it stays in the sampler's memory:
/// the minute samples in `tentanas.db` carry throughput and latency, never
/// per-direction operation rates.
const IOPS_BASELINE_POINTS: usize = 3600 / TICK.as_secs() as usize;

// ----- lsblk -------------------------------------------------------------------

const LSBLK_COLUMNS: &str =
    "NAME,PATH,TYPE,MODEL,SERIAL,WWN,SIZE,TRAN,ROTA,RM,REV,VENDOR,MOUNTPOINTS,FSTYPE,LABEL,UUID";

/// Device name prefixes that are not physical disks (virtual, optical,
/// arrays and volumes built ON disks — those belong to the pool views).
const SKIP_PREFIXES: &[&str] = &["loop", "ram", "zram", "zd", "dm-", "sr", "fd", "md", "nbd", "drbd"];

/// Older util-linux emits every JSON value as a string; newer ones type them.
fn json_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn json_u64(v: &Value) -> u64 {
    match v {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn json_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.trim(), "1" | "true"),
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':') { c } else { '_' })
        .collect()
}

/// Stable identity across reboots and name changes: WWN, then serial, then
/// (virtual disks without either) the kernel name.
fn disk_id(name: &str, serial: Option<&str>, wwn: Option<&str>) -> String {
    if let Some(w) = wwn {
        return format!("wwn-{}", sanitize_id(w.trim_start_matches("0x")));
    }
    if let Some(s) = serial {
        return format!("sn-{}", sanitize_id(s));
    }
    format!("dev-{}", sanitize_id(name))
}

#[derive(Debug, Default)]
struct Usage {
    mountpoints: Vec<String>,
    zfs_pool: Option<String>,
    raid_member: Option<String>,
    has_children_or_fs: bool,
    /// First filesystem signature seen that is neither a ZFS nor a RAID member
    /// label. `has_children_or_fs` only says THAT something occupies the disk;
    /// this says what, so the "used" role can name it instead of being a
    /// catch-all the reader cannot act on.
    fs_hint: Option<String>,
    /// UUID of the very signature `fs_hint` names — the two are set together
    /// and never separately, because a disk reporting one filesystem's type
    /// beside another's UUID is what an import would then adopt.
    fs_uuid: Option<String>,
    /// LABEL of that same signature, set with the other two for the same
    /// reason. It is the one human-readable name a bare filesystem carries,
    /// and the wipe plan needs it: "d3" tells an admin which Elastic branch a
    /// leftover xfs used to be, while "xfs" does not.
    fs_label: Option<String>,
}

fn collect_usage(node: &Value, usage: &mut Usage) {
    if let Some(mps) = node.get("mountpoints").and_then(Value::as_array) {
        usage
            .mountpoints
            .extend(mps.iter().filter_map(json_str));
    }
    let fstype = node.get("fstype").and_then(json_str);
    let label = node.get("label").and_then(json_str);
    match fstype.as_deref() {
        Some("zfs_member") => usage.zfs_pool = usage.zfs_pool.take().or(label),
        Some("linux_raid_member") => usage.raid_member = usage.raid_member.take().or(label),
        Some(other) => {
            usage.has_children_or_fs = true;
            // Keep the OUTERMOST signature: collect_usage recurses into
            // children afterwards, so the first one seen is the disk's own or
            // its first partition's, which is what the role should name.
            if usage.fs_hint.is_none() {
                usage.fs_hint = Some(other.to_string());
                usage.fs_uuid = node.get("uuid").and_then(json_str);
                usage.fs_label = label;
            }
        }
        None => {}
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        usage.has_children_or_fs = true;
        for c in children {
            collect_usage(c, usage);
        }
    }
}

fn role_of(usage: &Usage) -> (&'static str, Option<String>) {
    if usage
        .mountpoints
        .iter()
        .any(|m| matches!(m.as_str(), "/" | "/boot" | "/boot/efi" | "[SWAP]"))
    {
        return ("system", None);
    }
    if let Some(pool) = &usage.zfs_pool {
        return ("pool_member", Some(pool.clone()));
    }
    if let Some(array) = &usage.raid_member {
        return ("array_member", Some(array.clone()));
    }
    if !usage.mountpoints.is_empty() {
        return ("mounted", None);
    }
    if usage.has_children_or_fs {
        return ("used", None);
    }
    ("free", None)
}

/// Builds the identity part of a `NasDisk` from one lsblk block device.
pub fn disk_from_lsblk(node: &Value) -> Option<NasDisk> {
    let name = node.get("name").and_then(json_str)?;
    if node.get("type").and_then(json_str).as_deref() != Some("disk") {
        return None;
    }
    if SKIP_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return None;
    }
    let serial = node.get("serial").and_then(json_str);
    let wwn = node.get("wwn").and_then(json_str);
    let rotational = json_bool(node.get("rota").unwrap_or(&Value::Null));
    let transport = node
        .get("tran")
        .and_then(json_str)
        .unwrap_or_else(|| if name.starts_with("nvme") { "nvme".into() } else { "unknown".into() });
    let kind = if name.starts_with("nvme") || transport == "nvme" {
        "nvme"
    } else if rotational {
        "hdd"
    } else {
        "ssd"
    };
    let mut usage = Usage::default();
    collect_usage(node, &mut usage);
    let (role, member_of) = role_of(&usage);
    let vendor = node.get("vendor").and_then(json_str);
    let model = node.get("model").and_then(json_str).unwrap_or_default();
    let model = match vendor {
        // lsblk reports ATA disks with vendor "ATA" — the model already
        // names the maker there.
        Some(v) if !model.starts_with(&v) && v != "ATA" => format!("{v} {model}"),
        _ => model,
    };
    Some(NasDisk {
        disk_id: disk_id(&name, serial.as_deref(), wwn.as_deref()),
        path: node
            .get("path")
            .and_then(json_str)
            .unwrap_or_else(|| format!("/dev/{name}")),
        name,
        kind: kind.to_string(),
        model,
        serial: serial.unwrap_or_default(),
        wwn,
        size_bytes: json_u64(node.get("size").unwrap_or(&Value::Null)),
        transport,
        rotational,
        removable: json_bool(node.get("rm").unwrap_or(&Value::Null)),
        firmware: node.get("rev").and_then(json_str),
        role: role.to_string(),
        member_of,
        health: "unknown".to_string(),
        health_reason: String::new(),
        health_reasons: Vec::new(),
        temperature_c: None,
        power_on_hours: None,
        reallocated_sectors: None,
        pending_sectors: None,
        crc_errors: None,
        media_errors: None,
        wear_pct: None,
        smart_available: false,
        smart_passed: None,
        smart_read_at: None,
        io: NasDiskIo::default(),
        io_history_bps: Vec::new(),
        mountpoints: usage.mountpoints,
        fs_type: usage.fs_hint,
        fs_uuid: usage.fs_uuid,
        fs_label: usage.fs_label,
        // lsblk knows the pool from the member label, never the vdev inside
        // it; `refresh_inventory` fills these from `zpool status`.
        vdev_role: String::new(),
        vdev_kind: String::new(),
        // An Elastic Array leaves NO signature on its disks — the union is a
        // mergerfs mount over ordinary per-disk filesystems — so lsblk cannot
        // see this either; `refresh_inventory` fills it from this node's own
        // array rows.
        array_role: String::new(),
    })
}

// =============================================================================
// Clearing a disk: the plan
// =============================================================================
//
// Read-only, and the whole point of it is the REASON. A disk that reads as
// `used` on the Disks tab cannot be put into a pool or an array, and until
// this existed the only way to free one was to build an array on it. What an
// admin needs before erasing 32 TB is not a yes/no but a sentence naming what
// occupies the disk and what to do about it.
//
// NONE of these checks is a safety guarantee, and the code must not pretend
// otherwise. `lsblk` — which is all `NasDisk` is built from — cannot see an
// Elastic Array branch at all: the branches are mounted inside the union
// process's own mount namespace and only the union is published to the host.
// On a live node `grep -c xfs /proc/mounts` read 0 while nine branches were
// mounted. The guarantee is the exclusive open of the block device on the
// privileged side, which the kernel refuses for a filesystem mounted in ANY
// namespace. Everything here exists to explain a refusal BEFORE the kernel
// has to produce it.

/// The Elastic journal that still claims a disk, with the one fact the
/// protocol struct does not carry: whether the array is serving right now.
///
/// Read from the node's journals (`0700 root`), which is the only place a
/// dissolved array still exists — a dissolve keeps the journal so the array
/// import can take the array back, and the database row is gone.
pub struct JournalClaim {
    pub claim: NasDiskWipeJournalClaim,
    /// The union is published on the host, or the journal still holds a
    /// namespace anchor. `None` means the mount table could not be read:
    /// unknown, and unknown is never "free".
    pub serving: Option<bool>,
}

/// The wipe refusal for a disk that another organisation of this node still
/// claims through its Elastic journal. A code the dialog words in the admin's
/// language, because the detail beside it is the node's sentence.
pub const JOURNAL_OTHER_ORG: &str = "journal_other_org";

/// The role a disk of ANOTHER organisation's live Elastic Array is shown
/// under (`hide_other_org_array`). The disk inventory is the node's — every
/// tenant's Disks tab reads the same lsblk scan and the same array rows — so
/// the membership `apply_array_membership` fills in names every tenant's
/// arrays. The disk stays visible (it is shared hardware, and hiding it would
/// look like a free disk), but only as "in another organisation's array".
pub const ROLE_OTHER_ORG_ARRAY: &str = "other_org_array";

/// Turns a disk of an Elastic Array that `own_arrays` (the asking
/// organisation's array names) does not hold into a disk of "another
/// organisation's array": no array name, no part in it, no branch filesystem,
/// no mount path. Applied per request, on the copy that leaves the server —
/// the shared inventory keeps the real membership, which the node's own
/// checks (pool eligibility, array claims) need whoever asks.
///
/// The role rewrite below only touches an Elastic member (`array_role` set):
/// a ZFS pool or an mdraid set is named by a label ON the disk, which any
/// admin of the node can read with lsblk, and neither is owned by a tenant.
/// But a branch mountpoint is scrubbed off EVERY disk regardless of role,
/// because a member whose disk id has drifted stops being one (see
/// `own_branch_mount`) while its mount still names the array by path.
///
/// EVERY field of `NasDisk` was weighed against "can it carry the array's
/// name or the array's shape", and this is the list of what goes:
/// - `member_of`: the array's name itself.
/// - `mountpoints`: a branch is mounted at
///   `/mnt/tentanas-branches/<array>/{data,cache}/<disk>` or `…/parity/<n>`
///   (`tentanas_helper::elastic::BRANCH_ROOT`), and lsblk reports that path
///   for the disk and its partitions. ALL of them go, not only the ones under
///   the branch root: every mount of another tenant's member is that
///   tenant's, its path is chosen by it, and "mounted" is already said by
///   the role and refused by the wipe plan.
/// - `array_role`: the disk's part in the array (data/cache/parity), which
///   says how that tenant built it.
/// - `fs_type` / `fs_label` / `fs_uuid`: the branch filesystem; the label is
///   the branch name and the UUID is the array's `expected_uuid`.
/// - `vdev_role` / `vdev_kind`: empty for an Elastic member (they describe a
///   ZFS vdev of `member_of`), cleared so no future filler can put the
///   array's layout there.
///
/// Kept, because they are the shared HARDWARE every admin of the node reads
/// with lsblk/smartctl and none is written from an array: `disk_id`, `name`,
/// `path` (`/dev/…`), `kind`, `model`, `serial`, `wwn`, `size_bytes`,
/// `transport`, `rotational`, `removable`, `firmware`, the SMART figures
/// (`health`, `temperature_c`, `power_on_hours`, `reallocated_sectors`,
/// `pending_sectors`, `crc_errors`, `media_errors`, `wear_pct`,
/// `smart_available`, `smart_passed`, `smart_read_at`), `health_reason`
/// and `health_reasons` (built by `score_health` from SMART counters, plus
/// `grade_disk_health`'s FAULTED/UNAVAIL code, which names no pool; no code
/// carries a pool or array name as a parameter either) and the I/O figures
/// (`io`, `io_history_bps`: numbers). `role` stays, rewritten to
/// `ROLE_OTHER_ORG_ARRAY`.
pub fn hide_other_org_array(disk: &mut NasDisk, own_arrays: &std::collections::BTreeSet<String>) {
    // A branch mount names its array by PATH alone
    // (`/mnt/tentanas-branches/<array>/{data,cache}/<disk>` or
    // `…/parity/<n>`), so it leaks even off an `array_member`: a disk whose
    // id drifted (a WWN that stops reporting behind a new HBA or USB bridge)
    // falls out of `array_membership` entirely and is reported as plain
    // "mounted", but lsblk still shows where it is mounted. Every mountpoint
    // under another organisation's branch is dropped here, whatever this
    // disk's role turns out to be.
    disk.mountpoints.retain(|path| own_branch_mount(path, own_arrays));

    if disk.role != "array_member" || disk.array_role.is_empty() {
        return;
    }
    if disk.member_of.as_ref().is_some_and(|name| own_arrays.contains(name)) {
        return;
    }
    disk.role = ROLE_OTHER_ORG_ARRAY.to_string();
    disk.member_of = None;
    disk.array_role.clear();
    disk.mountpoints.clear();
    disk.fs_uuid = None;
    disk.fs_label = None;
    disk.fs_type = None;
    disk.vdev_role.clear();
    disk.vdev_kind.clear();
}

/// Whether `path` may be shown to this caller: anything outside the Elastic
/// branch root is shared hardware or a ZFS/mdraid mount, named by a label ON
/// the disk rather than by an org, so it is always kept. A branch mount is
/// kept only when its `<array>` segment is one of `own_arrays` — otherwise
/// it names another organisation's array and must go, exactly like
/// `member_of` above.
fn own_branch_mount(path: &str, own_arrays: &std::collections::BTreeSet<String>) -> bool {
    match path.strip_prefix(tentanas_helper::elastic::BRANCH_ROOT) {
        Some(rest) => rest.split('/').next().is_some_and(|array| own_arrays.contains(array)),
        None => true,
    }
}

fn refuse(code: &str, detail: String) -> NasDiskWipeRefusal {
    NasDiskWipeRefusal { code: code.to_string(), detail }
}

/// The one refusal for a disk another organisation of this node holds, live
/// or through its journal alike. It names no array — the caller may not know
/// it — and says only what this organisation cannot do.
fn refuse_other_org(name: &str) -> NasDiskWipeRefusal {
    refuse(
        JOURNAL_OTHER_ORG,
        format!(
            "{name}: dysk należy do macierzy Elastic innej organizacji na tym węźle — \
             ta organizacja nie może go wyczyścić ani przejąć"
        ),
    )
}

/// Everything clearing `disk` would remove and every reason the node would
/// refuse. Pure: it reads no device, runs no tool and changes nothing.
pub fn plan_wipe(disk: &NasDisk, journal: Option<JournalClaim>) -> NasDiskWipePlan {
    let name = disk.name.as_str();
    let mounts = || disk.mountpoints.join(", ");
    let mut refusals = Vec::new();
    match disk.role.as_str() {
        "system" => refusals.push(refuse(
            "system",
            format!(
                "{name}: to dysk systemowy tego noda ({}) — nie ma operacji, która go zwalnia; \
                 przenieś system na inny nośnik, jeśli ten dysk ma być wolny",
                mounts()
            ),
        )),
        "pool_member" => refusals.push(refuse(
            "zfs_pool",
            format!(
                "{name}: dysk należy do puli ZFS {} — zniszcz pulę albo odłącz od niej ten dysk, \
                 a potem wyczyść go ponownie",
                disk.member_of.as_deref().unwrap_or("?")
            ),
        )),
        "array_member" if disk.array_role.is_empty() => refusals.push(refuse(
            "mdraid",
            format!(
                "{name}: dysk jest członkiem macierzy mdraid {} — rozłóż macierz \
                 (mdadm --stop) i wyczyść dysk ponownie",
                disk.member_of.as_deref().unwrap_or("?")
            ),
        )),
        // `array_role` is what separates the two owners that share this role:
        // it is set only for a member of an Elastic Array this node has a
        // record of, and empty for an mdraid member, whose label `lsblk`
        // reads off the disk itself.
        "array_member" => refusals.push(refuse(
            "elastic_member",
            format!(
                "{name}: dysk należy do macierzy Elastic {} jako {} — rozwiąż macierz, a potem \
                 wyczyść jej dyski",
                disk.member_of.as_deref().unwrap_or("?"),
                disk.array_role
            ),
        )),
        // Another organisation's LIVE array (`hide_other_org_array` ran on
        // this disk before the plan): refused like its journal would be, and
        // without the name the `elastic_member` sentence above would print.
        ROLE_OTHER_ORG_ARRAY => refusals.push(refuse_other_org(name)),
        "mounted" => refusals.push(refuse(
            "mounted",
            format!(
                "{name}: dysk ma zamontowany system plików ({}) — odmontuj go i wyczyść ponownie",
                mounts()
            ),
        )),
        _ => (),
    }
    // A mount the role did not already refuse. `role_of` reports 'system' or
    // 'mounted' for a disk with mountpoints, so this only fires for one the
    // inventory classified by a signature while something still has it
    // mounted — and it is then the last legible warning before the kernel's.
    if refusals.is_empty() && !disk.mountpoints.is_empty() {
        refusals.push(refuse(
            "mounted",
            format!(
                "{name}: dysk ma zamontowany system plików ({}) — odmontuj go i wyczyść ponownie",
                mounts()
            ),
        ));
    }
    let claim = journal.and_then(|journal| match journal.serving {
        // Another tenant's array, served or dissolved alike: refused, and
        // refused WITHOUT its name — the claim carries none by now
        // (`journal_claim_of`). The owner's rule is that a tenant may not
        // destroy what it may not adopt, and it may not adopt this.
        _ if journal.claim.owner_kind == super::elastic::OWNER_KIND_OTHER_ORG_ON_NODE => {
            // A live array of that organisation has already been refused by
            // its role; one sentence says it once.
            if !refusals.iter().any(|r| r.code == JOURNAL_OTHER_ORG) {
                refusals.push(refuse_other_org(name));
            }
            None
        }
        Some(true) => {
            refusals.push(refuse(
                "journal_serving",
                format!(
                    "{name}: dysk należy do macierzy Elastic {}, która nadal udostępnia unię na \
                     tym nodzie — rozwiąż macierz, a potem wyczyść jej dyski",
                    journal.claim.name
                ),
            ));
            None
        }
        None => {
            refusals.push(refuse(
                "journal_unknown",
                format!(
                    "{name}: nie udało się odczytać, czy macierz Elastic {} jest jeszcze \
                     udostępniana — nieznany stan nie jest stanem wolnym; powtórz plan, gdy \
                     node odpowie",
                    journal.claim.name
                ),
            ));
            None
        }
        Some(false) => Some(journal.claim),
    });
    NasDiskWipePlan {
        disk_id: disk.disk_id.clone(),
        name: disk.name.clone(),
        path: disk.path.clone(),
        size_bytes: disk.size_bytes,
        model: disk.model.clone(),
        serial: disk.serial.clone(),
        fs_type: disk.fs_type.clone(),
        fs_label: disk.fs_label.clone(),
        fs_uuid: disk.fs_uuid.clone(),
        // Never for another tenant's member: `hide_other_org_array` has
        // cleared them already, and the plan repeats the rule on its own
        // because a branch mount path is `/mnt/tentanas-branches/<array>/…`
        // and the dialog prints this list ("zamontowany w …").
        mountpoints: if disk.role == ROLE_OTHER_ORG_ARRAY {
            Vec::new()
        } else {
            disk.mountpoints.clone()
        },
        allowed: refusals.is_empty(),
        refusals,
        journal_claim: claim,
    }
}

/// Why a wipe request was refused before anything privileged ran. The two
/// cases are kept apart because they mean different things to the caller: a
/// mismatch is the REQUEST being wrong (a stale dialog, a hand-built frame),
/// while the node's own state is the DISK being unavailable, and only the
/// second is worth showing an admin as a condition to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WipeRefusal {
    BadRequest(String),
    NotAvailable(String),
}

/// The wipe request turned into the ONE privileged command that performs it.
///
/// Pure over the plan the node just read for itself, so every gate between
/// "an admin clicked" and "a device is opened" is a decision a test can make
/// the node take. It is also the only place a `HelperCommand::DiskWipe` is
/// ever built: the plan request cannot reach it, which is what makes the plan
/// read-only by construction rather than by review.
///
/// `plan` must be the node's OWN plan, never one a client carried back — the
/// device set moves, and a plan is a statement about the moment it was read.
pub fn wipe_command(
    plan: &NasDiskWipePlan,
    disk: &NasDisk,
    confirm_device: &str,
    release_journal_array: &str,
) -> Result<HelperCommand, WipeRefusal> {
    // The retype gate, against the PLAN's device name rather than anything the
    // request carried: a request naming a device that has since been renamed
    // fails here instead of reaching whatever answers to that name now.
    if confirm_device != plan.name {
        return Err(WipeRefusal::BadRequest(format!(
            "Przepisana nazwa urządzenia nie zgadza się z {}",
            plan.name
        )));
    }
    if plan.disk_id != disk.disk_id || plan.path != disk.path {
        return Err(WipeRefusal::BadRequest(
            "Plan dotyczy innego dysku niż ten, który node ma teraz w inwentarzu".to_string(),
        ));
    }
    if !plan.refusals.is_empty() {
        return Err(WipeRefusal::NotAvailable(
            plan.refusals
                .iter()
                .map(|r| r.detail.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    // The SECOND acknowledgement, deliberately not a boolean: it names the
    // array, so a client that never showed the claim cannot satisfy it, and an
    // acknowledgement meant for one array cannot release another's journal.
    let release_journal = match (&plan.journal_claim, release_journal_array) {
        (None, "") => None,
        (None, _) => {
            return Err(WipeRefusal::BadRequest(
                "Żaden dziennik macierzy Elastic nie rezerwuje tego dysku — odczytaj plan ponownie"
                    .to_string(),
            ))
        }
        (Some(claim), ack) if ack == claim.name => Some(claim.array_id.clone()),
        (Some(claim), _) => {
            return Err(WipeRefusal::NotAvailable(format!(
                "Dziennik rozwiązanej macierzy Elastic {} nadal rezerwuje ten dysk — potwierdź \
                 utratę tej macierzy albo przywróć ją importem macierzy",
                claim.name
            )))
        }
    };
    Ok(HelperCommand::DiskWipe {
        device: disk.path.clone(),
        // Identity beside the path, because the path is not one: the
        // privileged side re-resolves the disk from these three and refuses if
        // the node now answers on a different device.
        wwn: disk.wwn.clone().filter(|w| !w.is_empty()),
        serial: (!disk.serial.is_empty()).then(|| disk.serial.clone()),
        bytes: disk.size_bytes,
        release_journal,
    })
}

/// Runs the wipe and reports what it removed.
///
/// A job rather than a direct answer for two reasons that are not cosmetic:
/// the step log IS the audit trail of the one operation in the product that
/// erases a disk outside a create, and the Disks tab has to show the disk as
/// `free` afterwards — which only a fresh inventory can say, and saying it
/// here means the admin does not have to wait out a poll to learn whether the
/// disk actually came back.
pub async fn wipe_job(
    h: super::jobs::JobHandle,
    command: HelperCommand,
    explicit: Option<Arc<ElevationToken>>,
) -> Result<()> {
    let out = super::jobs::run_step(&h, &command, explicit.as_deref(), WIPE_TIMEOUT).await?;
    drop(explicit);
    let result: tentanas_helper::elastic::DiskWipeResult = serde_json::from_str(&out.stdout)?;
    for step in &result.steps {
        h.log(step);
    }
    for signature in &result.removed {
        h.log(format!(
            "usunięto sygnaturę {} na {}{}{}",
            signature.kind,
            signature.offset,
            signature
                .label
                .as_deref()
                .map(|l| format!(" (label {l})"))
                .unwrap_or_default(),
            signature
                .uuid
                .as_deref()
                .map(|u| format!(" (uuid {u})"))
                .unwrap_or_default(),
        ));
    }
    if let Some(array) = &result.journal_released {
        h.log(format!(
            "zwolniono rezerwację dziennika macierzy Elastic {array}: macierzy nie da się już \
             przywrócić importem"
        ));
    }
    h.log(format!("urządzenie {} nie zgłasza już żadnych sygnatur", result.device));
    refresh_inventory(h.db()).await?;
    h.progress(100);
    Ok(())
}

/// The journal claiming one disk, out of every journal this node holds.
///
/// Identity, never the device name: a journal records a member's WWN and
/// serial, and matching on either is what makes the claim survive the kernel
/// renaming the disk between two boots.
///
/// `current` is the instance asking. Whether the journal is somebody else's
/// is decided HERE, against it — the dialog used to infer "foreign" from the
/// owner ids merely being filled in, which they always are, and so called
/// this instance's own dissolved arrays another instance's.
///
/// A journal of another organisation that lives on this node
/// (`OWNER_KIND_OTHER_ORG_ON_NODE`, decided against `orgs_on_node`) is still
/// returned — the disk IS claimed, and pretending otherwise would offer to
/// erase another tenant's array — but as a claim that carries nothing but
/// that code: no array id, no name, no role, no member count, no ids.
/// `plan_wipe` turns it into a refusal that names no array.
pub fn journal_claim_of(
    disk: &NasDisk,
    journals: &[tentanas_helper::elastic::ElasticJournalEntry],
    current: &tentanas_helper::elastic::ElasticOwner,
    orgs_on_node: &std::collections::BTreeSet<String>,
) -> Option<JournalClaim> {
    for entry in journals {
        let members = || {
            entry
                .spec
                .data
                .iter()
                .map(|d| (d, "data"))
                .chain(entry.spec.cache.iter().map(|d| (d, "cache")))
                .chain(entry.spec.parity.iter().map(|d| (d, "parity")))
        };
        let Some((_, role)) = members().find(|(member, _)| {
            member.disk_id == disk.disk_id
                || member
                    .wwn
                    .as_ref()
                    .is_some_and(|w| !w.is_empty() && disk.wwn.as_ref() == Some(w))
                || member
                    .serial
                    .as_ref()
                    .is_some_and(|s| !s.is_empty() && *s == disk.serial)
        }) else {
            continue;
        };
        let foreign = entry.spec.owner != *current;
        let kind = super::elastic::owner_kind(&entry.spec.owner, current, orgs_on_node);
        if kind == super::elastic::OWNER_KIND_OTHER_ORG_ON_NODE {
            // Blanked HERE, at the one place the journal is read for a wipe,
            // so no later step can leak what this tenant may not know.
            return Some(JournalClaim {
                claim: NasDiskWipeJournalClaim {
                    owner_foreign: true,
                    owner_kind: kind.to_string(),
                    ..Default::default()
                },
                serving: entry.union_mounted,
            });
        }
        // Another installation's ids never leave the node: the dialog words
        // the owner from `owner_kind`, and the wipe path re-reads the journal
        // itself rather than trusting anything the client carries back.
        let (owner_org_id, owner_addon_id) = super::elastic::owner_ids_for(kind, &entry.spec.owner);
        return Some(JournalClaim {
            claim: NasDiskWipeJournalClaim {
                array_id: entry.spec.array_id.clone(),
                name: entry.spec.name.clone(),
                array_role: role.to_string(),
                member_count: members().count() as u32,
                owner_org_id,
                owner_addon_id,
                owner_foreign: foreign,
                owner_kind: kind.to_string(),
                // The instance's display name is the platform database's, and
                // the dispatch handler adds it only for an owner of the asking
                // organisation.
                owner_instance_name: String::new(),
            },
            serving: entry.union_mounted,
        });
    }
    None
}

/// Kernel name → (vdev role, vdev kind) for every leaf of every imported
/// pool. The Disks tab names the group a disk serves ("tank · RAIDZ2"), which
/// only `zpool status` knows; reading it here — once per inventory refresh —
/// keeps it off the tab's five-second poll, where it used to cost a full pool
/// listing (datasets and snapshots included) per tick.
///
/// The second map is the same leaves' grading state
/// (`pools::leaf_grading_state`: a state word the parser does not know is
/// `unknown`, never `unavail`), from the same `zpool status` read:
/// FAULTED/UNAVAIL is what `grade_disk_health` calls "Awaria", and a second
/// pass over the pools for it would only be able to disagree with this one.
/// A leaf whose device node is gone is in neither map: its kernel name may
/// already belong to a new disk (`zfs::resolved_kernel_name`).
/// It is `None` when any pool could not be read: a
/// leaf missing from a PARTIAL read is unknown, not healthy, and reading it
/// as healthy would close a faulted disk's alert on one failed `zpool
/// status` and re-raise it — with a new timestamp — on the next.
///
/// The third answer is the names of the pools that ARE imported — what tells
/// "this disk left every pool" from "its pool is not imported yet"
/// (`awaiting_pool_import`).
async fn vdev_membership() -> (HashMap<String, (String, String)>, Option<HashMap<String, String>>, HashSet<String>) {
    if !super::zfs::available() {
        // No ZFS, no pool, no leaf: that is a complete answer.
        return (HashMap::new(), Some(HashMap::new()), HashSet::new());
    }
    let rows = match super::pools::list_rows().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("tentanas: pool list for disk roles failed: {e}");
            return (HashMap::new(), None, HashSet::new());
        }
    };
    let imported: HashSet<String> = rows.iter().map(|row| row.name.clone()).collect();
    let mut index = HashMap::new();
    let mut states = Some(HashMap::new());
    for row in rows {
        let status = match super::pools::status(&row.name).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("tentanas: pool status for disk roles failed ({}): {e}", row.name);
                states = None;
                continue;
            }
        };
        for leaf in status.leaves {
            let Some(name) = leaf.kernel_name else { continue };
            if let Some(states) = states.as_mut() {
                states.insert(name.clone(), leaf.state.to_string());
            }
            index.insert(name, (leaf.role, leaf.kind));
        }
    }
    (index, states, imported)
}

/// Disk id → (array name, part played in it) for every Elastic Array this
/// node has recorded.
///
/// Membership of an Elastic Array is NOT on the disk. mdraid writes a
/// `linux_raid_member` superblock and ZFS a `zfs_member` label, so lsblk alone
/// can name those owners; an Elastic Array is a union mount over ordinary xfs
/// or ext4 filesystems, one per branch, and nothing on the disk says which
/// array it serves. Only this node's database knows — which is why the lookup
/// happens HERE, once per inventory refresh, and never on the tab's
/// five-second poll, for the same reason `vdev_membership` is read here.
///
/// Keyed by `disk_id`, never by kernel name: `BranchRow::name` is the branch's
/// slot name (`d1`, `c1`), not a device, and a kernel name can be handed to a
/// different disk across a reboot.
fn array_membership(db: &DbPool) -> HashMap<String, (String, String)> {
    let arrays = match store::elastic_arrays_all(db) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("tentanas: elastic array list for disk roles failed: {e}");
            return HashMap::new();
        }
    };
    let mut index = HashMap::new();
    for array in arrays {
        for branch in &array.branches {
            index.insert(
                branch.disk_id.clone(),
                (array.name.clone(), branch.role.clone()),
            );
        }
        // Parity is not a branch of the union (it holds a file, not a tree),
        // so it carries no role of its own in the row and gets one here.
        for parity in &array.parity {
            index.insert(
                parity.disk_id.clone(),
                (array.name.clone(), "parity".to_string()),
            );
        }
    }
    index
}

/// Applies the two memberships lsblk cannot see to a freshly scanned
/// inventory: the ZFS vdev a pool member sits in, and the part an Elastic
/// Array member plays in its array.
///
/// Split out of `refresh_inventory` so both can be exercised without lsblk,
/// `zpool status` or a live node.
fn apply_membership(
    disks: &mut [NasDisk],
    vdevs: &HashMap<String, (String, String)>,
    arrays: &HashMap<String, (String, String)>,
) {
    for d in disks.iter_mut() {
        apply_array_membership(d, arrays);
        if let Some((role, kind)) = vdevs.get(&d.name) {
            d.vdev_role.clone_from(role);
            d.vdev_kind.clone_from(kind);
        }
    }
}

/// Names the Elastic Array that owns one disk, when this node recorded one.
///
/// Without this the branches of an array fall through `role_of` to the
/// catch-all `used` (or `mounted`), which is why a node with one array of 23
/// disks showed "Zajęty · xfs" 23 times: true, and useless — the filesystem is
/// an implementation detail of the union, and the thing the admin acts on is
/// the array.
fn apply_array_membership(d: &mut NasDisk, arrays: &HashMap<String, (String, String)>) {
    // The system disk stays the system disk, and a signature ON the disk
    // (`zfs_member`, `linux_raid_member`) is harder evidence than a database
    // row that a dissolved array may have left behind.
    if d.role == "system" || d.member_of.is_some() {
        return;
    }
    let Some((array, role)) = arrays.get(&d.disk_id) else {
        return;
    };
    d.role = "array_member".to_string();
    d.member_of = Some(array.clone());
    d.array_role = role.clone();
    // The branch filesystem is what the array built, not a foreign occupier:
    // `fs_type` exists only so the catch-all `used` can name what it cannot
    // otherwise explain, and leaving it set would put "· xfs" back on a row
    // that now has a real owner to show.
    d.fs_type = None;
    d.fs_label = None;
}

pub fn disks_from_lsblk_json(text: &str) -> Result<Vec<NasDisk>> {
    let doc: Value = serde_json::from_str(text)?;
    let devices = doc
        .get("blockdevices")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("lsblk: no blockdevices array"))?;
    Ok(devices.iter().filter_map(disk_from_lsblk).collect())
}

async fn inventory() -> Result<Vec<NasDisk>> {
    if !cfg!(target_os = "linux") {
        return Ok(Vec::new());
    }
    let out = broker::run_unprivileged(
        "lsblk",
        &["-J", "-b", "-o", LSBLK_COLUMNS],
        Duration::from_secs(15),
    )
    .await?;
    if !out.success() {
        return Err(anyhow!("lsblk exited with {}: {}", out.code, out.stderr.trim()));
    }
    disks_from_lsblk_json(&out.stdout)
}

// ----- /proc/diskstats ------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct DiskStats {
    reads: u64,
    sectors_read: u64,
    ms_reading: u64,
    writes: u64,
    sectors_written: u64,
    ms_writing: u64,
    ms_io: u64,
}

pub fn parse_diskstats(text: &str) -> HashMap<String, DiskStats> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        map.insert(
            f[2].to_string(),
            DiskStats {
                reads: n(3),
                sectors_read: n(5),
                ms_reading: n(6),
                writes: n(7),
                sectors_written: n(9),
                ms_writing: n(10),
                ms_io: n(12),
            },
        );
    }
    map
}

/// Rates between two readings `elapsed` apart. Sector size of diskstats is
/// always 512 bytes regardless of the device's block size.
pub fn io_rates(prev: &DiskStats, cur: &DiskStats, elapsed: Duration) -> NasDiskIo {
    let secs = elapsed.as_secs_f64().max(0.001);
    let d = |a: u64, b: u64| b.saturating_sub(a);
    let reads = d(prev.reads, cur.reads);
    let writes = d(prev.writes, cur.writes);
    let ops = reads + writes;
    let await_ms = if ops == 0 {
        0.0
    } else {
        (d(prev.ms_reading, cur.ms_reading) + d(prev.ms_writing, cur.ms_writing)) as f64 / ops as f64
    };
    NasDiskIo {
        read_bps: (d(prev.sectors_read, cur.sectors_read) as f64 * 512.0 / secs) as u64,
        write_bps: (d(prev.sectors_written, cur.sectors_written) as f64 * 512.0 / secs) as u64,
        read_iops: reads as f64 / secs,
        write_iops: writes as f64 / secs,
        await_ms: (await_ms * 100.0).round() / 100.0,
        util_pct: ((d(prev.ms_io, cur.ms_io) as f64 / (secs * 1000.0)) * 100.0).min(100.0),
    }
}

// ----- smartctl JSON ---------------------------------------------------------------

/// The fields of a `smartctl --json=c -x` document the app uses.
#[derive(Debug, Default, Clone)]
pub struct SmartSummary {
    pub passed: Option<bool>,
    pub temperature_c: Option<i32>,
    pub power_on_hours: Option<u64>,
    pub reallocated: Option<u64>,
    pub pending: Option<u64>,
    pub crc_errors: Option<u64>,
    pub media_errors: Option<u64>,
    pub wear_pct: Option<u8>,
    pub firmware: Option<String>,
    pub self_test_running_pct: Option<u8>,
    pub self_test_failed: bool,
}

fn attr_raw(doc: &Value, id: u64) -> Option<u64> {
    doc.get("ata_smart_attributes")?
        .get("table")?
        .as_array()?
        .iter()
        .find(|a| a.get("id").and_then(Value::as_u64) == Some(id))?
        .get("raw")?
        .get("value")?
        .as_u64()
}

fn attr_value(doc: &Value, id: u64) -> Option<u64> {
    doc.get("ata_smart_attributes")?
        .get("table")?
        .as_array()?
        .iter()
        .find(|a| a.get("id").and_then(Value::as_u64) == Some(id))?
        .get("value")?
        .as_u64()
}

/// Unrecovered errors of a SCSI disk, summed over the command classes it
/// reported. They are disjoint event counts — a read that failed is not a write
/// that failed — so the total is the disk's media-error count; `max` would hide
/// every class but the worst, and a bare "any of them is non-zero" would throw
/// away the number the disk list and the minute samples show.
///
/// smartctl emits only the classes the disk answered for: `verify` is absent on
/// all ten SAS disks this was measured against, so no key may be assumed. A
/// document that carries none of them returns None rather than 0 — "the disk
/// did not report it" is not "the disk reported none".
fn scsi_uncorrected_errors(doc: &Value) -> Option<u64> {
    let log = doc.get("scsi_error_counter_log")?;
    let mut total: Option<u64> = None;
    for class in ["read", "write", "verify"] {
        if let Some(n) = log
            .get(class)
            .and_then(|c| c.get("total_uncorrected_errors"))
            .and_then(Value::as_u64)
        {
            total = Some(total.unwrap_or(0).saturating_add(n));
        }
    }
    total
}

pub fn summarize_smart(doc: &Value) -> SmartSummary {
    let nvme = doc.get("nvme_smart_health_information_log");
    let mut s = SmartSummary {
        passed: doc.pointer("/smart_status/passed").and_then(Value::as_bool),
        temperature_c: doc
            .pointer("/temperature/current")
            .and_then(Value::as_i64)
            .map(|t| t as i32),
        power_on_hours: doc.pointer("/power_on_time/hours").and_then(Value::as_u64),
        // A SAS/SCSI disk reports no `firmware_version` at all; the revision it
        // does report is `scsi_revision` (measured on /dev/sdb: the first key
        // absent, the second "0101").
        firmware: doc
            .get("firmware_version")
            .or_else(|| doc.get("scsi_revision"))
            .and_then(Value::as_str)
            .map(str::to_string),
        ..Default::default()
    };
    if let Some(n) = nvme {
        s.media_errors = n.get("media_errors").and_then(Value::as_u64);
        s.wear_pct = n
            .get("percentage_used")
            .and_then(Value::as_u64)
            .map(|p| p.min(100) as u8);
        if s.power_on_hours.is_none() {
            s.power_on_hours = n.get("power_on_hours").and_then(Value::as_u64);
        }
        if n.get("critical_warning").and_then(Value::as_u64).unwrap_or(0) != 0 {
            s.passed = Some(false);
        }
    } else {
        s.reallocated = attr_raw(doc, 5);
        s.pending = attr_raw(doc, 197);
        s.crc_errors = attr_raw(doc, 199);
        // 187 Reported_Uncorrect / 198 Offline_Uncorrectable: media errors of
        // an ATA disk; whichever the vendor exposes.
        s.media_errors = attr_raw(doc, 187).or_else(|| attr_raw(doc, 198));
        // SSD wear: the normalized value counts DOWN from 100 (177
        // Wear_Leveling_Count, 231 SSD_Life_Left, 233 Media_Wearout_Indicator).
        s.wear_pct = [177, 231, 233]
            .into_iter()
            .find_map(|id| attr_value(doc, id))
            .map(|v| (100 - v.min(100)) as u8);
        // Attribute 190/194 when the top-level temperature block is absent.
        if s.temperature_c.is_none() {
            s.temperature_c = attr_raw(doc, 194)
                .or_else(|| attr_raw(doc, 190))
                .map(|t| (t & 0xff) as i32);
        }
        // SAS/SCSI disks carry no attribute table at all, so every `attr_raw`
        // above is None for them. The grown defect list is the SCSI analogue of
        // reallocated sectors: blocks the drive reassigned after it left the
        // factory. The error counter log is the analogue of the ATA
        // uncorrectable counts. `pending` and `crc_errors` stay unmapped — SCSI
        // exposes no honest equivalent of a pending-reallocation count or of the
        // SATA interface CRC counter.
        if s.reallocated.is_none() {
            s.reallocated = doc.get("scsi_grown_defect_list").and_then(Value::as_u64);
        }
        if s.media_errors.is_none() {
            s.media_errors = scsi_uncorrected_errors(doc);
        }
    }
    let st = doc.pointer("/ata_smart_data/self_test/status");
    if let Some(st) = st {
        if let Some(rem) = st.get("remaining_percent").and_then(Value::as_u64) {
            s.self_test_running_pct = Some((100 - rem.min(100)) as u8);
        }
        if st.get("passed").and_then(Value::as_bool) == Some(false)
            && st.get("remaining_percent").is_none()
        {
            s.self_test_failed = true;
        }
    }
    // NVMe has no status block at all: it reports a running test in the header
    // of the self-test log itself. `current_self_test_operation.value` is the
    // operation code and 0 means none is running, and the percentage is emitted
    // ONLY while one is — in `nvmeprint.cpp` the assignment
    // `jref["current_self_test_completion_percent"]` sits inside
    // `if (self_test_log.current_operation & 0xf)`, under
    // `jref = jglb["nvme_self_test_log"]`. Verified against smartctl 7.5
    // (r5714), whose string table carries both keys.
    //
    // Unlike ATA's `remaining_percent` this is the percentage ALREADY DONE, so
    // it is taken as it stands rather than subtracted from 100.
    if s.self_test_running_pct.is_none()
        && doc
            .pointer("/nvme_self_test_log/current_self_test_operation/value")
            .and_then(Value::as_u64)
            .is_some_and(|op| op != 0)
    {
        let done = doc
            .pointer("/nvme_self_test_log/current_self_test_completion_percent")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        s.self_test_running_pct = Some(done.min(100) as u8);
    }
    // A SAS/SCSI disk has no ATA status pointer, so the block above never fires
    // for it: without this, the detail view could show a red "failed" self-test
    // row while `score_health` called the disk "ok" and no alert was raised.
    // NVMe has neither an ATA status pointer nor `scsi_self_test_0`, so until
    // now its self-test verdict reached NOTHING: a genuinely failed NVMe test
    // (result 5, 6 or 7) left `self_test_failed` false and `score_health`
    // never saw it. Reading the normalised entries covers both transports with
    // one rule — and it is only safe because the mapping above now separates
    // an abort from a failure. Without that, a test the controller merely
    // interrupted would mark the disk critical.
    //
    // The newest entry that carries a VERDICT is the last one the disk gave,
    // and only "passed" and "failed" are verdicts. Skipping just "running" was
    // not enough: a reserved NVMe code (Ah–Eh), an SPC code 3
    // ("unknown error, incomplete"), an SPC reserved code (8–14) and an entry
    // with no parseable result all normalise to "unknown", and any of them
    // sitting on top was taken as THE verdict — showing a red "failed" row in
    // the detail view while `score_health` still called the disk "ok". Listing
    // the verdicts rather than the non-verdicts also keeps a status added
    // later from silently counting as one.
    //
    // An ABORT is not one of them, and used to be. An abort says the run was
    // stopped — the user's own command, a controller reset, a Format NVM, a
    // sanitize — and says NOTHING about the medium, so reading it as "the last
    // verdict" let a controller reset or a sanitize silently CLEAR a recorded
    // failure underneath it: the disk had failed a test, one reset later the
    // detail view was green and no alert was ever raised again. It is now
    // skipped exactly like "running" and "unknown", and the newest real verdict
    // stands. The counter-argument — that a very old failure followed by
    // aborts may be stale — does not hold: an abort is no evidence that the
    // medium improved, and the way to clear a stale failure is a test that
    // PASSES, which still clears it on the next poll. The two mistakes are not
    // symmetric either: a cleared real failure loses the alert on a dying
    // disk, while a retained stale failure costs one more self-test.
    if st.is_none()
        && (doc.get("scsi_self_test_0").is_some() || doc.get("nvme_self_test_log").is_some())
    {
        s.self_test_failed = smart_self_tests(doc)
            .iter()
            .find(|t| matches!(t.status.as_str(), "passed" | "failed"))
            .is_some_and(|t| t.status == "failed");
    }
    s
}

pub fn smart_attributes(doc: &Value) -> Vec<NasSmartAttribute> {
    let Some(table) = doc.pointer("/ata_smart_attributes/table").and_then(Value::as_array) else {
        return nvme_pseudo_attributes(doc);
    };
    table
        .iter()
        .filter_map(|a| {
            let value = a.get("value").and_then(Value::as_i64)?;
            let threshold = a.get("thresh").and_then(Value::as_i64).unwrap_or(0);
            let failing_now = a
                .pointer("/when_failed")
                .and_then(Value::as_str)
                .is_some_and(|w| !w.is_empty());
            let status = if failing_now || (threshold > 0 && value <= threshold) {
                "failing"
            } else if threshold > 0 && value <= threshold + 10 {
                "warning"
            } else {
                "ok"
            };
            Some(NasSmartAttribute {
                id: a.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                name: a.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                value,
                worst: a.get("worst").and_then(Value::as_i64).unwrap_or(value),
                threshold,
                raw: a.pointer("/raw/value").and_then(Value::as_i64).unwrap_or(0),
                raw_text: a
                    .pointer("/raw/string")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: status.to_string(),
                raw_week_ago: None,
            })
        })
        .collect()
}

/// NVMe has no attribute table; the health log fields are shown in the same
/// table so the detail view has one shape for both.
fn nvme_pseudo_attributes(doc: &Value) -> Vec<NasSmartAttribute> {
    let Some(log) = doc.get("nvme_smart_health_information_log") else {
        return Vec::new();
    };
    const FIELDS: &[(&str, &str)] = &[
        ("critical_warning", "Critical Warning"),
        ("temperature", "Temperature"),
        ("available_spare", "Available Spare"),
        ("available_spare_threshold", "Available Spare Threshold"),
        ("percentage_used", "Percentage Used"),
        ("data_units_read", "Data Units Read"),
        ("data_units_written", "Data Units Written"),
        ("power_cycles", "Power Cycles"),
        ("power_on_hours", "Power On Hours"),
        ("unsafe_shutdowns", "Unsafe Shutdowns"),
        ("media_errors", "Media and Data Integrity Errors"),
        ("num_err_log_entries", "Error Information Log Entries"),
    ];
    let spare_threshold = log
        .get("available_spare_threshold")
        .and_then(Value::as_i64)
        .unwrap_or(10);
    FIELDS
        .iter()
        .enumerate()
        .filter_map(|(i, (key, name))| {
            let raw = log.get(*key).and_then(Value::as_i64)?;
            let status = match *key {
                "critical_warning" | "media_errors" if raw > 0 => "failing",
                "available_spare" if raw <= spare_threshold => "failing",
                "percentage_used" if raw >= 90 => "warning",
                "unsafe_shutdowns" | "num_err_log_entries" if raw > 0 => "warning",
                _ => "ok",
            };
            Some(NasSmartAttribute {
                id: i as u32,
                name: name.to_string(),
                value: raw,
                worst: raw,
                threshold: 0,
                raw,
                raw_text: raw.to_string(),
                status: status.to_string(),
                raw_week_ago: None,
            })
        })
        .collect()
}

pub fn smart_self_tests(doc: &Value) -> Vec<NasSmartSelfTest> {
    let mut out = Vec::new();
    if let Some(rows) = doc
        .pointer("/ata_smart_self_test_log/standard/table")
        .and_then(Value::as_array)
    {
        for r in rows {
            let status_str = r.pointer("/status/string").and_then(Value::as_str).unwrap_or("");
            let passed = r.pointer("/status/passed").and_then(Value::as_bool);
            out.push(NasSmartSelfTest {
                kind: r.pointer("/type/string").and_then(Value::as_str).unwrap_or("").to_string(),
                status: match passed {
                    Some(true) => "passed",
                    Some(false) => "failed",
                    None if status_str.to_ascii_lowercase().contains("progress") => "running",
                    None => "unknown",
                }
                .to_string(),
                lifetime_hours: r.get("lifetime_hours").and_then(Value::as_u64).unwrap_or(0),
                started_at: None,
                detail: status_str.to_string(),
            });
        }
    }
    if let Some(rows) = doc.pointer("/nvme_self_test_log/table").and_then(Value::as_array) {
        for r in rows {
            // smartctl writes `jref["table"][i]` at the RAW log index
            // (`nvmeprint.cpp`), so the array it emits is SPARSE and its JSON
            // writer prints every slot it never set as a literal `null`. A
            // null is not a test: it used to fall through the match below into
            // "unknown" and become a phantom row with an empty kind and zero
            // hours — which, sitting on top of the list, was then read as the
            // disk's last verdict.
            let Some(r) = r.as_object() else {
                continue;
            };
            let result = r
                .get("self_test_result")
                .and_then(|v| v.get("value"))
                .and_then(Value::as_u64);
            // 15 means the log slot holds NO result (NVMe Device Self-test Log,
            // Self-test Result field). The real producer drops those entries
            // before it writes JSON (`if (!op || res == 0xf) continue;`), so
            // this arm is the spec, not the observed shape — kept because it
            // costs nothing and reporting the absence of a test as a failure
            // is exactly the defect above.
            if result == Some(15) {
                continue;
            }
            out.push(NasSmartSelfTest {
                kind: r
                    .get("self_test_code")
                    .and_then(|v| v.get("string"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                // An abort is not a failure, exactly as for ATA and SPC. Per
                // the same field: 1 aborted by a self-test command, 2 by a
                // controller reset, 3 by a namespace removal, 4 by a Format
                // NVM, 8 for an unknown reason, 9 by a sanitize — none of
                // those says anything bad about the medium. Only 5 (fatal or
                // unknown error), 6 (a failed segment, unidentified) and 7 (a
                // failed segment, identified) are real failures.
                status: match result {
                    Some(0) => "passed",
                    Some(1..=4 | 8 | 9) => "aborted",
                    Some(5..=7) => "failed",
                    _ => "unknown",
                }
                .to_string(),
                lifetime_hours: r.get("power_on_hours").and_then(Value::as_u64).unwrap_or(0),
                started_at: None,
                detail: r
                    .get("self_test_result")
                    .and_then(|v| v.get("string"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    // SCSI has no self-test table: smartctl prints the log as numbered
    // top-level keys, newest first, and stops emitting them where the log ends.
    for i in 0..20 {
        let Some(r) = doc.get(format!("scsi_self_test_{i}")) else {
            break;
        };
        out.push(NasSmartSelfTest {
            kind: r.pointer("/code/string").and_then(Value::as_str).unwrap_or("").to_string(),
            // The SPC self-test result code (smartctl's own table in
            // `scsiprint.cpp`): 0 completed, 1 aborted by the user's command,
            // 2 aborted by a device reset, 3 "unknown error, incomplete",
            // 4–7 segment failures, 8–14 reserved, 15 still running.
            //
            // Only 4–7 are a disk fault. Code 3 means the test did not finish
            // and 8–14 mean nothing at all, so both stay "unknown": the
            // self-test job fails a run on "failed", and the health score now
            // marks a disk critical on it, so lumping them in would report a
            // healthy disk as broken.
            status: match r.pointer("/result/value").and_then(Value::as_u64) {
                Some(0) => "passed",
                Some(1 | 2) => "aborted",
                Some(4..=7) => "failed",
                Some(15) => "running",
                // 3, 8–14, and a document with no result code at all.
                _ => "unknown",
            }
            .to_string(),
            lifetime_hours: r.pointer("/power_on_time/hours").and_then(Value::as_u64).unwrap_or(0),
            started_at: None,
            detail: r.pointer("/result/string").and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    out
}

// ----- health ----------------------------------------------------------------------

/// SMART's verdict on one disk (§5.4), graded against the SPEC legend of the
/// Disks tab (n03 "Jak czytać zdrowie"): `ok` = no symptom in any source,
/// `warning` ("Uwaga") = a symptom that needs watching or a decision — new
/// reallocations and temperature are the legend's own examples — and
/// `critical` ("Awaria") = "dysk FAULTED/UNAVAIL — wymagana wymiana", a disk
/// that is out of service or must be replaced.
///
/// A COUNTER IS A SYMPTOM, NOT A FAILURE. Before this, a disk with three new
/// reallocations in a week — the SPEC's `sdd`, "+3 realokacje w 7 dni", drawn
/// as "Uwaga" on n03, n04 and in the alert list — was shown as "Awaria", the
/// same red as a disk that is gone. The urgency of a MOVING count is not lost
/// with the colour: it is `replacement_advice`'s `urgent` severity, which
/// does not wait for the two-day threshold, and the reason still says
/// "growing".
///
/// Rules against the legend (owner decision 2026-09-21):
/// - reallocated count growing against the week-old sample: was `critical`,
///   now `warning` — the legend's example of "Uwaga";
/// - temperature over the critical limit: was `critical`, now `warning` —
///   temperature is the legend's other example; the reason names the limit;
/// - pending sectors: was `critical`, now `warning` — the SPEC's `sdd` passed
///   its long test "z uwagą: 2 pending sectors" and is "Uwaga";
/// - media errors (NVMe / SCSI uncorrected): was `critical`, now `warning` —
///   the NVMe and SCSI counterpart of pending sectors;
/// - wear >= 95 %: was `critical`, now `warning` — a worn-out SSD still
///   serves; replacing it is a decision to plan, not an outage;
/// - overall SMART FAILED and a failed self-test stay `critical`: they are the
///   drive's OWN verdict that it is failing, which is the "wymagana wymiana"
///   of the legend's "Awaria" — and the pool wizard refuses a `critical` disk
///   (`pool-wizard.js`), which is the only thing keeping such a free disk out
///   of a new pool;
/// - static reallocated count, UDMA CRC errors, wear >= 85 %, temperature over
///   the warning limit: `warning` before and after — they already matched.
///
/// FAULTED/UNAVAIL itself is not SMART's to know; `grade_disk_health` adds it
/// from the pool's leaf state.
pub fn score_health(
    s: &SmartSummary,
    kind: &str,
    reallocated_week_ago: Option<i64>,
) -> (&'static str, Vec<NasHealthReason>) {
    let mut critical = Vec::new();
    let mut warning = Vec::new();
    if s.passed == Some(false) {
        critical.push(coded_reason("smart_failed", &[]));
    }
    if s.self_test_failed {
        critical.push(coded_reason("self_test_failed", &[]));
    }
    if let Some(p) = s.pending.filter(|v| *v > 0) {
        warning.push(coded_reason("pending_sectors", &[("count", p.to_string())]));
    }
    if let Some(m) = s.media_errors.filter(|v| *v > 0) {
        warning.push(coded_reason("media_errors", &[("count", m.to_string())]));
    }
    if let Some(r) = s.reallocated.filter(|v| *v > 0) {
        match reallocated_week_ago {
            Some(old) if (r as i64) > old => warning.push(coded_reason(
                "reallocated_growing",
                &[("from", old.to_string()), ("to", r.to_string())],
            )),
            _ => warning.push(coded_reason("reallocated", &[("count", r.to_string())])),
        }
    }
    let temp_warn = if kind == "hdd" { 50 } else { 65 };
    let temp_limit = if kind == "hdd" { 60 } else { 75 };
    if let Some(t) = s.temperature_c {
        if t >= temp_limit {
            warning.push(coded_reason(
                "temperature_over_limit",
                &[("celsius", t.to_string()), ("limit", temp_limit.to_string())],
            ));
        } else if t >= temp_warn {
            // The threshold travels with the reading: the alert reads
            // "temperature 54°C is over the 50°C threshold" (mockups n01/n02),
            // and which threshold applies depends on the drive's kind.
            warning.push(coded_reason(
                "temperature_high",
                &[("celsius", t.to_string()), ("limit", temp_warn.to_string())],
            ));
        }
    }
    if let Some(c) = s.crc_errors.filter(|v| *v > 0) {
        warning.push(coded_reason("crc_errors", &[("count", c.to_string())]));
    }
    if let Some(w) = s.wear_pct {
        if w >= 85 {
            warning.push(coded_reason("wear", &[("pct", w.to_string())]));
        }
    }
    if !critical.is_empty() {
        // The symptoms travel with the verdict: a failing drive that is also
        // hot or reallocating says so in the same list.
        critical.extend(warning);
        ("critical", critical)
    } else if !warning.is_empty() {
        ("warning", warning)
    } else if s.passed.is_some() {
        ("ok", Vec::new())
    } else {
        ("unknown", vec![coded_reason("no_smart_data", &[])])
    }
}

/// One reason as the wire carries it (`NasHealthReason`): a code, and its
/// parameters in their decimal or wire spelling. Shared with the pool grade
/// (`pools::score_health`).
pub fn coded_reason(code: &str, params: &[(&str, String)]) -> NasHealthReason {
    NasHealthReason {
        code: code.to_string(),
        params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
    }
}

/// One parameter of a reason, for the English sentence. A missing one reads
/// "?" rather than panicking: the sentence is a tooltip and an alert text,
/// and a gap in it must not take the inventory pass down.
pub fn reason_param<'a>(reason: &'a NasHealthReason, key: &str) -> &'a str {
    reason.params.get(key).map_or("?", String::as_str)
}

/// The node's English sentence for one disk reason — what
/// `NasDisk::health_reason` and the disk's health alert carry.
///
/// Word for word what `score_health` and `grade_disk_health` wrote before
/// the codes existed. WHY it must not drift: the alert rows in `nas_alerts`
/// still store this text (their own codes need a schema change), and an
/// alert that is refreshed in place — same severity — would otherwise have
/// its detail reworded by nothing but an upgrade. The screens do not read it
/// any more; they word the codes themselves.
pub fn disk_reason_sentence(reason: &NasHealthReason) -> String {
    let p = |key| reason_param(reason, key);
    match reason.code.as_str() {
        "smart_failed" => "SMART overall status FAILED".to_string(),
        "self_test_failed" => "last self-test failed".to_string(),
        "pending_sectors" => format!("{} pending sectors", p("count")),
        "media_errors" => format!("{} media errors", p("count")),
        "reallocated_growing" => format!(
            "reallocated sectors growing ({} → {} in 7 days)",
            p("from"),
            p("to")
        ),
        "reallocated" => format!("{} reallocated sectors", p("count")),
        "temperature_over_limit" => format!("{}°C (over the {}°C limit)", p("celsius"), p("limit")),
        "temperature_high" => format!("{}°C", p("celsius")),
        "crc_errors" => format!("{} UDMA CRC errors (cable/backplane)", p("count")),
        "wear" => format!("{}% worn", p("pct")),
        "no_smart_data" => "no SMART data".to_string(),
        "zfs_faulted" => "ZFS reports this disk FAULTED".to_string(),
        "zfs_unavail" => "ZFS reports this disk UNAVAIL".to_string(),
        "reallocated_grew" => format!(
            "reallocated sectors grew from {} to {} in the last 7 days",
            p("from"),
            p("to")
        ),
        "unhealthy_for_days" => format!("{} for {} days", p("health"), p("days")),
        other => other.to_string(),
    }
}

/// The whole English sentence of a list of disk reasons, "; "-joined as
/// `health_reason` always was.
pub fn disk_reasons_text(reasons: &[NasHealthReason]) -> String {
    reasons.iter().map(disk_reason_sentence).collect::<Vec<_>>().join("; ")
}

/// The health a disk shows, in both forms: the codes the screens word and
/// the English sentence the alert and the tooltip keep.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskGrade {
    pub health: String,
    pub reason: String,
    pub reasons: Vec<NasHealthReason>,
}

/// The health the Disks tab shows: SMART's verdict, raised to `critical` when
/// the disk's pool has taken it out of service.
///
/// `leaf_state` is the grading state of the disk's leaf in `zpool status`
/// (`pools::leaf_grading_state`), `None` for a disk that is no ZFS leaf.
/// FAULTED and UNAVAIL are the legend's "Awaria": the pool no longer reads
/// the disk, so whatever SMART says about the medium, the redundancy it
/// provided is already spent. Every other leaf state (online, degraded,
/// offline, removed, and `unknown` — a state word the parser does not
/// recognise) leaves SMART's verdict as it is: OFFLINE and REMOVED are an
/// admin's act or a pulled disk, and an unreadable word is no evidence of a
/// failure at all.
///
/// The SMART reason is kept behind the pool's, so a faulted disk that SMART
/// also complains about says both. No pool name goes into the reason: the
/// health text is shown to every tenant of the node (`hide_other_org_array`
/// keeps it), and the membership is already a column of its own.
///
/// SMART's sentence and its codes come in apart (`smart_reason`,
/// `smart_reasons`) because they can disagree for one reason only: a row
/// stored before migration 21 kept the sentence without its codes, and a
/// restart seeds the codes empty rather than claim something the node did
/// not measure (`live_from_persisted`).
pub fn grade_disk_health(
    smart_health: &str,
    smart_reason: &str,
    smart_reasons: &[NasHealthReason],
    leaf_state: Option<&str>,
) -> DiskGrade {
    let pool_verdict = match leaf_state {
        Some("faulted") => Some(coded_reason("zfs_faulted", &[])),
        Some("unavail") => Some(coded_reason("zfs_unavail", &[])),
        _ => None,
    };
    match pool_verdict {
        Some(verdict) => {
            let sentence = disk_reason_sentence(&verdict);
            let reason = if smart_reason.is_empty() {
                sentence
            } else {
                format!("{sentence}; {smart_reason}")
            };
            let mut reasons = vec![verdict];
            reasons.extend_from_slice(smart_reasons);
            DiskGrade { health: "critical".to_string(), reason, reasons }
        }
        None => DiskGrade {
            health: smart_health.to_string(),
            reason: smart_reason.to_string(),
            reasons: smart_reasons.to_vec(),
        },
    }
}

/// Puts a grade on the disk the screens read, and returns the health and the
/// sentence its alert is written with.
fn show_grade(disk: &mut NasDisk, grade: DiskGrade) -> (String, String) {
    disk.health = grade.health.clone();
    disk.health_reason = grade.reason.clone();
    disk.health_reasons = grade.reasons;
    (grade.health, grade.reason)
}

// ----- proactive replacement (§5.10, research R5) -----------------------------------

/// How long a disk has to stay unhealthy before the app says "replace it".
/// Two days, not two hours: a single hot afternoon or one cable reseat should
/// not produce a purchase recommendation, and a disk that has been in
/// `warning` across two nights is not having a bad moment.
pub const ADVICE_AFTER_DAYS: i64 = 2;

/// "Wymień, dopóki dysk jeszcze żyje": the recommendation for one disk, or
/// `None` when there is nothing to recommend.
///
/// Two independent triggers, both from history this node already keeps:
/// - the disk has been unhealthy for `ADVICE_AFTER_DAYS` — `warning_since` is
///   when the open health alert was raised, which is exactly when the health
///   last changed for the worse;
/// - the reallocated count GREW against the week-old sample. That one does not
///   wait: a moving reallocation count is the sign that the disk is failing
///   now. `score_health` grades it `warning` (the SPEC legend's "Uwaga"), so
///   the urgency lives HERE, in the `urgent` severity.
///
/// Pure over its inputs so both triggers are testable on a real history shape
/// without a disk being present.
pub fn replacement_advice(
    disk: &NasDisk,
    warning_since: Option<&str>,
    reallocated_week_ago: Option<i64>,
    spare_available: bool,
) -> Option<NasReplacementAdvice> {
    // A disk nobody's data depends on is not a replacement decision: an
    // unhealthy free disk is simply not put into a pool.
    if disk.health != "warning" && disk.health != "critical" {
        return None;
    }
    let days = warning_since
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| {
            chrono::Utc::now()
                .signed_duration_since(t.with_timezone(&chrono::Utc))
                .num_days()
        })
        .unwrap_or(0)
        .max(0);
    let growing = match (disk.reallocated_sectors, reallocated_week_ago) {
        (Some(now), Some(old)) => now as i64 > old,
        _ => false,
    };
    let long_enough = days >= ADVICE_AFTER_DAYS;
    if !growing && !long_enough {
        return None;
    }
    // `growing` is the urgent one whatever the health says: the counter is
    // moving, so the disk is not going to get better on its own.
    let severity = if growing || disk.health == "critical" {
        "urgent"
    } else {
        "advice"
    };
    // The English sentence (tooltip, deprecated for display) and the codes
    // the screens word are built side by side. They differ on purpose in two
    // places, both in favour of the reader: the codes say "for N days" from
    // the first whole day (the sentence only from `ADVICE_AFTER_DAYS`, which
    // an urgent growth does not wait for), and they leave out the disk's own
    // "reallocated sectors growing" when the growth part already says it.
    let mut reason = Vec::new();
    let mut reasons = Vec::new();
    if growing {
        let (now, old) = (
            disk.reallocated_sectors.unwrap_or(0),
            reallocated_week_ago.unwrap_or(0),
        );
        let grew = coded_reason("reallocated_grew", &[("from", old.to_string()), ("to", now.to_string())]);
        reason.push(disk_reason_sentence(&grew));
        reasons.push(grew);
    }
    if days >= 1 {
        let unhealthy = coded_reason(
            "unhealthy_for_days",
            &[("health", disk.health.clone()), ("days", days.to_string())],
        );
        if long_enough {
            reason.push(disk_reason_sentence(&unhealthy));
        }
        reasons.push(unhealthy);
    }
    if !disk.health_reason.is_empty() {
        reason.push(disk.health_reason.clone());
    }
    reasons.extend(
        disk.health_reasons
            .iter()
            .filter(|r| !(growing && r.code == "reallocated_growing"))
            .cloned(),
    );
    Some(NasReplacementAdvice {
        disk_id: disk.disk_id.clone(),
        name: disk.name.clone(),
        severity: severity.to_string(),
        reason: reason.join("; "),
        reasons,
        warning_days: days.min(i64::from(u32::MAX)) as u32,
        reallocated: disk.reallocated_sectors,
        reallocated_week_ago,
        member_of: disk.member_of.clone().unwrap_or_default(),
        spare_available,
    })
}

/// When the disk's health last turned bad: the `raised_at` of its OPEN health
/// alert. `sync_health_alert` closes and re-raises the row whenever the
/// severity changes, so that timestamp is the age of the CURRENT verdict and
/// not of the first one ever.
fn warning_since(db: &DbPool, disk_id: &str) -> Option<String> {
    store::alerts_for_subject(db, "disk", disk_id)
        .ok()?
        .into_iter()
        .find(|a| a.resolved_at.is_none())
        .map(|a| a.raised_at)
}

/// The recommendation for one disk, read against this node's stored history.
pub fn advice_for(db: &DbPool, disk: &NasDisk, spares: &[NasDisk]) -> Option<NasReplacementAdvice> {
    let week_ago = store::attribute_week_ago(db, &disk.disk_id, "reallocated").unwrap_or(None);
    // A spare in the SAME pool is what makes the replacement a hot swap; a
    // free disk elsewhere still has to be put in by hand.
    let spare_available = spares
        .iter()
        .any(|s| s.member_of == disk.member_of && s.health == "ok");
    replacement_advice(disk, warning_since(db, &disk.disk_id).as_deref(), week_ago, spare_available)
}

/// Every disk of this node that should be replaced while it still works, worst
/// first. Empty on a healthy node, which is what the UI shows nothing for.
pub fn advice(db: &DbPool, disks: &[NasDisk]) -> Vec<NasReplacementAdvice> {
    // `vdev_role`, NOT `role`: a hot spare is attached to the pool, so `role_of`
    // calls it `pool_member` and has no "spare" verdict to give — the spares
    // section of `zpool status` is what names it (pools.rs:107), and that lands
    // in `vdev_role`. Filtering on `role` matched nothing on every real node,
    // so "brak spare w puli" was advice this code could never not give.
    let spares: Vec<NasDisk> = disks.iter().filter(|d| d.vdev_role == "spare").cloned().collect();
    let mut out: Vec<NasReplacementAdvice> = disks
        .iter()
        .filter_map(|d| advice_for(db, d, &spares))
        .collect();
    out.sort_by(|a, b| {
        (a.severity != "urgent")
            .cmp(&(b.severity != "urgent"))
            .then(b.warning_days.cmp(&a.warning_days))
            .then(a.name.cmp(&b.name))
    });
    out
}

// ----- live state --------------------------------------------------------------------

struct Live {
    disk: NasDisk,
    history: VecDeque<u64>,
    /// Latest SMART counters, kept for the minute sample.
    smart: SmartSummary,
    /// SMART's own verdict (`score_health`), kept apart from `disk.health`:
    /// the shown health is this raised by the pool's leaf state
    /// (`grade_disk_health`), and the two are refreshed on different clocks —
    /// SMART every 30 minutes, the leaf state with every inventory pass.
    /// Recombining from the shown value would make a FAULTED verdict stick
    /// after the pool took the disk back.
    smart_health: String,
    smart_reason: String,
    /// SMART's verdict as codes, beside `smart_reason` (see
    /// `grade_disk_health` for when the two can disagree).
    smart_reasons: Vec<NasHealthReason>,
    /// The disk's leaf state in `zpool status`, `None` for no ZFS leaf.
    leaf_state: Option<String>,
    /// Whether this process has written the disk's health alert against its
    /// current grade. False on first sight and after a failed alert write.
    ///
    /// WHY a flag of its own and not an unset `leaf_state`: an alert left
    /// open by the previous process lifetime (a disk that was FAULTED, and has
    /// since left every pool) is graded `None` both before and after the
    /// first pass, so a leaf-state comparison never sees a change and the
    /// alert would stay open until a SMART pass — never, on a node without a
    /// privilege channel.
    alert_settled: bool,
    /// Whether the last inventory pass held this disk's alert back because
    /// the pool its label names is not imported yet (`awaiting_pool_import`).
    /// The SMART pass holds it back too: its grade has no leaf state to add.
    awaiting_import: bool,
}

struct State {
    disks: BTreeMap<String, Live>,
    /// Total IOPS of the node per tick, newest last — the last hour of it.
    iops_hour: VecDeque<f64>,
    prev_stats: HashMap<String, DiskStats>,
    prev_stats_at: Option<Instant>,
    last_inventory: Option<Instant>,
    last_sample: Option<Instant>,
    last_smart: Option<Instant>,
    last_prune: Option<Instant>,
    last_summary: Option<Instant>,
    /// When this process's disk state began: the boot window
    /// (`BOOT_IMPORT_GRACE`) is counted from here.
    started: Instant,
    telemetry: NasTelemetryState,
    inventory_error: Option<String>,
}

fn state() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(State::new()))
}

/// Serialises everything that writes a disk's health alert: a whole
/// inventory pass, and the alert write of each SMART read.
///
/// WHY: `refresh_inventory` is called by the sampler AND by dispatch (the
/// wipe plan, array create/add, the journal scan, the wipe job). Two passes
/// running at once grade under the state lock in one order and write their
/// alerts, after releasing it, in the other — the older pass's FAULTED raise
/// lands after the newer pass's resolve, and nothing re-writes it while the
/// leaf state stays put. A tokio mutex, because a pass holds it across the
/// `lsblk` and `zpool status` awaits: the read must not start before the
/// previous pass's writes are done either, or a pass could grade from a read
/// older than the one already applied.
fn health_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

impl State {
    fn new() -> Self {
        State {
            disks: BTreeMap::new(),
            iops_hour: VecDeque::with_capacity(IOPS_BASELINE_POINTS),
            prev_stats: HashMap::new(),
            prev_stats_at: None,
            last_inventory: None,
            last_sample: None,
            last_smart: None,
            last_prune: None,
            last_summary: None,
            started: Instant::now(),
            telemetry: NasTelemetryState {
                sampled_at: None,
                smart_read_at: None,
                smart_state: "pending".to_string(),
                detail: String::new(),
            },
            inventory_error: None,
        }
    }
}

/// Current disks with live I/O, the sparkline and the telemetry state.
pub fn snapshot() -> (Vec<NasDisk>, NasTelemetryState) {
    let st = state().read();
    let disks = st
        .disks
        .values()
        .map(|l| {
            let mut d = l.disk.clone();
            d.io_history_bps = l.history.iter().copied().collect();
            d
        })
        .collect();
    (disks, st.telemetry.clone())
}

/// Mean total IOPS of the node over the sampled hour, 0 before the sampler
/// has produced a single rate. The Overview tile shows the current value
/// against it ("+12% vs śr. godzinowa", n02).
pub fn iops_hour_avg() -> f64 {
    let st = state().read();
    if st.iops_hour.is_empty() {
        return 0.0;
    }
    st.iops_hour.iter().sum::<f64>() / st.iops_hour.len() as f64
}

pub fn disk(disk_id: &str) -> Option<NasDisk> {
    let st = state().read();
    st.disks.get(disk_id).map(|l| {
        let mut d = l.disk.clone();
        d.io_history_bps = l.history.iter().copied().collect();
        d
    })
}

/// Device path of a known disk, for privileged commands (the catalog then
/// validates it again — the id → path mapping is this node's own inventory,
/// never a caller-supplied path).
/// The kernel name (`sdg`) behind a `disk_id` (`wwn-5000cca27dc7a4c6`), for
/// the surfaces that must SHOW a disk rather than identify it. `disk()` would
/// answer too, but it clones the whole I/O history with it.
pub fn disk_name(disk_id: &str) -> Option<String> {
    state()
        .read()
        .disks
        .get(disk_id)
        .map(|l| l.disk.name.clone())
        .filter(|n| !n.is_empty())
}

// -----------------------------------------------------------------------------
// Naming a disk to the admin — ONE rule for every surface
// -----------------------------------------------------------------------------
//
// THE RULE (alerts, SMART job subjects and Elastic member cells all go through
// `shown_disk_name` / `pick_shown_name`):
//
//   1. A LIVE name when there is one: the kernel name the inventory holds for
//      the `disk_id` right now, or — for an Elastic member the inventory does
//      not hold yet (a core that has just started, a member whose id scheme
//      changed) — the kernel name the helper observed for that member's
//      device in the same read. Both are the device as it is NOW.
//   2. Otherwise the LAST KNOWN name the database remembers, and always
//      MARKED as last-known wherever it is shown ("last seen as sdq"). It is
//      never presented as the current device: once a disk is gone the kernel
//      may hand the same name to a different disk, and an unmarked `sdq`
//      would point the admin at the wrong drive in the shelf.
//   3. Otherwise no name — the surface says what the disk IS (its part in the
//      array, "a disk") and keeps the id in a tooltip. The id (`wwn-…`,
//      `sn-…`, `dev-…`) is NEVER the visible text.
//
// Whether a disk is ABSENT is a separate question with its own evidence (the
// helper's `device_present`), never inferred from a missing name.

/// What `shown_disk_name` found, and how it may be presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShownDiskName {
    /// The device as it is now — shown as-is.
    Live(String),
    /// The name it was last seen under — shown only marked as last-known.
    LastKnown(String),
    /// Nothing to name it by; the id stays out of the text.
    Unknown,
}

/// The rule itself, over its three inputs. `last_known` is only consulted
/// when neither live source has a name.
pub fn pick_shown_name(
    inventory: Option<String>,
    observed_now: Option<&str>,
    last_known: impl FnOnce() -> Option<String>,
) -> ShownDiskName {
    let named = |n: &str| {
        let n = n.trim();
        (!n.is_empty()).then(|| n.to_string())
    };
    if let Some(name) = inventory.as_deref().and_then(named) {
        return ShownDiskName::Live(name);
    }
    if let Some(name) = observed_now.and_then(named) {
        return ShownDiskName::Live(name);
    }
    match last_known().as_deref().and_then(named) {
        Some(name) => ShownDiskName::LastKnown(name),
        None => ShownDiskName::Unknown,
    }
}

/// `pick_shown_name` against this node's live inventory and its database.
pub fn shown_disk_name(db: &DbPool, disk_id: &str, observed_now: Option<&str>) -> ShownDiskName {
    pick_shown_name(disk_name(disk_id), observed_now, || {
        store::disk_last_name(db, disk_id).ok().flatten()
    })
}

pub fn device_path(disk_id: &str) -> Option<String> {
    state().read().disks.get(disk_id).map(|l| l.disk.path.clone())
}

fn stale(last: Option<Instant>, every: Duration) -> bool {
    last.is_none_or(|t| t.elapsed() >= every)
}

/// Re-reads lsblk and merges into the live map, keeping I/O history and SMART
/// of disks that are still there. A disk that disappeared is dropped from
/// the live view (its DB row stays for history).
///
/// One pass at a time (`health_gate`). A caller that finds a pass running
/// WAITS and then runs its own, rather than skipping or sharing the running
/// one's result: every dispatch caller reads `snapshot()`/`disk()` right
/// after this returns and acts on it — the wipe plan, the disks picked for an
/// Elastic create or add, the import candidates, the picture after a wipe —
/// so it needs a read that started after its own call, not one that may
/// predate the wipe or the pool change it is about to judge.
pub async fn refresh_inventory(db: &DbPool) -> Result<()> {
    inventory_pass(db, state(), health_gate(), || async {
        let found = inventory().await?;
        let (vdevs, leaf_states, imported) = vdev_membership().await;
        Ok::<InventoryRead, anyhow::Error>((found, vdevs, leaf_states, imported))
    })
    .await
}

/// What one inventory pass reads off the node: lsblk's disks, then
/// `vdev_membership`'s vdev index, leaf states and imported pools.
type InventoryRead = (
    Vec<NasDisk>,
    HashMap<String, (String, String)>,
    Option<HashMap<String, String>>,
    HashSet<String>,
);

/// How long after this process started a disk whose pool is not imported
/// yet keeps the health alert the previous process left (see
/// `awaiting_pool_import`). Pools import within seconds to a few minutes of
/// a boot; past this window an unimported pool is taken as gone for good
/// (exported, destroyed with its labels left on the disks), and its disks
/// settle like any disk with no leaf.
const BOOT_IMPORT_GRACE: Duration = Duration::from_secs(10 * 60);

/// Whether a disk's alert must wait for its pool to be imported.
///
/// THE BOOT CASE. After a reboot this process may start before the node has
/// imported its pools. `zpool list` then answers with no pool at all — a
/// complete, successful read — so a disk ZFS had FAULTED reads as "no leaf",
/// its first-sight settle closes the alert the previous process left open,
/// and the pool's import a minute later raises it again as a NEW alert: a new
/// timestamp, the acknowledgement lost, the replacement advice's clock
/// restarted. The disk says otherwise: its `zfs_member` label names a pool
/// (`role` 'pool_member', `member_of`), and a pool that is on the disk but
/// not in `zpool list` is one this node has not imported YET — its leaf
/// state is unknown, not healthy.
///
/// Only on first sight (`alert_settled` false: a restart, a hot-plug), and
/// only inside `BOOT_IMPORT_GRACE`: a disk this process has already settled
/// follows its leaf as always, and labels left behind on a destroyed pool's
/// disks do not hold an alert for ever.
fn awaiting_pool_import(live: &Live, imported: &HashSet<String>, in_boot_grace: bool) -> bool {
    in_boot_grace
        && !live.alert_settled
        && live.disk.role == "pool_member"
        && live
            .disk
            .member_of
            .as_deref()
            .is_some_and(|pool| !pool.is_empty() && !imported.contains(pool))
}

/// The disks of an inventory read that the live state does not hold yet —
/// the only ones whose persisted health has to be read (`inventory_pass`).
fn first_seen<'a>(
    found: &'a [NasDisk],
    known: &'a std::collections::HashSet<String>,
) -> impl Iterator<Item = &'a NasDisk> + 'a {
    found.iter().filter(move |d| !known.contains(&d.disk_id))
}

/// `refresh_inventory` over an injected read, state and gate, so the
/// serialisation of passes can be exercised without lsblk or `zpool status`
/// and without the process-wide state other tests share.
async fn inventory_pass<R, F>(
    db: &DbPool,
    cell: &RwLock<State>,
    gate: &tokio::sync::Mutex<()>,
    read: R,
) -> Result<()>
where
    R: FnOnce() -> F,
    F: std::future::Future<Output = Result<InventoryRead>>,
{
    // Held to the end of the pass, the alert writes and rollbacks included.
    let _pass = gate.lock().await;
    let (mut found, vdevs, leaf_states, imported) = match read().await {
        Ok(r) => r,
        Err(e) => {
            let mut st = cell.write();
            st.inventory_error = Some(e.to_string());
            st.last_inventory = Some(Instant::now());
            return Err(e);
        }
    };
    let arrays = array_membership(db);
    apply_membership(&mut found, &vdevs, &arrays);
    for d in &found {
        store::upsert_disk_seen(
            db,
            &DiskIdentity {
                disk_id: &d.disk_id,
                name: &d.name,
                model: &d.model,
                serial: &d.serial,
                wwn: d.wwn.as_deref(),
                size_bytes: d.size_bytes,
                kind: &d.kind,
            },
        )?;
    }
    // Health persisted from the previous process lifetime, so a restart does
    // not show every disk as "unknown" until the first SMART pass.
    //
    // Read ONLY for a disk this process has not seen yet: a disk already in
    // the live state keeps its live record (the `Some` arm below) and never
    // looks at the row. Reading every disk's row on every pass was a query
    // per disk on the pass that holds the health gate,
    // for an answer thrown away. The live set is stable while the gate is
    // held: only this pass replaces `st.disks`.
    let known: std::collections::HashSet<String> = cell.read().disks.keys().cloned().collect();
    let mut persisted = HashMap::new();
    for d in first_seen(&found, &known) {
        if let Some(row) = store::disk_row(db, &d.disk_id)? {
            persisted.insert(d.disk_id.clone(), row);
        }
    }
    let mut st = cell.write();
    let mut next = BTreeMap::new();
    let in_boot_grace = st.started.elapsed() < BOOT_IMPORT_GRACE;
    // Leaf-state changes, whose alerts are written once the lock is released:
    // `sync_health_alert` writes the database.
    let mut regrades = Vec::new();
    for mut d in found {
        let mut live = match st.disks.remove(&d.disk_id) {
            Some(mut old) => {
                // Identity/role/mounts from the fresh scan, everything SMART
                // and I/O from the live record.
                d.health = old.disk.health.clone();
                d.health_reason = old.disk.health_reason.clone();
                d.health_reasons = old.disk.health_reasons.clone();
                d.temperature_c = old.disk.temperature_c;
                d.power_on_hours = old.disk.power_on_hours;
                d.reallocated_sectors = old.disk.reallocated_sectors;
                d.pending_sectors = old.disk.pending_sectors;
                d.crc_errors = old.disk.crc_errors;
                d.media_errors = old.disk.media_errors;
                d.wear_pct = old.disk.wear_pct;
                d.smart_available = old.disk.smart_available;
                d.smart_passed = old.disk.smart_passed;
                d.smart_read_at = old.disk.smart_read_at.clone();
                d.io = old.disk.io.clone();
                if d.firmware.is_none() {
                    d.firmware = old.disk.firmware.take();
                }
                old.disk = d;
                old
            }
            None => {
                let row = persisted.get(&d.disk_id);
                live_from_persisted(d, row)
            }
        };
        // A read that could not see the pools holds every disk already
        // (`regrade_leaf`); one that saw them all holds a disk whose pool is
        // not imported yet, and leaves it unsettled for the pass that sees it.
        live.awaiting_import =
            leaf_states.is_some() && awaiting_pool_import(&live, &imported, in_boot_grace);
        if !live.awaiting_import {
            regrades.extend(regrade_leaf(&mut live, leaf_states.as_ref()));
        }
        next.insert(live.disk.disk_id.clone(), live);
    }
    st.disks = next;
    st.inventory_error = None;
    st.last_inventory = Some(Instant::now());
    drop(st);
    // One disk's failed alert write neither aborts the others nor is lost:
    // its leaf state is put back, so the next pass sees the same change and
    // writes the alert again. Until then the shown grade is the one its
    // alert says, not one no alert was ever raised for.
    let failed = settle_leaf_alerts(db, regrades);
    if !failed.is_empty() {
        let mut st = cell.write();
        for (regrade, e) in &failed {
            tracing::warn!(
                "tentanas: health alert of disk {} not written, retried next pass: {e}",
                regrade.disk_id
            );
            if let Some(live) = st.disks.get_mut(&regrade.disk_id) {
                roll_back_leaf(live, regrade);
            }
        }
    }
    Ok(())
}

/// A disk first seen in this process (a restart, a hot-plug), seeded from
/// its persisted row so a restart does not show every disk as "unknown"
/// until the first SMART pass. The persisted health is SMART's verdict
/// (`refresh_smart` stores it before the leaf state is applied), so it seeds
/// `smart_health` as it is.
///
/// The row keeps SMART's reasons both as the sentence and as codes
/// (`nas_disks.health_reasons`, migration 21, written by the same statement
/// in `store_smart`), so the codes come back as they were measured.
///
/// WHY stored rather than re-derived: grading the stored document again
/// needs the reallocated sample of a week ago, and that sample moves on
/// every minute — a restart a day after the read graded a different week and
/// had to drop the codes whenever the answer differed, leaving the screen on
/// the bare grade until the next SMART read. A row written before the column
/// existed has '[]': the disk shows the translated grade with the stored
/// sentence as its tooltip until its next SMART read — an honest gap, never
/// a reason the node did not measure.
fn live_from_persisted(mut d: NasDisk, row: Option<&store::DiskRow>) -> Live {
    let mut smart = SmartSummary::default();
    if let Some(row) = row {
        d.health = row.health.clone();
        d.health_reason = row.health_reason.clone();
        d.health_reasons = row.health_reasons.clone();
        d.smart_read_at = row.smart_read_at.clone();
        if let Some(doc) = row
            .smart_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        {
            smart = summarize_smart(&doc);
            apply_summary(&mut d, &smart);
        }
    }
    Live {
        smart_health: d.health.clone(),
        smart_reason: d.health_reason.clone(),
        smart_reasons: d.health_reasons.clone(),
        disk: d,
        history: VecDeque::with_capacity(HISTORY_POINTS),
        smart,
        leaf_state: None,
        // The first pass that reads the pools settles the disk's alert
        // against it, whatever the leaf state — `None` included.
        alert_settled: false,
        awaiting_import: false,
    }
}

/// A leaf-state change of one disk: found under the state lock, settled (its
/// alert written) after the lock is released, and undone if that write fails.
#[derive(Debug, Clone, PartialEq)]
struct LeafRegrade {
    disk_id: String,
    /// The leaf state before this pass, restored by `roll_back_leaf`.
    prior_leaf: Option<String>,
    leaf: Option<String>,
    health: String,
    reason: String,
    /// `reason` as codes, for the alert's coded detail.
    reasons: Vec<NasHealthReason>,
    /// The disk as regraded, for what the alert says beyond the grade
    /// (`health_alert_place`): its pool and whether to replace it.
    disk: NasDisk,
}

/// One inventory pass's regrade of one disk by its leaf state.
///
/// `leaf_states` is `vdev_membership`'s second map: `None` when a pool could
/// not be read, and then the last known leaf state stands — a failed `zpool
/// status` is no news about the disk, and treating it as "no leaf" would
/// close a FAULTED disk's alert and re-raise it, with a new timestamp, on the
/// next good read.
///
/// Any change of the leaf state is returned, even one that leaves the grade
/// as it was: the alert's text follows the reason ("ZFS reports this disk
/// FAULTED; …" on a disk SMART already calls critical), and settling an
/// unchanged alert costs one keyed read.
///
/// A disk whose alert is not settled (`Live::alert_settled`: first sight in
/// this process, or a failed write) is returned even with its leaf state
/// unchanged, so an alert the previous process left open is re-evaluated
/// against the current grade — a disk that was FAULTED and has since left
/// every pool reads `None` before and after. Not while the pools cannot be
/// read: the disk then stays unsettled until a pass that can.
fn regrade_leaf(
    live: &mut Live,
    leaf_states: Option<&HashMap<String, String>>,
) -> Option<LeafRegrade> {
    let leaf = leaf_states?.get(&live.disk.name).cloned();
    if live.leaf_state == leaf && live.alert_settled {
        return None;
    }
    live.alert_settled = true;
    let grade = grade_disk_health(
        &live.smart_health,
        &live.smart_reason,
        &live.smart_reasons,
        leaf.as_deref(),
    );
    let (health, reason) = show_grade(&mut live.disk, grade);
    let prior_leaf = std::mem::replace(&mut live.leaf_state, leaf.clone());
    Some(LeafRegrade {
        disk_id: live.disk.disk_id.clone(),
        prior_leaf,
        leaf,
        health,
        reason,
        reasons: live.disk.health_reasons.clone(),
        disk: live.disk.clone(),
    })
}

/// Undoes a regrade whose alert could not be written, so the next pass finds
/// the same change and writes it again. A later pass that already moved the
/// leaf on carries its own regrade, and is left alone.
fn roll_back_leaf(live: &mut Live, regrade: &LeafRegrade) {
    if live.leaf_state != regrade.leaf {
        return;
    }
    let grade = grade_disk_health(
        &live.smart_health,
        &live.smart_reason,
        &live.smart_reasons,
        regrade.prior_leaf.as_deref(),
    );
    show_grade(&mut live.disk, grade);
    live.leaf_state = regrade.prior_leaf.clone();
    // Also what makes a first-sight settle (no leaf change to find again)
    // retried: the alert table is not known to match any grade now.
    live.alert_settled = false;
}

/// Writes the health alert of every regrade and returns the ones that failed,
/// each with its error; one disk's failure does not stop the others.
fn settle_leaf_alerts(db: &DbPool, regrades: Vec<LeafRegrade>) -> Vec<(LeafRegrade, anyhow::Error)> {
    regrades
        .into_iter()
        .filter_map(|r| {
            write_health_alert(db, &r.disk_id, &r.health, &r.reason, &r.reasons, Some(&r.disk))
                .err()
                .map(|e| (r, e))
        })
        .collect()
}

/// The severity the alert moves FROM is the one of the disk's OPEN alert row,
/// not the grade this process showed before.
///
/// WHY: the shown grade is not what the alert table holds after a restart (a
/// disk seen for the first time has no shown grade of its own) or after a
/// failed write. Taken from the row, an unchanged severity refreshes the row
/// in place, so `warning_since` — and the replacement advice's two-day clock
/// — is not restarted by every restart of the node; and a RISEN one (an open
/// warning that is now critical) closes that row and raises a new one, with
/// its own `raised_at` and no acknowledgement carried over from the milder
/// condition. Refreshing it in place had kept the warning's timestamp and ack.
///
/// The SMART pass writes through here too: after a failed write the shown
/// grade it would otherwise move from is not what the table holds either.
///
/// `disk` is the disk as just graded, when the caller holds it: what the
/// alert says beyond the grade is read from it (`health_alert_place`).
fn write_health_alert(
    db: &DbPool,
    disk_id: &str,
    health: &str,
    reason: &str,
    reasons: &[NasHealthReason],
    disk: Option<&NasDisk>,
) -> Result<()> {
    let open = store::open_alert_severity(db, &health_alert_key(disk_id))?;
    let from = open.as_deref().unwrap_or("ok");
    // Only an alert that will stand needs its place: a healthy disk's write
    // closes the row, and the two reads behind the advice would be wasted.
    let place = disk
        .filter(|_| matches!(health, "warning" | "critical"))
        .map(|d| health_alert_place(db, d, from == health))
        .unwrap_or_default();
    sync_health_alert(db, disk_id, from, health, reason, reasons, &place)
}

/// What a disk's health alert says beyond its grade and reasons, decided at
/// raise time from what the node knows then:
/// - `pool`: the ZFS pool the disk serves and the layout of its group, the
///   n02 sub-line "tank · raidz2". Only a pool: an Elastic Array is an
///   organisation's, and a disk alert is node hardware every node admin
///   reads — it never names an array;
/// - `advise_replacement`: the node's own replacement advice holds for the
///   disk (`replacement_advice`, the same verdict the advice list gives),
///   the n01 suffix "— zaplanuj wymianę dysku". Never inferred by a screen.
///   Only for a ZFS pool member: replacing an Elastic member is not offered
///   in this version, and n03/n04 word its advice as "watch this disk"
///   (`replace_advice.array_*`) — the alert must not say otherwise.
#[derive(Debug, Default, Clone, PartialEq)]
struct HealthAlertPlace {
    pool: Option<(String, String)>,
    advise_replacement: bool,
}

/// `same_verdict`: the open alert already carries this grade, so its
/// `raised_at` is how long the disk has been in it — the advice's two-day
/// clock (`warning_since`). A changed grade is a fresh row: zero days.
///
/// The advice's clock moves without a new read, so a disk crosses the two
/// days between two writes of its alert; the next SMART pass (or leaf
/// change) rewrites the parameter. The advice list itself is always current.
fn health_alert_place(db: &DbPool, disk: &NasDisk, same_verdict: bool) -> HealthAlertPlace {
    let pool = match (disk.role.as_str(), disk.member_of.as_deref()) {
        ("pool_member", Some(pool)) if !pool.is_empty() && !disk.vdev_kind.is_empty() => {
            Some((pool.to_string(), disk.vdev_kind.clone()))
        }
        _ => None,
    };
    let advise_replacement = pool.is_some() && {
        let week_ago = store::attribute_week_ago(db, &disk.disk_id, "reallocated").unwrap_or(None);
        let since = if same_verdict { warning_since(db, &disk.disk_id) } else { None };
        replacement_advice(disk, since.as_deref(), week_ago, false).is_some()
    };
    HealthAlertPlace { pool, advise_replacement }
}

fn health_alert_key(disk_id: &str) -> String {
    format!("disk:{disk_id}:health")
}

fn apply_summary(d: &mut NasDisk, s: &SmartSummary) {
    d.smart_available = true;
    d.smart_passed = s.passed;
    d.temperature_c = s.temperature_c;
    d.power_on_hours = s.power_on_hours;
    d.reallocated_sectors = s.reallocated;
    d.pending_sectors = s.pending;
    d.crc_errors = s.crc_errors;
    d.media_errors = s.media_errors;
    d.wear_pct = s.wear_pct;
    if let Some(fw) = &s.firmware {
        d.firmware = Some(fw.clone());
    }
}

fn tick_io() {
    let Ok(text) = std::fs::read_to_string("/proc/diskstats") else {
        return;
    };
    let cur = parse_diskstats(&text);
    let now = Instant::now();
    let mut st = state().write();
    if let Some(prev_at) = st.prev_stats_at {
        let elapsed = now.duration_since(prev_at);
        let prev = std::mem::take(&mut st.prev_stats);
        let mut node_iops = 0.0;
        for live in st.disks.values_mut() {
            let (Some(p), Some(c)) = (prev.get(&live.disk.name), cur.get(&live.disk.name)) else {
                continue;
            };
            let io = io_rates(p, c, elapsed);
            if live.history.len() == HISTORY_POINTS {
                live.history.pop_front();
            }
            live.history.push_back(io.read_bps + io.write_bps);
            node_iops += io.read_iops + io.write_iops;
            live.disk.io = io;
        }
        if st.iops_hour.len() == IOPS_BASELINE_POINTS {
            st.iops_hour.pop_front();
        }
        st.iops_hour.push_back(node_iops);
    }
    st.prev_stats = cur;
    st.prev_stats_at = Some(now);
    st.telemetry.sampled_at = Some(store::now());
}

/// Reads SMART of every disk through the broker and rescores health; raises
/// or resolves the per-disk alert on transitions. Runs only when a
/// privilege channel exists — otherwise `telemetry.smart_state` says why.
pub async fn refresh_smart(db: &DbPool) -> Result<()> {
    if !broker::channel_available(db).await {
        let mut st = state().write();
        st.telemetry.smart_state = "unarmed".to_string();
        st.telemetry.detail = "no privilege channel: SMART needs root".to_string();
        return Ok(());
    }
    let targets: Vec<(String, String, String)> = state()
        .read()
        .disks
        .values()
        .map(|l| (l.disk.disk_id.clone(), l.disk.path.clone(), l.disk.kind.clone()))
        .collect();
    let mut failures = Vec::new();
    for (id, path, kind) in targets {
        match read_smart_document(db, &path, None).await {
            Ok(doc) => {
                let summary = summarize_smart(&doc);
                let week_ago = store::attribute_week_ago(db, &id, "reallocated").unwrap_or(None);
                let (smart_health, smart_reasons) = score_health(&summary, &kind, week_ago);
                // Not across an inventory pass: its alert writes and this
                // one would otherwise land in either order (`health_gate`).
                let _pass = health_gate().lock().await;
                settle_smart_read(db, state(), &id, &doc, summary, smart_health, smart_reasons);
            }
            Err(e) => failures.push(format!("{path}: {e}")),
        }
    }
    let mut st = state().write();
    st.last_smart = Some(Instant::now());
    st.telemetry.smart_read_at = Some(store::now());
    if failures.is_empty() {
        st.telemetry.smart_state = "ok".to_string();
        st.telemetry.detail = String::new();
    } else {
        st.telemetry.smart_state = "partial".to_string();
        st.telemetry.detail = failures.join("\n");
    }
    Ok(())
}

/// One disk's SMART read, applied: stored, graded into the live disk, and its
/// health alert written. Returns nothing on purpose — no write failing here
/// may end the pass over the remaining disks.
///
/// - A failed store is logged and the verdict still applied: it is a real
///   reading of the disk, and the next pass stores it again.
/// - A failed alert write is logged and leaves the disk unsettled
///   (`Live::alert_settled`), so the next inventory pass — seconds away, not
///   the next SMART pass half an hour away — writes it from the open row.
///   The verdict itself is kept, unlike the inventory pass's rollback: it
///   is already in the stored row, and rolling it back in memory would show
///   a grade that neither the row nor the disk says.
fn settle_smart_read(
    db: &DbPool,
    cell: &RwLock<State>,
    id: &str,
    doc: &Value,
    summary: SmartSummary,
    smart_health: &str,
    smart_reasons: Vec<NasHealthReason>,
) {
    // SMART's verdict is what persists: the leaf state is re-read on every
    // inventory pass, and a stored FAULTED would outlive the fault across a
    // restart. Its sentence AND its codes, in one statement, so a restart
    // shows the codes this read measured (`live_from_persisted`).
    if let Err(e) = store::store_smart(db, id, &doc.to_string(), smart_health, &smart_reasons) {
        tracing::warn!("tentanas: SMART verdict of disk {id} not stored: {e}");
    }
    let (health, reason, reasons, disk) = {
        let mut st = cell.write();
        let Some(live) = st.disks.get_mut(id) else { return };
        apply_summary(&mut live.disk, &summary);
        live.disk.smart_read_at = Some(store::now());
        live.smart = summary;
        let (health, reason) = apply_smart_verdict(live, smart_health, smart_reasons);
        // A disk whose pool is not imported yet has no leaf state for this
        // grade to add: writing its alert now would close a FAULTED one the
        // import is about to confirm (`awaiting_pool_import`). The verdict is
        // stored above; the inventory pass that sees the pool settles it.
        if live.awaiting_import {
            return;
        }
        (health, reason, live.disk.health_reasons.clone(), live.disk.clone())
    };
    if let Err(e) = write_health_alert(db, id, &health, &reason, &reasons, Some(&disk)) {
        tracing::warn!("tentanas: health alert of disk {id} not written, retried next pass: {e}");
        if let Some(live) = cell.write().disks.get_mut(id) {
            live.alert_settled = false;
        }
    }
}

/// Records SMART's own verdict on a live disk and re-grades it against the
/// leaf state it already has. Returns the shown health and reason.
fn apply_smart_verdict(
    live: &mut Live,
    smart_health: &str,
    smart_reasons: Vec<NasHealthReason>,
) -> (String, String) {
    let smart_reason = disk_reasons_text(&smart_reasons);
    let grade = grade_disk_health(smart_health, &smart_reason, &smart_reasons, live.leaf_state.as_deref());
    live.smart_health = smart_health.to_string();
    live.smart_reason = smart_reason;
    live.smart_reasons = smart_reasons;
    show_grade(&mut live.disk, grade)
}

/// One `smartctl --json=c -x` run. smartctl's exit code is a bitmask; only
/// bits 0–1 mean the command itself failed, the rest still come with a full
/// document.
pub async fn read_smart_document(
    db: &DbPool,
    device: &str,
    explicit: Option<&crate::profiling::collectors::elevation::ElevationToken>,
) -> Result<Value> {
    let (out, _) = broker::run_privileged(
        db,
        &HelperCommand::SmartctlInfo {
            device: device.to_string(),
        },
        explicit,
        Duration::from_secs(60),
    )
    .await?;
    smart_document_from_output(&out)
}

/// The exit-code bits that mean no document came back: bit 0 "command line did
/// not parse", bit 1 "device open failed". Bit 2 is deliberately NOT here — it
/// means "a SMART or other ATA command failed", which a SAS/SCSI disk reports
/// routinely because the ATA-specific command does not apply to it, while still
/// printing a complete SCSI document. Bits 3–7 are disk FINDINGS (failing,
/// prefail, error-log records) and have always come with a full document.
const SMARTCTL_NO_DOCUMENT: i32 = 0b011;

fn smart_document_from_output(out: &broker::CommandOutput) -> Result<Value> {
    if out.code & SMARTCTL_NO_DOCUMENT != 0 || out.stdout.trim().is_empty() {
        return Err(anyhow!(
            "smartctl failed ({}): {}",
            out.code,
            smartctl_failure_detail(out)
        ));
    }
    Ok(serde_json::from_str(&out.stdout)?)
}

/// What to tell the admin when the run really did fail. With `--json` smartctl
/// reports through `smartctl.messages` on stdout and leaves stderr empty by
/// design, so reading stderr alone produced "no output" even when stdout held a
/// whole document.
pub(super) fn smartctl_failure_detail(out: &broker::CommandOutput) -> String {
    let doc = serde_json::from_str::<Value>(&out.stdout).ok();
    // smartctl appends informational lines to this same array (`jinf()`), so
    // joining all of it made a failure lead with a note that was not the
    // failure. Informational text is still better than no reason at all, so it
    // becomes the fallback rather than being dropped; a message carrying no
    // severity is an unknown shape, not an informational one, and is kept.
    let spoken = doc
        .as_ref()
        .and_then(|d| d.pointer("/smartctl/messages"))
        .and_then(Value::as_array)
        .and_then(|msgs| {
            let join = |serious: bool| {
                msgs.iter()
                    .filter(|m| {
                        let severity = m.get("severity").and_then(Value::as_str).unwrap_or("");
                        (severity != "information") == serious
                    })
                    .filter_map(|m| m.get("string").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let text = match join(true) {
                t if t.is_empty() => join(false),
                t => t,
            };
            (!text.is_empty()).then_some(text)
        });
    if let Some(spoken) = spoken {
        return spoken;
    }
    if let Some(line) = out.stderr.trim().lines().next() {
        return line.to_string();
    }
    let stdout = out.stdout.trim();
    if stdout.is_empty() {
        "no output".to_string()
    } else {
        // Output WAS there, smartctl just gave no reason; say so honestly
        // rather than claiming the run printed nothing.
        format!("no diagnostic message ({} bytes on stdout)", stdout.len())
    }
}

/// The disk's health alert (code 'disk_health', see `NasAlert::code`): the
/// grade and the name as parameters, the reasons as coded lines, and what
/// `HealthAlertPlace` adds (`pool` + `layout`, `advice`). The English title
/// and detail are written beside them for forwarding and the tooltip.
fn health_alert_text(
    health: &str,
    name: &ShownDiskName,
    reason: &str,
    reasons: &[NasHealthReason],
    place: &HealthAlertPlace,
) -> store::AlertText {
    let (title, source, shown) = match name {
        ShownDiskName::Live(name) => (format!("Disk {name}: {health}"), "live", Some(name)),
        ShownDiskName::LastKnown(name) => (format!("Disk last seen as {name}: {health}"), "last_known", Some(name)),
        ShownDiskName::Unknown => (format!("Disk: {health}"), "unknown", None),
    };
    let mut text = store::AlertText::new("disk_health", title, reason)
        .param("health", health)
        .param("name_source", source)
        .reasons(reasons.to_vec());
    if let Some((pool, layout)) = &place.pool {
        text = text.param("pool", pool).param("layout", layout);
    }
    if place.advise_replacement {
        text = text.param("advice", "replace");
    }
    match shown {
        Some(name) => text.param("name", name),
        None => text,
    }
}

fn sync_health_alert(
    db: &DbPool,
    disk_id: &str,
    previous: &str,
    health: &str,
    reason: &str,
    reasons: &[NasHealthReason],
    place: &HealthAlertPlace,
) -> Result<()> {
    let key = health_alert_key(disk_id);
    match health {
        "critical" | "warning" => {
            if previous != health {
                // Severity changed: close the old row so the new severity
                // gets its own timestamp.
                store::resolve_alert(db, &key)?;
            }
            // Named by THE rule (`shown_disk_name`). A disk that has LEFT the
            // inventory is exactly the one whose alert matters most: it is
            // titled by the name it was last seen under, said as such, and
            // never by its id — which stays the alert's subject, not its text.
            let text = health_alert_text(health, &shown_disk_name(db, disk_id, None), reason, reasons, place);
            store::raise_coded_alert(db, &key, health, "disk", disk_id, &text)?;
        }
        _ => store::resolve_alert(db, &key)?,
    }
    Ok(())
}

fn persist_minute_sample(db: &DbPool) -> Result<()> {
    let at = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:00Z")
        .to_string();
    let st = state().read();
    let rows: Vec<(String, SmartSummary, NasDiskIo)> = st
        .disks
        .values()
        .map(|l| (l.disk.disk_id.clone(), l.smart.clone(), l.disk.io.clone()))
        .collect();
    drop(st);
    let samples: Vec<SampleInsert<'_>> = rows
        .iter()
        .map(|(id, s, io)| SampleInsert {
            disk_id: id,
            at: &at,
            temperature_c: s.temperature_c,
            reallocated: s.reallocated,
            pending: s.pending,
            crc_errors: s.crc_errors,
            media_errors: s.media_errors,
            read_bps: io.read_bps,
            write_bps: io.write_bps,
            await_ms: io.await_ms,
        })
        .collect();
    store::insert_samples(db, &samples)
}

async fn tick(db: &DbPool) {
    let (inv, sample, smart, prune) = {
        let st = state().read();
        (
            stale(st.last_inventory, INVENTORY_EVERY),
            stale(st.last_sample, SAMPLE_EVERY),
            stale(st.last_smart, SMART_EVERY),
            stale(st.last_prune, PRUNE_EVERY),
        )
    };
    if inv {
        if let Err(e) = refresh_inventory(db).await {
            tracing::warn!("tentanas: disk inventory failed: {e}");
        }
    }
    tick_io();
    if smart {
        if let Err(e) = refresh_smart(db).await {
            tracing::warn!("tentanas: SMART refresh failed: {e}");
        }
    }
    if sample {
        if let Err(e) = persist_minute_sample(db) {
            tracing::warn!("tentanas: sample persist failed: {e}");
        }
        // Pools ride the same cadence: one `zpool iostat` sample per minute
        // feeds both the live pool cards and their 24 h chart.
        super::pools::persist_sample(db).await;
        state().write().last_sample = Some(Instant::now());
    }
    if prune {
        match store::prune_samples(db) {
            Ok(n) if n > 0 => tracing::info!("tentanas: pruned {n} disk samples"),
            Ok(_) => {}
            Err(e) => tracing::warn!("tentanas: sample prune failed: {e}"),
        }
        if let Err(e) = store::prune_pool_samples(db) {
            tracing::warn!("tentanas: pool sample prune failed: {e}");
        }
        state().write().last_prune = Some(Instant::now());
    }
}

/// Starts the per-node sampler once per process. Called from the native
/// init hook; a second call (reconcile re-runs init) is a no-op.
pub fn start_sampler(main_db: DbPool, addon_id: String, db: DbPool) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("tentanas: no tokio runtime, disk sampler not started");
        return;
    };
    handle.spawn(async move {
        loop {
            tick(&db).await;
            if stale(state().read().last_summary, SUMMARY_EVERY) {
                super::fleet::publish_local_summary(&main_db, &addon_id, &db).await;
                state().write().last_summary = Some(Instant::now());
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// Forces a SMART pass on the next tick (after arming, after provisioning).
pub fn request_smart_refresh() {
    state().write().last_smart = None;
}

/// Republishes this node's fleet summary on the next tick instead of up to a
/// minute later: an arm or a disarm changes `armed_until`, and the other
/// nodes' fleet and Environment screens read it from that row.
pub fn request_summary_refresh() {
    state().write().last_summary = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `score_health` with its reasons as the English sentence the alert and
    /// the tooltip carry — what every grading test below was written against.
    fn scored(s: &SmartSummary, kind: &str, week_ago: Option<i64>) -> (&'static str, String) {
        let (health, reasons) = score_health(s, kind, week_ago);
        (health, disk_reasons_text(&reasons))
    }

    /// `grade_disk_health` over a SMART sentence alone, as the tuple the
    /// grading tests compare.
    fn graded(smart_health: &str, smart_reason: &str, leaf: Option<&str>) -> (String, String) {
        let g = grade_disk_health(smart_health, smart_reason, &[], leaf);
        (g.health, g.reason)
    }

    /// `smartctl --json=c -x /dev/sdb` of a TOSHIBA MG07SCA14TE behind a SAS
    /// expander, copied byte for byte off the node this fix was measured on.
    const SMART_SAS: &str = include_str!("../../tests/fixtures/smart-sas-sdb.json");

    /// `smartctl --json=c -x /dev/nvme0` off the node this area was measured
    /// on, hardware identifiers scrubbed. Its self-test log is the shape the
    /// synthetic documents cannot produce: `nvme_self_test_log` is PRESENT but
    /// has **no `table` key at all** (no self-test has ever run), and
    /// `current_self_test_completion_percent` is absent because smartctl emits
    /// it only while a test is actually running.
    const SMART_NVME: &str = include_str!("../../tests/fixtures/smart-nvme-nvme0.json");

    const LSBLK: &str = r#"{"blockdevices":[
      {"name":"sda","path":"/dev/sda","type":"disk","model":"WDC WD80EFZZ","serial":"WD-1","wwn":"0x5000cca","size":8001563222016,"tran":"sata","rota":true,"rm":false,"rev":"81.00","vendor":"ATA     ","mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"sda1","path":"/dev/sda1","type":"part","mountpoints":[null],"fstype":"zfs_member","label":"tank"}]},
      {"name":"nvme0n1","path":"/dev/nvme0n1","type":"disk","model":"Samsung 980","serial":"S1","wwn":null,"size":1000204886016,"tran":"nvme","rota":false,"rm":false,"rev":null,"vendor":null,"mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"nvme0n1p2","path":"/dev/nvme0n1p2","type":"part","mountpoints":["/"],"fstype":"ext4","label":null}]},
      {"name":"sdb","path":"/dev/sdb","type":"disk","model":"ST4000","serial":"Z4","wwn":null,"size":"4000787030016","tran":"sata","rota":"1","rm":"0","rev":null,"vendor":"ATA","mountpoints":[null],"fstype":null,"label":null},
      {"name":"sdc","path":"/dev/sdc","type":"disk","model":"HUS726","serial":"K5","wwn":null,"size":6001175126016,"tran":"sas","rota":true,"rm":false,"rev":"A3","vendor":"HGST","mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"sdc1","path":"/dev/sdc1","type":"part","mountpoints":[null],"fstype":"ext4","label":"old-data"}]},
      {"name":"loop0","path":"/dev/loop0","type":"loop","size":1,"mountpoints":["/snap/x"]},
      {"name":"zd0","path":"/dev/zd0","type":"disk","size":1,"mountpoints":[null]}
    ]}"#;

    // ----- clearing a disk -----------------------------------------------------

    /// The shape the whole feature exists for: a disk left over from a
    /// DISSOLVED Elastic Array. It carries a bare `xfs` signature, belongs to
    /// no pool and to no array this node has a database row for, and therefore
    /// reads as the catch-all `used` — which is exactly what makes it unusable
    /// for a new pool and what three user-space guards once read as "free".
    fn used_disk() -> NasDisk {
        NasDisk {
            disk_id: "wwn-produkt-data".to_string(),
            name: "sdc".to_string(),
            path: "/dev/sdc".to_string(),
            kind: "hdd".to_string(),
            model: "HGST HUS726".to_string(),
            serial: "serial-produkt-data".to_string(),
            wwn: Some("wwn-produkt-data".to_string()),
            size_bytes: 32 * 1024 * 1024 * 1024,
            role: "used".to_string(),
            fs_type: Some("xfs".to_string()),
            fs_label: Some("d1".to_string()),
            fs_uuid: Some("33333333-3333-4333-8333-333333333333".to_string()),
            ..Default::default()
        }
    }

    fn journal_entry(
        spec: tentanas_helper::elastic::ElasticCreateSpec,
        union_mounted: Option<bool>,
    ) -> tentanas_helper::elastic::ElasticJournalEntry {
        tentanas_helper::elastic::ElasticJournalEntry { spec, union_mounted }
    }

    /// The plan names WHAT would be removed, not merely that something would.
    /// `fs_type` alone covered 23 of 29 disks on a real node and told the
    /// reader nothing; the label and the filesystem UUID are what identify the
    /// filesystem being erased rather than the device node it answers on.
    #[test]
    fn the_wipe_plan_names_the_signature_label_and_filesystem_uuid() {
        let disk = used_disk();
        let plan = plan_wipe(&disk, None);
        assert!(plan.allowed, "{:?}", plan.refusals);
        assert!(plan.refusals.is_empty());
        assert_eq!(plan.fs_type.as_deref(), Some("xfs"));
        assert_eq!(plan.fs_label.as_deref(), Some("d1"));
        assert_eq!(plan.fs_uuid.as_deref(), Some("33333333-3333-4333-8333-333333333333"));
        assert_eq!(plan.name, "sdc");
        assert_eq!(plan.path, "/dev/sdc");
        assert_eq!(plan.size_bytes, disk.size_bytes);
        assert!(plan.journal_claim.is_none());
        // Read-only, and the plan is the ONE thing the plan request produces:
        // the privileged command that erases a disk is built by `wipe_command`
        // and cannot be reached from here. It refuses the plan on its own,
        // with no typed confirmation.
        assert!(matches!(
            wipe_command(&plan, &disk, "", ""),
            Err(WipeRefusal::BadRequest(_))
        ));
        assert_eq!(disk, used_disk(), "plan_wipe nie zmienia dysku, o który pyta");
    }

    /// One refusal per cause, each firing for its OWN cause and each naming
    /// both the cause and the remedy. A sentence an admin cannot act on is the
    /// failure this plan exists to prevent.
    #[test]
    fn every_wipe_plan_refusal_fires_for_its_own_cause_and_names_a_remedy() {
        let system = NasDisk {
            role: "system".to_string(),
            mountpoints: vec!["/".to_string(), "/boot/efi".to_string()],
            ..used_disk()
        };
        let plan = plan_wipe(&system, None);
        assert!(!plan.allowed);
        assert_eq!(plan.refusals.len(), 1, "{:?}", plan.refusals);
        assert_eq!(plan.refusals[0].code, "system");
        assert!(plan.refusals[0].detail.contains("dysk systemowy"), "{:?}", plan.refusals[0]);
        assert!(plan.refusals[0].detail.contains("/boot/efi"), "{:?}", plan.refusals[0]);
        assert!(plan.refusals[0].detail.contains("przenieś system"), "{:?}", plan.refusals[0]);

        let pool = NasDisk {
            role: "pool_member".to_string(),
            member_of: Some("tank".to_string()),
            ..used_disk()
        };
        let plan = plan_wipe(&pool, None);
        assert_eq!(plan.refusals[0].code, "zfs_pool");
        assert!(plan.refusals[0].detail.contains("puli ZFS tank"));
        assert!(plan.refusals[0].detail.contains("zniszcz pulę"));

        // An mdraid member and an Elastic Array member share `role`; what
        // separates them is `array_role`, which only an Elastic member has.
        let md = NasDisk {
            role: "array_member".to_string(),
            member_of: Some("nas:0".to_string()),
            ..used_disk()
        };
        let plan = plan_wipe(&md, None);
        assert_eq!(plan.refusals[0].code, "mdraid");
        assert!(plan.refusals[0].detail.contains("mdraid nas:0"));
        assert!(plan.refusals[0].detail.contains("mdadm --stop"));

        let elastic = NasDisk {
            role: "array_member".to_string(),
            member_of: Some("produkt".to_string()),
            array_role: "parity".to_string(),
            ..used_disk()
        };
        let plan = plan_wipe(&elastic, None);
        assert_eq!(plan.refusals[0].code, "elastic_member");
        assert!(plan.refusals[0].detail.contains("macierzy Elastic produkt"));
        assert!(plan.refusals[0].detail.contains("parity"));
        assert!(plan.refusals[0].detail.contains("rozwiąż macierz"));

        let mounted = NasDisk {
            role: "mounted".to_string(),
            mountpoints: vec!["/srv/stare".to_string()],
            ..used_disk()
        };
        let plan = plan_wipe(&mounted, None);
        assert_eq!(plan.refusals[0].code, "mounted");
        assert!(plan.refusals[0].detail.contains("/srv/stare"));
        assert!(plan.refusals[0].detail.contains("odmontuj"));

        // A mount the ROLE did not already explain still refuses: the role is
        // decided by the first rule that matches, and a signature-classified
        // disk with a mountpoint would otherwise sail through.
        let odd = NasDisk { mountpoints: vec!["/mnt/x".to_string()], ..used_disk() };
        let plan = plan_wipe(&odd, None);
        assert_eq!(plan.refusals[0].code, "mounted");
        assert!(!plan.allowed);
    }

    /// No organisation of this node is involved: every other org is one the
    /// node has never heard of, which is what these tests were written for.
    fn no_orgs() -> std::collections::BTreeSet<String> {
        std::collections::BTreeSet::new()
    }

    /// The owner's rule of 2026-09-22 on the wipe path: a disk that belongs
    /// to a journal of ANOTHER ORGANISATION THAT EXISTS ON THIS NODE is
    /// refused — served, dissolved or unknown alike — and nothing about that
    /// array reaches the plan: not its name, id, role, size or owner ids.
    #[test]
    fn a_disk_of_another_org_on_this_node_is_refused_and_its_array_name_is_not_sent() {
        let disk = used_disk();
        let spec = crate::tentanas::elastic::tests::create_spec("produkt");
        // The journal's owner org ("org") is a tenant of this node; the
        // admin asking is another one.
        let orgs: std::collections::BTreeSet<String> = ["org".to_string(), "inna-org".to_string()].into();
        let asking = tentanas_helper::elastic::ElasticOwner { org_id: "inna-org".into(), addon_id: spec.owner.addon_id.clone() };
        for serving in [Some(false), Some(true), None] {
            let claim = journal_claim_of(&disk, &[journal_entry(spec.clone(), serving)], &asking, &orgs)
                .expect("the disk is still claimed");
            assert_eq!(claim.claim.owner_kind, crate::tentanas::elastic::OWNER_KIND_OTHER_ORG_ON_NODE);
            assert_eq!(
                claim.claim,
                NasDiskWipeJournalClaim {
                    owner_foreign: true,
                    owner_kind: crate::tentanas::elastic::OWNER_KIND_OTHER_ORG_ON_NODE.to_string(),
                    ..Default::default()
                },
                "nothing but the code"
            );
            let plan = plan_wipe(&disk, Some(claim));
            assert!(!plan.allowed, "{serving:?}");
            assert_eq!(plan.refusals.len(), 1, "{:?}", plan.refusals);
            assert_eq!(plan.refusals[0].code, JOURNAL_OTHER_ORG);
            assert!(plan.refusals[0].detail.contains("innej organizacji na tym węźle"), "{:?}", plan.refusals[0]);
            assert!(plan.journal_claim.is_none(), "no claim to acknowledge, and none to read a name from");
            // What the plan says about the claim, as it would be serialised:
            // the array's name and id appear nowhere in it. (Only these two
            // fields — the disk's own serial and WWN in this fixture contain
            // the word too, and those are the disk's, not the array's.)
            let wire = serde_json::to_string(&(&plan.refusals, &plan.journal_claim)).unwrap();
            assert!(!wire.contains("produkt") && !wire.contains(&spec.array_id), "{wire}");
            assert!(serde_json::to_string(&plan).unwrap().find(&spec.array_id).is_none());

            // Refused on the node, whatever the request acknowledges — the
            // array's real name included, as a client that guessed it would.
            for ack in ["", "produkt"] {
                assert!(
                    matches!(wipe_command(&plan, &disk, "sdc", ack), Err(WipeRefusal::NotAvailable(_))),
                    "{serving:?} / '{ack}'"
                );
            }
        }
    }

    /// A member of ANOTHER organisation's live Elastic Array as the node's
    /// inventory really holds it: named by `apply_array_membership`, and
    /// MOUNTED at its branch, whose path spells the array
    /// (`/mnt/tentanas-branches/<array>/…`, lsblk's mountpoints of the disk
    /// and its partition). Its hardware identity names nothing, so the whole
    /// serialised disk can be searched for the array's name.
    fn other_org_member(array_role: &str) -> NasDisk {
        let part = if array_role == "parity" { "parity/1".to_string() } else { format!("{array_role}/d1") };
        NasDisk {
            disk_id: "wwn-0x5000c500a1b2c3d4".to_string(),
            serial: "ZL2ABCDE".to_string(),
            wwn: Some("0x5000c500a1b2c3d4".to_string()),
            role: "array_member".to_string(),
            member_of: Some("produkt".to_string()),
            array_role: array_role.to_string(),
            mountpoints: vec![format!("/mnt/tentanas-branches/produkt/{part}")],
            fs_type: None,
            fs_label: None,
            fs_uuid: Some("33333333-3333-4333-8333-333333333333".to_string()),
            ..used_disk()
        }
    }

    /// A disk of ANOTHER organisation's live array on the Disks tab: the
    /// shared inventory names that array (`apply_array_membership` reads every
    /// tenant's rows) and lsblk reports its branch mount, so the copy a tenant
    /// receives says only "another organisation's array" — no name, no part,
    /// no branch filesystem, no mount path.
    #[test]
    fn a_disk_of_another_organisations_live_array_reaches_the_tenant_without_the_array_name() {
        let theirs = other_org_member("data");
        assert!(serde_json::to_string(&theirs).unwrap().contains("/mnt/tentanas-branches/produkt/"),
            "the fixture carries the name where a real mounted branch does");
        let own: std::collections::BTreeSet<String> = ["media".to_string()].into();
        let mut shown = theirs.clone();
        hide_other_org_array(&mut shown, &own);
        assert_eq!(shown.role, ROLE_OTHER_ORG_ARRAY);
        assert_eq!(shown.member_of, None);
        assert!(shown.array_role.is_empty());
        assert!(shown.mountpoints.is_empty(), "{:?}", shown.mountpoints);
        assert_eq!((shown.fs_uuid.as_deref(), shown.fs_label.as_deref(), shown.fs_type.as_deref()), (None, None, None));
        // The property the name promises: the WHOLE disk as it goes out.
        let wire = serde_json::to_string(&shown).unwrap();
        assert!(!wire.contains("produkt"), "{wire}");
        assert!(!wire.contains("tentanas-branches"), "{wire}");
        // The disk itself stays: it is shared hardware, not the array.
        assert_eq!((shown.disk_id.as_str(), shown.name.as_str(), shown.serial.as_str()),
            (theirs.disk_id.as_str(), theirs.name.as_str(), theirs.serial.as_str()));

        // The owner's own array keeps its name, its part and its mount.
        let own_disk = NasDisk {
            member_of: Some("media".to_string()),
            mountpoints: vec!["/mnt/tentanas-branches/media/data/d1".to_string()],
            ..theirs.clone()
        };
        let mut kept = own_disk.clone();
        hide_other_org_array(&mut kept, &own);
        assert_eq!(kept, own_disk);

        // A pool or an mdraid set is named by the disk's own label and owned
        // by no tenant: untouched.
        for disk in [
            NasDisk { role: "pool_member".to_string(), member_of: Some("tank".to_string()), ..used_disk() },
            NasDisk { role: "array_member".to_string(), member_of: Some("nas:0".to_string()), ..used_disk() },
        ] {
            let mut same = disk.clone();
            hide_other_org_array(&mut same, &own);
            assert_eq!(same, disk);
        }
    }

    /// A disk whose id has DRIFTED (N2): the inventory no longer recognises
    /// it as an Elastic member at all — a WWN that stops reporting behind a
    /// new HBA or USB bridge changes `disk_id`, so `array_membership` (keyed
    /// on `disk_id`) no longer matches it, and it falls through to plain
    /// `role = "mounted"`. Its mountpoint still spells another
    /// organisation's array by path, and `hide_other_org_array` must scrub
    /// it even though the `array_member` arm never runs.
    #[test]
    fn a_mounted_disk_under_another_organisations_branch_names_no_array() {
        let own: std::collections::BTreeSet<String> = ["media".to_string()].into();
        // Based on `other_org_member`, not `used_disk()` directly: that
        // fixture's hardware identity (`disk_id`/`serial`/`wwn`) is chosen to
        // NOT already contain "produkt", so a `contains("produkt")` search
        // below can only find a leak through `mountpoints`, never a
        // coincidence of the fixture's own naming.
        let mut drifted = NasDisk {
            role: "mounted".to_string(),
            member_of: None,
            array_role: String::new(),
            ..other_org_member("data")
        };
        assert!(drifted.mountpoints[0].starts_with("/mnt/tentanas-branches/produkt/"),
            "{:?}", drifted.mountpoints);
        hide_other_org_array(&mut drifted, &own);
        assert!(drifted.mountpoints.is_empty(), "{:?}", drifted.mountpoints);
        // Role is untouched — this disk was never recognised as a member —
        // but the whole serialised disk must not carry the other org's name.
        assert_eq!(drifted.role, "mounted");
        let wire = serde_json::to_string(&drifted).unwrap();
        assert!(!wire.contains("produkt"), "{wire}");
        assert!(!wire.contains("tentanas-branches"), "{wire}");

        // Mounted under the caller's OWN array: the path is kept.
        let mut own_mount = NasDisk {
            role: "mounted".to_string(),
            member_of: None,
            array_role: String::new(),
            mountpoints: vec!["/mnt/tentanas-branches/media/data/d1".to_string()],
            ..other_org_member("data")
        };
        let before = own_mount.mountpoints.clone();
        hide_other_org_array(&mut own_mount, &own);
        assert_eq!(own_mount.mountpoints, before);
    }

    /// The wipe plan of that disk refuses with the one "another organisation"
    /// sentence — where the Elastic member arm would have printed
    /// "macierzy Elastic produkt" and the dialog "zamontowany w
    /// /mnt/tentanas-branches/produkt/…" — and says it ONCE when the array's
    /// journal refuses the same disk as well. The whole plan is searched.
    #[test]
    fn the_wipe_plan_of_another_organisations_live_array_member_names_no_array() {
        let mut disk = other_org_member("parity");
        hide_other_org_array(&mut disk, &std::collections::BTreeSet::new());
        // The journal names its member by the identity the disk really has.
        let mut spec = crate::tentanas::elastic::tests::create_spec("produkt");
        spec.parity[0].disk_id = disk.disk_id.clone();
        spec.parity[0].wwn = disk.wwn.clone();
        spec.parity[0].serial = Some(disk.serial.clone());
        let orgs: std::collections::BTreeSet<String> = ["org".to_string(), "inna-org".to_string()].into();
        let asking = tentanas_helper::elastic::ElasticOwner { org_id: "inna-org".into(), addon_id: spec.owner.addon_id.clone() };
        let with_journal = journal_claim_of(&disk, &[journal_entry(spec, Some(true))], &asking, &orgs);
        assert!(with_journal.is_some());
        for claim in [None, with_journal] {
            let plan = plan_wipe(&disk, claim);
            assert!(!plan.allowed);
            assert_eq!(plan.refusals.len(), 1, "{:?}", plan.refusals);
            assert_eq!(plan.refusals[0].code, JOURNAL_OTHER_ORG);
            assert!(plan.mountpoints.is_empty(), "{:?}", plan.mountpoints);
            assert!(plan.fs_uuid.is_none() && plan.fs_label.is_none());
            let wire = serde_json::to_string(&plan).unwrap();
            assert!(!wire.contains("produkt"), "{wire}");
            assert!(!wire.contains("tentanas-branches"), "{wire}");
            assert!(matches!(wipe_command(&plan, &disk, "sdc", "produkt"), Err(WipeRefusal::NotAvailable(_))));
        }
        // Even a plan built from a disk that was NOT passed through the hide
        // step but carries the role prints no mount path.
        let unhidden = NasDisk { role: ROLE_OTHER_ORG_ARRAY.to_string(), ..other_org_member("data") };
        assert!(plan_wipe(&unhidden, None).mountpoints.is_empty());
    }

    /// The other half of the rule: a journal whose organisation this node has
    /// NO record of (disks moved in from another machine) is claimed exactly
    /// as before — named, acknowledgeable and wipeable once acknowledged.
    #[test]
    fn a_disk_of_an_org_unknown_to_this_node_keeps_its_claim_and_can_be_wiped() {
        let disk = used_disk();
        let spec = crate::tentanas::elastic::tests::create_spec("produkt");
        let orgs: std::collections::BTreeSet<String> = ["inna-org".to_string()].into();
        let asking = tentanas_helper::elastic::ElasticOwner { org_id: "inna-org".into(), addon_id: spec.owner.addon_id.clone() };
        let claim = journal_claim_of(&disk, &[journal_entry(spec.clone(), Some(false))], &asking, &orgs)
            .expect("claimed");
        assert_eq!(claim.claim.owner_kind, crate::tentanas::elastic::OWNER_KIND_OTHER_INSTALLATION);
        assert_eq!(claim.claim.name, "produkt");
        assert_eq!((claim.claim.owner_org_id.as_str(), claim.claim.owner_addon_id.as_str()), ("", ""));
        let plan = plan_wipe(&disk, Some(claim));
        assert!(plan.allowed, "{:?}", plan.refusals);
        let command = wipe_command(&plan, &disk, "sdc", "produkt").expect("acknowledged by name");
        assert!(matches!(command, HelperCommand::DiskWipe { release_journal: Some(ref id), .. } if *id == spec.array_id));
    }

    /// The journal question, in all three states a dissolved array can be in.
    ///
    /// The claim is reported apart from the refusals ONLY when it can be
    /// acknowledged: a serving array, or one whose mount state could not be
    /// read, is a refusal no acknowledgement can lift.
    #[test]
    fn a_dissolved_arrays_journal_claim_is_reported_and_a_serving_one_refused() {
        let disk = used_disk();
        let spec = crate::tentanas::elastic::tests::create_spec("produkt");
        let journals = vec![journal_entry(spec.clone(), Some(false))];

        let claim = journal_claim_of(&disk, &journals, &spec.owner, &no_orgs()).expect("dziennik rezerwuje ten dysk");
        assert_eq!(claim.claim.name, "produkt");
        assert_eq!(claim.claim.array_id, spec.array_id);
        assert_eq!(claim.claim.array_role, "data");
        assert_eq!(claim.claim.member_count, 2, "jeden data i jeden parity");
        assert_eq!(claim.claim.owner_org_id, "org");
        // The instance that dissolved it is the one asking: its own journal,
        // never "another instance's".
        assert!(!claim.claim.owner_foreign);
        assert_eq!(claim.claim.owner_kind, crate::tentanas::elastic::OWNER_KIND_THIS_INSTANCE);
        let other = tentanas_helper::elastic::ElasticOwner { org_id: "org".into(), addon_id: "inny".into() };
        let foreign = journal_claim_of(&disk, &journals, &other, &no_orgs()).expect("dziennik");
        assert!(foreign.claim.owner_foreign);
        // Same organisation, another instance: nameable to this tenant.
        assert_eq!(foreign.claim.owner_kind, crate::tentanas::elastic::OWNER_KIND_THIS_ORG);
        let tenant = tentanas_helper::elastic::ElasticOwner { org_id: "inna-org".into(), addon_id: spec.owner.addon_id.clone() };
        let other_tenant = journal_claim_of(&disk, &journals, &tenant, &no_orgs()).expect("dziennik");
        assert!(other_tenant.claim.owner_foreign);
        assert_eq!(other_tenant.claim.owner_kind, crate::tentanas::elastic::OWNER_KIND_OTHER_INSTALLATION);
        // And another tenant's ids stay on the node — not even a tooltip.
        assert_eq!(
            (other_tenant.claim.owner_org_id.as_str(), other_tenant.claim.owner_addon_id.as_str()),
            ("", "")
        );
        assert_eq!(foreign.claim.owner_addon_id, spec.owner.addon_id, "own organisation: kept");
        let plan = plan_wipe(&disk, Some(claim));
        assert!(plan.allowed, "{:?}", plan.refusals);
        let reported = plan.journal_claim.as_ref().expect("rezerwacja w planie");
        assert_eq!(reported.name, "produkt");

        // Serving: the array is publishing its union right now.
        let serving = journal_claim_of(&disk, &[journal_entry(spec.clone(), Some(true))], &spec.owner, &no_orgs())
            .expect("dziennik");
        let plan = plan_wipe(&disk, Some(serving));
        assert!(!plan.allowed);
        assert_eq!(plan.refusals[0].code, "journal_serving");
        assert!(plan.refusals[0].detail.contains("produkt"));
        assert!(plan.journal_claim.is_none(), "nie ma czego potwierdzać");

        // Unknown mount state. THIS is the incident: unreadable is not free.
        let unknown =
            journal_claim_of(&disk, &[journal_entry(spec.clone(), None)], &spec.owner, &no_orgs()).expect("dziennik");
        let plan = plan_wipe(&disk, Some(unknown));
        assert!(!plan.allowed);
        assert_eq!(plan.refusals[0].code, "journal_unknown");
        assert!(plan.refusals[0].detail.contains("nieznany stan nie jest stanem wolnym"));
        assert!(plan.journal_claim.is_none());
    }

    /// A journal claims a disk by IDENTITY. The device name is not an
    /// identity: a kernel rename between two boots is how a wipe reaches the
    /// disk nobody meant.
    #[test]
    fn a_journal_claim_is_matched_by_identity_and_never_by_device_name() {
        let spec = crate::tentanas::elastic::tests::create_spec("produkt");
        let journals = vec![journal_entry(spec.clone(), Some(false))];

        // Renamed device, same serial: still claimed.
        let renamed = NasDisk {
            name: "sdz".to_string(),
            path: "/dev/sdz".to_string(),
            disk_id: "sn-serial-produkt-data".to_string(),
            wwn: None,
            ..used_disk()
        };
        assert_eq!(
            journal_claim_of(&renamed, &journals, &spec.owner, &no_orgs()).expect("po numerze seryjnym").claim.array_role,
            "data"
        );

        // Same device name, no matching identity: NOT claimed.
        let stranger = NasDisk {
            disk_id: "wwn-obcy".to_string(),
            serial: "serial-obcy".to_string(),
            wwn: Some("wwn-obcy".to_string()),
            ..used_disk()
        };
        assert!(journal_claim_of(&stranger, &journals, &spec.owner, &no_orgs()).is_none());

        // The parity disk of the same array is claimed as parity.
        let parity = NasDisk {
            disk_id: "wwn-produkt-parity".to_string(),
            serial: "serial-produkt-parity".to_string(),
            wwn: Some("wwn-produkt-parity".to_string()),
            ..used_disk()
        };
        assert_eq!(
            journal_claim_of(&parity, &journals, &spec.owner, &no_orgs()).expect("parity").claim.array_role,
            "parity"
        );
    }

    /// The two confirmations, and the one command they unlock.
    #[test]
    fn the_wipe_command_needs_the_typed_name_and_a_named_journal_acknowledgement() {
        let disk = used_disk();
        let plan = plan_wipe(&disk, None);

        // The retype gate. Case, whitespace and a near miss all refuse.
        for typed in ["", "sdb", "SDC", " sdc", "/dev/sdc"] {
            assert!(
                matches!(wipe_command(&plan, &disk, typed, ""), Err(WipeRefusal::BadRequest(_))),
                "'{typed}' nie może przejść"
            );
        }
        let command = wipe_command(&plan, &disk, "sdc", "").expect("przepisana nazwa");
        // The command carries the IDENTITY, not just the path.
        assert_eq!(
            command,
            HelperCommand::DiskWipe {
                device: "/dev/sdc".to_string(),
                wwn: Some("wwn-produkt-data".to_string()),
                serial: Some("serial-produkt-data".to_string()),
                bytes: 32 * 1024 * 1024 * 1024,
                release_journal: None,
            }
        );
        // And it is a real catalog entry, validated as one.
        assert_eq!(command.builtin_label(), Some("disk_wipe"));
        assert!(command.plan().is_ok());

        // A plan with any refusal never produces a command, however carefully
        // the name was typed.
        let refused = plan_wipe(
            &NasDisk { role: "pool_member".to_string(), member_of: Some("tank".into()), ..disk.clone() },
            None,
        );
        assert!(matches!(
            wipe_command(&refused, &disk, "sdc", ""),
            Err(WipeRefusal::NotAvailable(_))
        ));

        // An acknowledgement for a disk no journal claims is a disagreement
        // between the caller and this node, and is refused rather than ignored.
        assert!(matches!(
            wipe_command(&plan, &disk, "sdc", "produkt"),
            Err(WipeRefusal::BadRequest(_))
        ));

        // A plan whose disk is no longer the one in the inventory refuses.
        let moved = NasDisk { path: "/dev/sdz".to_string(), ..disk.clone() };
        assert!(matches!(
            wipe_command(&plan, &moved, "sdc", ""),
            Err(WipeRefusal::BadRequest(_))
        ));
    }

    /// The journal acknowledgement is a SECOND gate, and it names the array.
    #[test]
    fn releasing_a_journal_claim_takes_more_than_the_typed_device_name() {
        let disk = used_disk();
        let spec = crate::tentanas::elastic::tests::create_spec("produkt");
        let claim = journal_claim_of(&disk, &[journal_entry(spec.clone(), Some(false))], &spec.owner, &no_orgs())
            .expect("dziennik");
        let plan = plan_wipe(&disk, Some(claim));

        // The retyped device name alone does NOT release the array.
        let refusal = wipe_command(&plan, &disk, "sdc", "").expect_err("brak potwierdzenia");
        assert!(matches!(&refusal, WipeRefusal::NotAvailable(d) if d.contains("produkt")));
        // Neither does an acknowledgement naming a different array.
        assert!(matches!(
            wipe_command(&plan, &disk, "sdc", "foto"),
            Err(WipeRefusal::NotAvailable(_))
        ));
        // Nor a truthy-looking value that is not the array's name: the
        // acknowledgement cannot be satisfied by a client that never showed it.
        for ack in ["true", "1", "yes", "PRODUKT"] {
            assert!(
                matches!(wipe_command(&plan, &disk, "sdc", ack), Err(WipeRefusal::NotAvailable(_))),
                "'{ack}' nie jest potwierdzeniem"
            );
        }
        let command = wipe_command(&plan, &disk, "sdc", "produkt").expect("potwierdzone");
        assert_eq!(
            command,
            HelperCommand::DiskWipe {
                device: "/dev/sdc".to_string(),
                wwn: Some("wwn-produkt-data".to_string()),
                serial: Some("serial-produkt-data".to_string()),
                bytes: 32 * 1024 * 1024 * 1024,
                // The array_id, so the privileged side drops the journal the
                // admin acknowledged and not whichever one claims the disk.
                release_journal: Some(spec.array_id.clone()),
            }
        );
        assert!(command.plan().is_ok(), "uuid dziennika przechodzi walidację katalogu");
    }

    /// A disk with neither a WWN nor a serial cannot be re-identified after a
    /// rename, and re-identification is the wipe's only protection against
    /// reaching the wrong device. The catalog refuses such a command, so the
    /// refusal happens before any channel is chosen.
    #[test]
    fn a_disk_without_any_identity_is_refused_by_the_catalog() {
        let disk = NasDisk { wwn: None, serial: String::new(), ..used_disk() };
        let plan = plan_wipe(&disk, None);
        let command = wipe_command(&plan, &disk, "sdc", "").expect("plan bez odmów");
        assert_eq!(
            command,
            HelperCommand::DiskWipe {
                device: "/dev/sdc".to_string(),
                wwn: None,
                serial: None,
                bytes: 32 * 1024 * 1024 * 1024,
                release_journal: None,
            }
        );
        assert!(command.plan().is_err(), "brak tożsamości nie może dojść do urządzenia");
    }

    /// The label travels with the signature it belongs to, and only with it.
    /// A `zfs_member` or `linux_raid_member` label IS the pool or array name
    /// and belongs in `member_of`; carrying it here too would name the wrong
    /// thing in the wipe plan, which is the one place it is read.
    #[test]
    fn the_filesystem_label_is_read_with_its_own_signature_and_nowhere_else() {
        let disks = disks_from_lsblk_json(LSBLK).unwrap();
        let sdc = disks.iter().find(|d| d.name == "sdc").expect("sdc");
        assert_eq!(sdc.role, "used");
        assert_eq!(sdc.fs_type.as_deref(), Some("ext4"));
        assert_eq!(sdc.fs_label.as_deref(), Some("old-data"));
        // The pool member's label named the pool, so it went to `member_of`.
        let sda = disks.iter().find(|d| d.name == "sda").expect("sda");
        assert_eq!(sda.member_of.as_deref(), Some("tank"));
        assert!(sda.fs_label.is_none(), "etykieta puli nie jest etykietą systemu plików");
        assert!(sda.fs_type.is_none());
        // And a recorded Elastic member loses both: the branch filesystem is
        // what the array built, not a foreign occupier.
        let mut member = sdc.clone();
        let mut arrays = HashMap::new();
        arrays.insert(
            member.disk_id.clone(),
            ("produkt".to_string(), "data".to_string()),
        );
        apply_array_membership(&mut member, &arrays);
        assert_eq!(member.role, "array_member");
        assert!(member.fs_type.is_none() && member.fs_label.is_none());
    }

    #[test]
    fn lsblk_inventory_classifies_disks() {
        let disks = disks_from_lsblk_json(LSBLK).unwrap();
        assert_eq!(disks.len(), 4);
        let sda = &disks[0];
        assert_eq!(sda.disk_id, "wwn-5000cca");
        assert_eq!(sda.kind, "hdd");
        assert_eq!(sda.role, "pool_member");
        assert_eq!(sda.member_of.as_deref(), Some("tank"));
        assert_eq!(sda.model, "WDC WD80EFZZ");
        let nvme = &disks[1];
        assert_eq!(nvme.kind, "nvme");
        assert_eq!(nvme.role, "system");
        assert_eq!(nvme.disk_id, "sn-S1");
        assert_eq!(nvme.mountpoints, vec!["/"]);
        let sdb = &disks[2];
        assert_eq!(sdb.size_bytes, 4000787030016);
        assert!(sdb.rotational);
        assert_eq!(sdb.role, "free");
    }

    /// `used` is the role every disk falls back to that is not system, not in a
    /// pool or array and not mounted, yet carries a filesystem signature. On the
    /// machine this was measured on it covers 23 of 29 disks, so on its own it
    /// tells the reader nothing they can act on — it has to name the occupier.
    #[test]
    fn a_disk_with_only_a_filesystem_signature_names_it() {
        let disks = disks_from_lsblk_json(LSBLK).unwrap();
        let sdc = disks.iter().find(|d| d.name == "sdc").expect("sdc is in the fixture");
        assert_eq!(sdc.role, "used");
        assert_eq!(sdc.fs_type.as_deref(), Some("ext4"));
        // Nothing owns it, so no membership may be claimed.
        assert_eq!(sdc.member_of, None);
    }

    /// An lsblk document of a node whose Elastic Array `produkt` owns three
    /// disks. This is what the union really looks like from below: every
    /// branch carries its OWN xfs filesystem and nothing that names the array
    /// — no `linux_raid_member`, no `zfs_member` — which is exactly why
    /// `role_of` alone cannot classify them. Deliberately separate from
    /// `LSBLK`, whose `disks.len()` and indexes other tests depend on.
    const LSBLK_ARRAY: &str = r#"{"blockdevices":[
      {"name":"sdg","path":"/dev/sdg","type":"disk","model":"WD80EFZZ","serial":"D1","wwn":null,"size":8001563222016,"tran":"sata","rota":true,"rm":false,"rev":null,"vendor":"ATA","mountpoints":[null],"fstype":"xfs","label":null},
      {"name":"sdh","path":"/dev/sdh","type":"disk","model":"WD80EFZZ","serial":"P1","wwn":null,"size":8001563222016,"tran":"sata","rota":true,"rm":false,"rev":null,"vendor":"ATA","mountpoints":[null],"fstype":"xfs","label":null},
      {"name":"nvme1n1","path":"/dev/nvme1n1","type":"disk","model":"980 PRO","serial":"C1","wwn":null,"size":1000204886016,"tran":"nvme","rota":false,"rm":false,"rev":null,"vendor":null,"mountpoints":[null],"fstype":"xfs","label":null},
      {"name":"sdi","path":"/dev/sdi","type":"disk","model":"ST4000","serial":"F1","wwn":null,"size":4000787030016,"tran":"sata","rota":true,"rm":false,"rev":null,"vendor":"ATA","mountpoints":[null],"fstype":null,"label":null},
      {"name":"sdj","path":"/dev/sdj","type":"disk","model":"WD80EFZZ","serial":"Z1","wwn":null,"size":8001563222016,"tran":"sata","rota":true,"rm":false,"rev":null,"vendor":"ATA","mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"sdj1","path":"/dev/sdj1","type":"part","mountpoints":[null],"fstype":"zfs_member","label":"tank"}]},
      {"name":"nvme0n1","path":"/dev/nvme0n1","type":"disk","model":"Samsung 980","serial":"S1","wwn":null,"size":1000204886016,"tran":"nvme","rota":false,"rm":false,"rev":null,"vendor":null,"mountpoints":[null],"fstype":null,"label":null,
       "children":[{"name":"nvme0n1p2","path":"/dev/nvme0n1p2","type":"part","mountpoints":["/"],"fstype":"ext4","label":null}]}
    ]}"#;

    fn array_index(pairs: &[(&str, &str, &str)]) -> HashMap<String, (String, String)> {
        pairs
            .iter()
            .map(|(id, array, role)| {
                ((*id).to_string(), ((*array).to_string(), (*role).to_string()))
            })
            .collect()
    }

    fn by_name(disks: &[NasDisk], name: &str) -> NasDisk {
        disks
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name} is in the fixture"))
            .clone()
    }

    /// The defect: an Elastic Array leaves no signature on its disks, so every
    /// branch fell through `role_of` to the catch-all `used` and the chip read
    /// "Zajęty · xfs" — 23 times on the node this was measured on. Membership
    /// lives in this node's database, so it is applied after the scan, and
    /// each part of the array says which part it is: the admin's next move
    /// differs between data, cache (unprotected bytes) and parity (no data at
    /// all).
    #[test]
    fn an_elastic_array_member_is_named_after_its_array_not_its_filesystem() {
        let mut disks = disks_from_lsblk_json(LSBLK_ARRAY).unwrap();
        // Before membership is applied the three branches are exactly the
        // useless rows the owner complained about.
        for name in ["sdg", "sdh", "nvme1n1"] {
            let raw = by_name(&disks, name);
            assert_eq!(raw.role, "used", "{name} before membership");
            assert_eq!(raw.fs_type.as_deref(), Some("xfs"), "{name} before membership");
        }
        let arrays = array_index(&[
            ("sn-D1", "produkt", "data"),
            ("sn-C1", "produkt", "cache"),
            ("sn-P1", "produkt", "parity"),
        ]);
        apply_membership(&mut disks, &HashMap::new(), &arrays);
        for (name, role) in [("sdg", "data"), ("nvme1n1", "cache"), ("sdh", "parity")] {
            let d = by_name(&disks, name);
            assert_eq!(d.role, "array_member", "{name}");
            assert_eq!(d.member_of.as_deref(), Some("produkt"), "{name}");
            assert_eq!(d.array_role, role, "{name}");
            // The branch filesystem is the array's own doing; leaving it would
            // put "· xfs" back on a row that now has an owner to show.
            assert_eq!(d.fs_type, None, "{name}");
        }
    }

    /// The array rows are this node's record, not evidence on the disk, so
    /// they may NOT overrule what the disk itself says. A stale row naming the
    /// system disk or a ZFS member must leave both exactly as the scan found
    /// them, and a disk no array claims stays free.
    #[test]
    fn array_rows_never_overrule_the_system_disk_a_pool_member_or_a_free_disk() {
        let mut disks = disks_from_lsblk_json(LSBLK_ARRAY).unwrap();
        let arrays = array_index(&[
            // Both of these are wrong on purpose: the row outlived the array.
            ("sn-S1", "produkt", "data"),
            ("sn-Z1", "produkt", "data"),
        ]);
        let vdevs: HashMap<String, (String, String)> =
            [("sdj".to_string(), ("data".to_string(), "raidz3".to_string()))].into();
        apply_membership(&mut disks, &vdevs, &arrays);

        let system = by_name(&disks, "nvme0n1");
        assert_eq!(system.role, "system");
        assert_eq!(system.member_of, None);
        assert!(system.array_role.is_empty());

        // "tank · RAIDZ3" on the node this was measured on: the pool, its vdev
        // role and its layout, none of them touched.
        let member = by_name(&disks, "sdj");
        assert_eq!(member.role, "pool_member");
        assert_eq!(member.member_of.as_deref(), Some("tank"));
        assert_eq!((member.vdev_role.as_str(), member.vdev_kind.as_str()), ("data", "raidz3"));
        assert!(member.array_role.is_empty());

        let free = by_name(&disks, "sdi");
        assert_eq!(free.role, "free");
        assert_eq!(free.member_of, None);
        assert!(free.array_role.is_empty());
        assert_eq!(free.fs_type, None);
    }

    /// A disk carrying a filesystem that belongs to NOTHING keeps the `used`
    /// role and its signature: that is the one row where naming the
    /// filesystem is all the inventory can honestly say. Applying array
    /// membership must not blank it.
    #[test]
    fn a_disk_no_array_claims_keeps_used_and_its_filesystem() {
        let mut disks = disks_from_lsblk_json(LSBLK_ARRAY).unwrap();
        apply_membership(&mut disks, &HashMap::new(), &array_index(&[("sn-D1", "produkt", "data")]));
        let orphan = by_name(&disks, "sdh");
        assert_eq!(orphan.role, "used");
        assert_eq!(orphan.fs_type.as_deref(), Some("xfs"));
        assert_eq!(orphan.member_of, None);
        assert!(orphan.array_role.is_empty());
    }

    /// The lookup itself, against a real database: an array created through
    /// the production intent must yield every one of its disks, keyed by the
    /// stable `disk_id` the branch rows carry — never by kernel name, which
    /// the rows do not even store (`BranchRow::name` is the slot, `d1`/`c1`).
    #[test]
    fn array_membership_reads_every_disk_of_every_recorded_array() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));

        let mut spec = crate::tentanas::elastic::tests::create_spec("produkt");
        spec.cache = Some(tentanas_helper::elastic::ElasticDiskSpec {
            disk_id: "produkt-cache".to_string(),
            wwn: Some("wwn-produkt-cache".to_string()),
            serial: Some("serial-produkt-cache".to_string()),
            bytes: 32 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        });
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::new_v4().to_string(),
            kind: "elastic_create".to_string(),
            subject: spec.name.clone(),
            status: "running".to_string(),
            started_by: "test".to_string(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&db, &job, Some(&crate::tentanas::jobs::ElasticJobIntent::Create(spec)))
            .expect("record the array");

        let index = array_membership(&db);
        let mut rows: Vec<(String, String, String)> = index
            .into_iter()
            .map(|(id, (array, role))| (id, array, role))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("produkt-cache".to_string(), "produkt".to_string(), "cache".to_string()),
                ("produkt-data".to_string(), "produkt".to_string(), "data".to_string()),
                ("produkt-parity".to_string(), "produkt".to_string(), "parity".to_string()),
            ]
        );
    }

    /// A ZFS member label belongs in `member_of` and must never reach
    /// `fs_type`: the two are rendered in different places, and a leak would
    /// print a pool name where a filesystem is expected.
    #[test]
    fn a_pool_member_reports_no_filesystem_signature() {
        let disks = disks_from_lsblk_json(LSBLK).unwrap();
        let sda = disks.iter().find(|d| d.name == "sda").expect("sda is in the fixture");
        assert_eq!(sda.role, "pool_member");
        assert_eq!(sda.member_of.as_deref(), Some("tank"));
        assert_eq!(sda.fs_type, None);
    }

    /// NVMe self-test results are not a pass/fail pair. Collapsing everything
    /// above 1 into "failed" made a disk whose test was merely interrupted —
    /// or an empty log row — read as a failed self-test. Same defect class as
    /// the ATA and SPC arms, left behind for NVMe.
    ///
    /// EVERY code of every range appears here, not a representative of it, and
    /// the fallback arm is pinned too. A mutation run walked straight through
    /// the earlier vector: dropping `| 8` from the aborted arm, narrowing
    /// `1..=4` to `1 | 2 | 4` and turning `_ => "unknown"` into
    /// `_ => "failed"` all left the suite green, because 3, 8, a reserved code
    /// and an entry with no readable result were never in the table.
    #[test]
    fn nvme_self_test_codes_separate_abort_from_failure() {
        let doc: Value = serde_json::json!({
            "nvme_self_test_log": {"table": [
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 0, "string": "ok"}, "power_on_hours": 10},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 1, "string": "aborted by command"}, "power_on_hours": 11},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 2, "string": "aborted by reset"}, "power_on_hours": 12},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 3, "string": "aborted by namespace removal"}, "power_on_hours": 13},
                {"self_test_code": {"string": "Extended"}, "self_test_result": {"value": 4, "string": "aborted by format"}, "power_on_hours": 14},
                {"self_test_code": {"string": "Extended"}, "self_test_result": {"value": 5, "string": "fatal error"}, "power_on_hours": 15},
                {"self_test_code": {"string": "Extended"}, "self_test_result": {"value": 6, "string": "unknown segment failed"}, "power_on_hours": 16},
                {"self_test_code": {"string": "Extended"}, "self_test_result": {"value": 7, "string": "segment failed"}, "power_on_hours": 17},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 8, "string": "aborted for an unknown reason"}, "power_on_hours": 18},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 9, "string": "aborted by sanitize"}, "power_on_hours": 19},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 11, "string": "reserved"}, "power_on_hours": 20},
                {"self_test_code": {"string": "Short"}, "power_on_hours": 21},
                {"self_test_code": {"string": "Short"}, "self_test_result": {"value": 15, "string": "entry not used"}, "power_on_hours": 22},
                null
            ]}
        });
        let tests = smart_self_tests(&doc);
        assert_eq!(
            tests.len(),
            12,
            "the `entry not used` row and the unset slot are dropped, not reported: {tests:?}"
        );
        let statuses: Vec<&str> = tests.iter().map(|t| t.status.as_str()).collect();
        assert_eq!(
            statuses,
            vec![
                "passed",                                     // 0
                "aborted", "aborted", "aborted", "aborted",   // 1, 2, 3, 4 — both ends
                "failed", "failed", "failed",                 // 5, 6, 7 — both ends
                "aborted", "aborted",                         // 8, 9 — both ends
                "unknown", "unknown",                         // reserved Bh, and no result field
            ]
        );
        // The dropped rows are the LAST two of the table, so a phantom row
        // would land at the end with nothing of its own in it.
        assert!(
            tests.iter().all(|t| t.lifetime_hours != 0 && !t.kind.is_empty()),
            "every reported row came from a real entry: {tests:?}"
        );
    }

    /// The newest entry that carries a VERDICT is the disk's last verdict, and
    /// "unknown" is not one. A reserved NVMe code, an entry whose result field
    /// cannot be read, an unset (null) log slot and — pre-existing on SCSI —
    /// an SPC code 3 or a reserved 8–14 all normalise to "unknown", and any of
    /// them on top used to be taken as THE verdict: the detail view showed a
    /// red `failed` row while `self_test_failed` stayed false and
    /// `score_health` called the disk "ok".
    #[test]
    fn a_non_verdict_entry_does_not_hide_the_failure_under_it() {
        let nvme_entry = |value: u64| {
            serde_json::json!({
                "self_test_code": {"string": "Extended"},
                "self_test_result": {"value": value, "string": "x"},
                "power_on_hours": 5,
            })
        };
        let nvme = |table: Value| {
            summarize_smart(&serde_json::json!({
                "smart_status": {"passed": true},
                "nvme_self_test_log": {"table": table},
            }))
        };

        // A reserved code (Ch, in the Ah–Eh range) on top of a failed segment.
        let reserved = nvme(serde_json::json!([nvme_entry(12), nvme_entry(6)]));
        assert!(reserved.self_test_failed, "a reserved code is not a verdict");
        assert_eq!(
            scored(&reserved, "ssd", None),
            ("critical", "last self-test failed".to_string())
        );

        // An entry with no readable `self_test_result.value` at all.
        let unreadable = nvme(serde_json::json!([
            {"self_test_code": {"string": "Extended"}, "power_on_hours": 6},
            nvme_entry(6),
        ]));
        assert!(unreadable.self_test_failed, "an unreadable result is not a verdict");

        // An unset log slot: smartctl writes the table at the RAW log index,
        // so the array is sparse and the gaps arrive as literal nulls.
        let sparse = nvme(serde_json::json!([Value::Null, nvme_entry(6)]));
        assert!(sparse.self_test_failed, "a null slot is not even an entry");

        // A real verdict on top still wins — the newest VERDICT is the verdict.
        let cleared = nvme(serde_json::json!([
            nvme_entry(12),
            nvme_entry(0),
            nvme_entry(6),
        ]));
        assert!(!cleared.self_test_failed, "the newest verdict is a pass");
        assert_eq!(scored(&cleared, "ssd", None).0, "ok");

        // SCSI: the same shape, newest first as numbered top-level keys.
        let scsi_entry = |value: u64| {
            serde_json::json!({
                "code": {"string": "Background long"},
                "result": {"value": value, "string": "x"},
                "power_on_time": {"hours": 100},
            })
        };
        let scsi = |newest: u64, older: u64| {
            summarize_smart(&serde_json::json!({
                "smart_status": {"passed": true},
                "scsi_grown_defect_list": 0,
                "scsi_self_test_0": scsi_entry(newest),
                "scsi_self_test_1": scsi_entry(older),
            }))
        };
        let incomplete = scsi(3, 5);
        assert!(
            incomplete.self_test_failed,
            "SPC 3 (unknown error, incomplete) is not a verdict"
        );
        assert_eq!(scored(&incomplete, "hdd", None).0, "critical");
        assert!(scsi(8, 5).self_test_failed, "an SPC reserved code is not a verdict");
        assert!(!scsi(0, 5).self_test_failed, "a pass on top clears it");
    }

    /// A failed NVMe self-test has to reach `score_health`, and an aborted one
    /// must not. Before this the NVMe verdict reached nothing at all, so a
    /// disk that reported a failed segment still read as healthy.
    ///
    /// This test used to assert only the single-entry cases below, which read
    /// the same under both abort contracts — an abort on its own has no verdict
    /// beneath it to clear. That is precisely how the defect survived: an abort
    /// counted as A VERDICT, so an abort STACKED on a failure cleared it. The
    /// stacked cases are now asserted here too.
    #[test]
    fn a_failed_nvme_self_test_counts_but_an_aborted_one_does_not() {
        let doc = |result: u64| {
            serde_json::json!({
                "nvme_self_test_log": {"table": [
                    {"self_test_code": {"string": "Extended"},
                     "self_test_result": {"value": result, "string": "x"},
                     "power_on_hours": 5}
                ]}
            })
        };
        assert!(
            summarize_smart(&doc(6)).self_test_failed,
            "result 6 is a failed segment and must reach health"
        );
        assert!(
            !summarize_smart(&doc(2)).self_test_failed,
            "result 2 is a controller reset, not a verdict on the medium"
        );
        assert!(
            !summarize_smart(&doc(9)).self_test_failed,
            "result 9 is a sanitize abort, not a failure"
        );
        assert!(!summarize_smart(&doc(0)).self_test_failed, "result 0 passed");
    }

    /// An abort is not a statement about the medium, so it is not a verdict:
    /// it must neither raise a failure nor CLEAR one recorded beneath it. It
    /// used to clear one — "aborted" sat in the same list as "passed" and
    /// "failed" — so a controller reset (NVMe result 2) or a sanitize (result
    /// 9) silently turned a disk that had failed its self-test green again,
    /// and the alert was never raised a second time. Only a real verdict moves
    /// the answer, and the way to clear a stale failure is a test that PASSES.
    #[test]
    fn an_abort_does_not_clear_a_failure_recorded_beneath_it() {
        let nvme_entry = |value: u64| {
            serde_json::json!({
                "self_test_code": {"string": "Extended"},
                "self_test_result": {"value": value, "string": "x"},
                "power_on_hours": 5,
            })
        };
        let nvme = |table: Value| {
            summarize_smart(&serde_json::json!({
                "smart_status": {"passed": true},
                "nvme_self_test_log": {"table": table},
            }))
        };

        // A controller reset on top of a failed segment: the failure stands.
        let reset = nvme(serde_json::json!([nvme_entry(2), nvme_entry(6)]));
        assert!(reset.self_test_failed, "a controller reset is not a verdict");
        assert_eq!(scored(&reset, "ssd", None).0, "critical");

        // A sanitize abort on top of it, likewise.
        let sanitize = nvme(serde_json::json!([nvme_entry(9), nvme_entry(6)]));
        assert!(sanitize.self_test_failed, "a sanitize abort is not a verdict");

        // Aborts stacked on top of a PASS leave the pass standing, so an abort
        // never invents a failure either.
        let clean = nvme(serde_json::json!([nvme_entry(2), nvme_entry(9), nvme_entry(0)]));
        assert!(!clean.self_test_failed, "the newest verdict is still the pass");
        assert_eq!(scored(&clean, "ssd", None).0, "ok");

        // A fresh PASS above the abort is what clears the failure.
        let retested = nvme(serde_json::json!([
            nvme_entry(0),
            nvme_entry(2),
            nvme_entry(6)
        ]));
        assert!(!retested.self_test_failed, "a passing re-test clears it");

        // SCSI: the same contract. SPC 1 is the user's own abort command and
        // SPC 2 a device reset, newest first as numbered top-level keys.
        let scsi_entry = |value: u64| {
            serde_json::json!({
                "code": {"string": "Background long"},
                "result": {"value": value, "string": "x"},
                "power_on_time": {"hours": 100},
            })
        };
        let scsi = |newest: u64, older: u64| {
            summarize_smart(&serde_json::json!({
                "smart_status": {"passed": true},
                "scsi_grown_defect_list": 0,
                "scsi_self_test_0": scsi_entry(newest),
                "scsi_self_test_1": scsi_entry(older),
            }))
        };
        assert!(scsi(1, 5).self_test_failed, "an abort by command is not a verdict");
        assert!(scsi(2, 5).self_test_failed, "an abort by device reset is not a verdict");
        assert!(!scsi(1, 0).self_test_failed, "an abort over a pass invents nothing");
        assert!(!scsi(0, 5).self_test_failed, "a passing re-test still clears it");
    }

    /// A real NVMe document with no self-test history at all. The health guard
    /// fires on `nvme_self_test_log` being present, so this is the path every
    /// NVMe disk on a fresh node takes: present log, no table, therefore no
    /// verdict — and it must not read as a failure or panic.
    #[test]
    fn a_real_nvme_document_without_a_self_test_table_yields_no_verdict() {
        let doc: Value = serde_json::from_str(SMART_NVME).expect("fixture parses");
        assert!(
            doc.get("nvme_self_test_log").is_some(),
            "the guard's trigger is present in the real document"
        );
        assert!(
            doc.pointer("/nvme_self_test_log/table").is_none(),
            "and the real document carries no table, not merely an empty one"
        );
        assert!(smart_self_tests(&doc).is_empty(), "no entries to report");
        let s = summarize_smart(&doc);
        assert!(!s.self_test_failed, "no verdict is not a failure");
        // The counters this area reads, straight off the device.
        assert_eq!(s.media_errors, Some(835));
        assert_eq!(s.wear_pct, Some(9));
    }

    #[test]
    fn diskstats_rates() {
        let a = parse_diskstats("   8       0 sda 100 0 2048 50 10 0 1024 20 0 500 70\n");
        let b = parse_diskstats("   8       0 sda 200 0 4096 150 20 0 2048 40 0 1500 190\n");
        let io = io_rates(&a["sda"], &b["sda"], Duration::from_secs(1));
        assert_eq!(io.read_bps, 2048 * 512);
        assert_eq!(io.write_bps, 1024 * 512);
        assert_eq!(io.read_iops, 100.0);
        assert!((io.await_ms - 120.0 / 110.0).abs() < 0.01);
        assert!((io.util_pct - 100.0).abs() < 0.01);
    }

    #[test]
    fn smart_summary_and_health_for_ata() {
        let doc: Value = serde_json::json!({
            "smart_status": {"passed": true},
            "temperature": {"current": 41},
            "power_on_time": {"hours": 12345},
            "firmware_version": "81.00A81",
            "ata_smart_attributes": {"table": [
                {"id": 5, "name": "Reallocated_Sector_Ct", "value": 100, "worst": 100, "thresh": 10, "when_failed": "", "raw": {"value": 8, "string": "8"}},
                {"id": 197, "name": "Current_Pending_Sector", "value": 100, "worst": 100, "thresh": 0, "when_failed": "", "raw": {"value": 0, "string": "0"}},
                {"id": 199, "name": "UDMA_CRC_Error_Count", "value": 200, "worst": 200, "thresh": 0, "when_failed": "", "raw": {"value": 3, "string": "3"}}
            ]},
            "ata_smart_self_test_log": {"standard": {"table": [
                {"type": {"string": "Short offline"}, "status": {"string": "Completed without error", "passed": true}, "lifetime_hours": 12000}
            ]}}
        });
        let s = summarize_smart(&doc);
        assert_eq!(s.reallocated, Some(8));
        assert_eq!(s.crc_errors, Some(3));
        assert_eq!(s.temperature_c, Some(41));
        let (h, reason) = scored(&s, "hdd", Some(8));
        assert_eq!(h, "warning");
        assert!(reason.contains("8 reallocated"));
        // Growth is still SAID ("growing"), but graded `warning`: the SPEC's
        // sdd, "+3 realokacje w 7 dni", is "Uwaga", not "Awaria".
        let (h, reason) = scored(&s, "hdd", Some(2));
        assert_eq!(h, "warning");
        assert!(reason.contains("growing"));
        assert_eq!(smart_attributes(&doc).len(), 3);
        let tests = smart_self_tests(&doc);
        assert_eq!(tests[0].status, "passed");
    }

    /// Owner decision 2026-09-21: a reallocated count that GREW in the last
    /// week is "Uwaga", not "Awaria". The SPEC's sdd — three new reallocations
    /// in seven days, 47°C, SMART passed — is `warning` with the growth named.
    #[test]
    fn realloc_growth_grades_warning() {
        let sdd = SmartSummary {
            passed: Some(true),
            temperature_c: Some(47),
            reallocated: Some(3),
            pending: Some(0),
            crc_errors: Some(0),
            ..Default::default()
        };
        let (health, reason) = scored(&sdd, "hdd", Some(0));
        assert_eq!(health, "warning");
        assert_eq!(reason, "reallocated sectors growing (0 → 3 in 7 days)");
        // Unchanged over the week it is still a symptom, just not a moving one.
        assert_eq!(
            scored(&sdd, "hdd", Some(3)),
            ("warning", "3 reallocated sectors".to_string())
        );
    }

    /// A counter is a symptom — "Uwaga" — however high: pending sectors,
    /// media errors, a temperature past the limit, a worn-out SSD. Only the
    /// drive's own verdict (overall FAILED, a failed self-test) is "Awaria",
    /// and it then carries the symptoms in the same sentence.
    #[test]
    fn counter_symptoms_grade_warning_and_a_failing_drive_critical() {
        let worn = SmartSummary {
            passed: Some(true),
            temperature_c: Some(61),
            reallocated: Some(40),
            pending: Some(2),
            media_errors: Some(1),
            crc_errors: Some(5),
            wear_pct: Some(97),
            ..Default::default()
        };
        let (health, reason) = scored(&worn, "hdd", Some(10));
        assert_eq!(health, "warning");
        assert_eq!(
            reason,
            "2 pending sectors; 1 media errors; reallocated sectors growing (10 → 40 in 7 days); \
             61°C (over the 60°C limit); 5 UDMA CRC errors (cable/backplane); 97% worn"
        );
        // The same disk once the drive itself says it is failing.
        let failing = SmartSummary { passed: Some(false), self_test_failed: true, ..worn.clone() };
        let (health, reason) = scored(&failing, "hdd", Some(10));
        assert_eq!(health, "critical");
        assert!(reason.starts_with("SMART overall status FAILED; last self-test failed; 2 pending sectors"), "{reason}");
        // Below the warning limits there is nothing to say.
        let calm = SmartSummary { passed: Some(true), temperature_c: Some(49), wear_pct: Some(84), ..Default::default() };
        assert_eq!(scored(&calm, "hdd", None), ("ok", String::new()));
        assert_eq!(scored(&SmartSummary::default(), "hdd", None).0, "unknown");
    }

    /// "Awaria" is a disk its pool reports FAULTED or UNAVAIL — whatever SMART
    /// says, since the pool no longer reads it. The SMART reason stays behind
    /// the pool's sentence, and no pool name is put into it.
    #[test]
    fn faulted_or_unavail_grades_critical() {
        for (state, sentence) in [
            ("faulted", "ZFS reports this disk FAULTED"),
            ("unavail", "ZFS reports this disk UNAVAIL"),
        ] {
            assert_eq!(
                graded("ok", "", Some(state)),
                ("critical".to_string(), sentence.to_string())
            );
            assert_eq!(
                graded("warning", "3 reallocated sectors", Some(state)),
                ("critical".to_string(), format!("{sentence}; 3 reallocated sectors"))
            );
        }
        // Every other leaf state, and no leaf at all, leaves SMART's verdict:
        // OFFLINE and REMOVED are an admin's act or a pulled disk, not a
        // verdict on the hardware.
        for state in [Some("online"), Some("degraded"), Some("offline"), Some("removed"), None] {
            assert_eq!(
                graded("warning", "47°C", state),
                ("warning".to_string(), "47°C".to_string()),
                "{state:?}"
            );
        }
        // And a pool that still reads the disk does not soften SMART's own
        // verdict either.
        assert_eq!(
            graded("critical", "SMART overall status FAILED", Some("online")),
            ("critical".to_string(), "SMART overall status FAILED".to_string())
        );
    }

    /// The codes and parameters a screen words, as `(code, [(key, value)])`.
    fn codes(reasons: &[NasHealthReason]) -> Vec<(String, Vec<(String, String)>)> {
        reasons
            .iter()
            .map(|r| (r.code.clone(), r.params.clone().into_iter().collect()))
            .collect()
    }

    fn code(c: &str, params: &[(&str, &str)]) -> (String, Vec<(String, String)>) {
        (c.to_string(), params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    /// Every symptom `score_health` can find reaches the wire as a code with
    /// its numbers as parameters — in the same order as the sentence, the
    /// drive's own verdict first — so a screen never has to parse English.
    #[test]
    fn every_smart_symptom_travels_as_a_code_with_its_numbers() {
        let failing = SmartSummary {
            passed: Some(false),
            self_test_failed: true,
            temperature_c: Some(61),
            reallocated: Some(40),
            pending: Some(2),
            media_errors: Some(1),
            crc_errors: Some(5),
            wear_pct: Some(97),
            ..Default::default()
        };
        let (health, reasons) = score_health(&failing, "hdd", Some(10));
        assert_eq!(health, "critical");
        assert_eq!(
            codes(&reasons),
            vec![
                code("smart_failed", &[]),
                code("self_test_failed", &[]),
                code("pending_sectors", &[("count", "2")]),
                code("media_errors", &[("count", "1")]),
                code("reallocated_growing", &[("from", "10"), ("to", "40")]),
                code("temperature_over_limit", &[("celsius", "61"), ("limit", "60")]),
                code("crc_errors", &[("count", "5")]),
                code("wear", &[("pct", "97")]),
            ]
        );
        // The reallocated count without growth, and a temperature over the
        // warning limit only — an SSD, whose limits are 65/75.
        let warm = SmartSummary { passed: Some(true), temperature_c: Some(70), reallocated: Some(3), ..Default::default() };
        let (health, reasons) = score_health(&warm, "ssd", Some(3));
        assert_eq!(health, "warning");
        assert_eq!(
            codes(&reasons),
            vec![code("reallocated", &[("count", "3")]), code("temperature_high", &[("celsius", "70"), ("limit", "65")])]
        );
        assert_eq!(disk_reasons_text(&reasons), "3 reallocated sectors; 70°C");
        // No verdict at all is a code too; a healthy drive has none.
        let (health, reasons) = score_health(&SmartSummary::default(), "hdd", None);
        assert_eq!((health, codes(&reasons)), ("unknown", vec![code("no_smart_data", &[])]));
        assert_eq!(disk_reasons_text(&reasons), "no SMART data");
        let calm = SmartSummary { passed: Some(true), ..Default::default() };
        assert!(score_health(&calm, "hdd", None).1.is_empty());
    }

    /// A pool's verdict leads the codes as it leads the sentence, and SMART's
    /// codes follow unchanged; no parameter names the pool.
    #[test]
    fn a_faulted_or_unavail_leaf_leads_the_codes_and_names_no_pool() {
        let smart = vec![coded_reason("reallocated", &[("count", "3".to_string())])];
        for (state, lead) in [("faulted", "zfs_faulted"), ("unavail", "zfs_unavail")] {
            let g = grade_disk_health("warning", "3 reallocated sectors", &smart, Some(state));
            assert_eq!(g.health, "critical");
            assert_eq!(codes(&g.reasons), vec![code(lead, &[]), code("reallocated", &[("count", "3")])]);
            assert_eq!(g.reason, disk_reasons_text(&g.reasons), "the sentence is the codes' sentence");
            assert!(g.reasons[0].params.is_empty(), "no pool name rides along");
        }
        let g = grade_disk_health("warning", "3 reallocated sectors", &smart, Some("online"));
        assert_eq!((g.health.as_str(), g.reasons), ("warning", smart));
    }

    /// A restart shows the codes the last SMART read MEASURED: `store_smart`
    /// writes them beside the sentence, and the first inventory pass of a new
    /// process (an empty live state) seeds the disk from that row. The
    /// week-old sample moved on since the read — which made the old
    /// re-derivation drop the codes — and changes nothing now.
    #[tokio::test]
    async fn a_restart_keeps_the_reason_codes_the_last_smart_read_stored() {
        let db = leaf_db(true);
        let disk = NasDisk { disk_id: "wwn-leaf".to_string(), name: "sde".to_string(), ..used_disk() };
        store::upsert_disk_seen(
            &db,
            &DiskIdentity {
                disk_id: &disk.disk_id,
                name: &disk.name,
                model: &disk.model,
                serial: &disk.serial,
                wwn: disk.wwn.as_deref(),
                size_bytes: disk.size_bytes,
                kind: &disk.kind,
            },
        )
        .unwrap();
        let measured = vec![
            coded_reason("reallocated", &[("count", "8".to_string())]),
            coded_reason("crc_errors", &[("count", "3".to_string())]),
        ];
        let doc = serde_json::json!({
            "smart_status": {"passed": true},
            "ata_smart_attributes": {"table": [
                {"id": 5, "name": "Reallocated_Sector_Ct", "value": 100, "worst": 100, "thresh": 10, "when_failed": "", "raw": {"value": 8, "string": "8"}},
                {"id": 199, "name": "UDMA_CRC_Error_Count", "value": 200, "worst": 200, "thresh": 0, "when_failed": "", "raw": {"value": 3, "string": "3"}}
            ]}
        });
        store::store_smart(&db, "wwn-leaf", &doc.to_string(), "warning", &measured).unwrap();
        // The week-old sample has moved on: grading the document again
        // would now say "growing", which the read did not.
        let week_ago = (chrono::Utc::now() - chrono::Duration::days(8)).format("%Y-%m-%dT%H:%M:00Z").to_string();
        store::insert_samples(
            &db,
            &[SampleInsert {
                disk_id: "wwn-leaf",
                at: &week_ago,
                temperature_c: None,
                reallocated: Some(2),
                pending: None,
                crc_errors: None,
                media_errors: None,
                read_bps: 0,
                write_bps: 0,
                await_ms: 0.0,
            }],
        )
        .unwrap();

        // The restart: a process with nothing live meets the disk.
        let cell = RwLock::new(State::new());
        let gate = tokio::sync::Mutex::new(());
        let found = disk.clone();
        inventory_pass(&db, &cell, &gate, move || async move {
            Ok::<InventoryRead, anyhow::Error>((vec![found], HashMap::new(), None, HashSet::new()))
        })
        .await
        .unwrap();
        let st = cell.read();
        let live = &st.disks["wwn-leaf"];
        assert_eq!(live.disk.health, "warning");
        assert_eq!(
            codes(&live.disk.health_reasons),
            vec![code("reallocated", &[("count", "8")]), code("crc_errors", &[("count", "3")])]
        );
        assert_eq!(live.disk.health_reason, "8 reallocated sectors; 3 UDMA CRC errors (cable/backplane)");
        assert_eq!(live.smart_reasons, live.disk.health_reasons, "SMART's codes are kept for the regrade");
        assert_eq!(live.disk.reallocated_sectors, Some(8), "the stored document still fills the counters");
    }

    /// A row stored before migration 21 has the sentence and '[]'. The disk
    /// comes back with no codes — the screen shows the translated grade with
    /// the sentence as its tooltip — and a FAULTED regrade on top of it still
    /// leads with its own code.
    #[test]
    fn a_restart_over_a_row_without_codes_shows_no_codes_rather_than_guessed_ones() {
        let mut live = restarted_live("warning", "8 reallocated sectors; 3 UDMA CRC errors (cable/backplane)");
        assert!(live.disk.health_reasons.is_empty(), "{:?}", live.disk.health_reasons);
        assert_eq!(live.disk.health_reason, "8 reallocated sectors; 3 UDMA CRC errors (cable/backplane)");
        assert_eq!(live.disk.health, "warning");
        let regrade = regrade_leaf(&mut live, Some(&leaves("faulted"))).expect("a change");
        assert_eq!(
            regrade.reason,
            "ZFS reports this disk FAULTED; 8 reallocated sectors; 3 UDMA CRC errors (cable/backplane)"
        );
        assert_eq!(codes(&live.disk.health_reasons), vec![code("zfs_faulted", &[])]);
        assert_eq!(regrade.reasons, live.disk.health_reasons, "the alert gets the codes the disk shows");
    }

    // ----- regrading by the ZFS leaf state -----------------------------------------

    /// A disk this process has not seen yet, seeded like `refresh_inventory`
    /// seeds it after a restart: from the persisted SMART verdict.
    fn restarted_live(smart_health: &str, smart_reason: &str) -> Live {
        let disk = NasDisk { disk_id: "wwn-leaf".to_string(), name: "sde".to_string(), ..used_disk() };
        let row = store::DiskRow {
            disk_id: disk.disk_id.clone(),
            first_seen_at: "2026-09-01T00:00:00Z".to_string(),
            last_seen_at: "2026-09-22T00:00:00Z".to_string(),
            smart_json: None,
            smart_read_at: Some("2026-09-22T00:00:00Z".to_string()),
            health: smart_health.to_string(),
            health_reason: smart_reason.to_string(),
            // A row from before migration 21: the sentence, no codes.
            health_reasons: Vec::new(),
        };
        live_from_persisted(disk, Some(&row))
    }

    fn leaves(state: &str) -> HashMap<String, String> {
        HashMap::from([("sde".to_string(), state.to_string())])
    }

    fn leaf_db(with_alerts: bool) -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        if !with_alerts {
            // A write that fails for real, the way a locked or broken database
            // fails it: the statement itself is refused.
            conn.execute_batch("DROP TABLE nas_alerts").expect("drop");
        }
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    fn open_health_alert(db: &DbPool) -> Option<tentaflow_protocol::tentanas::NasAlert> {
        store::alerts_for_subject(db, "disk", "wwn-leaf")
            .unwrap()
            .into_iter()
            .find(|a| a.resolved_at.is_none())
    }

    /// Several inventory passes over one disk: FAULTED raises the grade to
    /// critical, a failed pool read changes nothing (no flap to SMART's grade
    /// and back), a SMART pass in the middle does not lift the pool's
    /// verdict, and once the pool reads the disk again the grade is SMART's
    /// LATEST verdict — kept apart the whole time — not the persisted one.
    #[test]
    fn a_faulted_leaf_holds_through_a_failed_pool_read_and_recovers_to_the_smart_grade() {
        let mut live = restarted_live("warning", "3 reallocated sectors");
        assert_eq!(live.disk.health, "warning", "the restart shows the persisted SMART verdict");

        let faulted = regrade_leaf(&mut live, Some(&leaves("faulted"))).expect("a change");
        assert_eq!(faulted.health, "critical");
        assert_eq!(faulted.reason, "ZFS reports this disk FAULTED; 3 reallocated sectors");
        assert_eq!(faulted.prior_leaf, None);
        assert_eq!(live.disk.health, "critical");

        // `zpool status` failed: the last known leaf state stands.
        assert_eq!(regrade_leaf(&mut live, None), None);
        assert_eq!(live.disk.health, "critical");
        assert_eq!(live.leaf_state.as_deref(), Some("faulted"));
        // The same state again is no change either.
        assert_eq!(regrade_leaf(&mut live, Some(&leaves("faulted"))), None);

        // SMART now finds nothing wrong; the pool still does not read the disk.
        let previous = live.disk.health.clone();
        let (health, reason) = apply_smart_verdict(&mut live, "ok", Vec::new());
        assert_eq!((previous.as_str(), health.as_str()), ("critical", "critical"));
        assert_eq!(reason, "ZFS reports this disk FAULTED");

        // Another failed read, then the pool takes the disk back.
        assert_eq!(regrade_leaf(&mut live, None), None);
        let back = regrade_leaf(&mut live, Some(&leaves("online"))).expect("a change");
        assert_eq!((back.health.as_str(), back.reason.as_str()), ("ok", ""));
        assert_eq!(live.disk.health, "ok", "SMART's latest verdict, not the persisted warning");

        // Out of the pool altogether (replaced): still SMART's grade.
        let gone = regrade_leaf(&mut live, Some(&HashMap::new())).expect("a change");
        assert_eq!((gone.leaf, gone.health.as_str()), (None, "ok"));

        // A state the parser could not read is not a failure.
        let odd = regrade_leaf(&mut live, Some(&leaves("unknown"))).expect("a change");
        assert_eq!(odd.health, "ok");
    }

    /// Across a restart the alert moves from the severity of its OPEN row.
    /// The same severity refreshes that row in place, so its `raised_at` and
    /// acknowledgement — the replacement advice's clock — survive the
    /// restart; a RISEN severity is a new condition with its own timestamp
    /// and no acknowledgement carried over from the milder one.
    #[test]
    fn an_escalated_health_alert_is_raised_anew_and_an_unchanged_one_keeps_its_time() {
        let db = leaf_db(true);
        store::raise_alert(&db, "disk:wwn-leaf:health", "warning", "disk", "wwn-leaf", "Disk sde: warning", "3 reallocated sectors")
            .unwrap();
        let old = open_health_alert(&db).unwrap();
        {
            let conn = db.write().unwrap();
            conn.execute(
                "UPDATE nas_alerts SET raised_at = '2026-09-01T00:00:00Z', acked_at = '2026-09-02T00:00:00Z' WHERE alert_id = ?1",
                rusqlite::params![old.alert_id],
            )
            .unwrap();
        }

        // The restart: the disk is first seen as an ONLINE leaf. Its grade is
        // still the warning, and settling it leaves the row as it was.
        let mut live = restarted_live("warning", "3 reallocated sectors");
        let online = regrade_leaf(&mut live, Some(&leaves("online"))).expect("first sight is a change");
        assert!(settle_leaf_alerts(&db, vec![online]).is_empty());
        let kept = open_health_alert(&db).unwrap();
        assert_eq!(kept.alert_id, old.alert_id);
        assert_eq!(kept.raised_at, "2026-09-01T00:00:00Z");
        assert_eq!(kept.acked_at.as_deref(), Some("2026-09-02T00:00:00Z"));

        // The pool faults the disk: warning → critical.
        let faulted = regrade_leaf(&mut live, Some(&leaves("faulted"))).expect("a change");
        assert!(settle_leaf_alerts(&db, vec![faulted]).is_empty());
        let raised = open_health_alert(&db).unwrap();
        assert_ne!(raised.alert_id, old.alert_id, "a new row for the new condition");
        assert_eq!(raised.severity, "critical");
        assert_ne!(raised.raised_at, "2026-09-01T00:00:00Z");
        assert_eq!(raised.acked_at, None);
        assert_eq!(raised.detail, "ZFS reports this disk FAULTED; 3 reallocated sectors");
        // And as codes: the grade the row was raised at, the pool's verdict
        // as its coded line (the restarted row had no SMART codes).
        assert_eq!(raised.code, "disk_health");
        assert_eq!(raised.params.get("health").map(String::as_str), Some("critical"));
        assert_eq!(codes(&raised.reasons), vec![code("zfs_faulted", &[])]);
        let all = store::alerts_for_subject(&db, "disk", "wwn-leaf").unwrap();
        assert_eq!(all.iter().filter(|a| a.resolved_at.is_none()).count(), 1);
        assert!(all.iter().any(|a| a.alert_id == old.alert_id && a.resolved_at.is_some()));

        // And the same escalation seen by a restarted process (the shown grade
        // is fresh, the open row is the warning) takes the same path.
        let db = leaf_db(true);
        store::raise_alert(&db, "disk:wwn-leaf:health", "warning", "disk", "wwn-leaf", "Disk sde: warning", "3 reallocated sectors")
            .unwrap();
        let old = open_health_alert(&db).unwrap();
        let mut live = restarted_live("warning", "3 reallocated sectors");
        let faulted = regrade_leaf(&mut live, Some(&leaves("faulted"))).expect("a change");
        assert!(settle_leaf_alerts(&db, vec![faulted]).is_empty());
        let raised = open_health_alert(&db).unwrap();
        assert_ne!(raised.alert_id, old.alert_id);
        assert_eq!(raised.severity, "critical");
    }

    /// A failed alert write is not lost: every regrade is attempted (one
    /// failure does not stop the rest), the failed one's leaf state is put
    /// back, and the next pass finds the very same change and writes it.
    #[test]
    fn a_failed_health_alert_write_is_retried_on_the_next_pass() {
        let broken = leaf_db(false);
        let mut live = restarted_live("warning", "3 reallocated sectors");
        let mut other = restarted_live("ok", "");
        other.disk.disk_id = "wwn-other".to_string();
        other.disk.name = "sdf".to_string();

        let states = HashMap::from([
            ("sde".to_string(), "faulted".to_string()),
            ("sdf".to_string(), "unavail".to_string()),
        ]);
        let first = regrade_leaf(&mut live, Some(&states)).expect("a change");
        let second = regrade_leaf(&mut other, Some(&states)).expect("a change");
        let failed = settle_leaf_alerts(&broken, vec![first.clone(), second]);
        let ids: Vec<&str> = failed.iter().map(|(r, _)| r.disk_id.as_str()).collect();
        assert_eq!(ids, ["wwn-leaf", "wwn-other"], "both attempted, both reported");

        roll_back_leaf(&mut live, &first);
        assert_eq!(live.leaf_state, None);
        assert_eq!(live.disk.health, "warning", "the grade its (missing) alert says");

        // The next pass, with the database back: the same change, written.
        let good = leaf_db(true);
        let again = regrade_leaf(&mut live, Some(&states)).expect("the change is found again");
        assert_eq!(again, first);
        assert!(settle_leaf_alerts(&good, vec![again]).is_empty());
        let alert = open_health_alert(&good).expect("the alert is written");
        assert_eq!(alert.severity, "critical");

        // A rollback that arrives after a later pass moved the leaf on leaves
        // that later state alone.
        let stale = LeafRegrade { leaf: Some("online".to_string()), ..first };
        roll_back_leaf(&mut live, &stale);
        assert_eq!(live.leaf_state.as_deref(), Some("faulted"));
        assert_eq!(live.disk.health, "critical");
    }

    /// The previous process raised a critical alert for a FAULTED disk; the
    /// disk has since left every pool and SMART calls it healthy. Its leaf
    /// state is `None` before and after the restart, so no leaf change ever
    /// closes that alert — only the first-sight settle does, once a pass can
    /// read the pools. A failed first-sight write is retried the same way.
    #[test]
    fn a_stale_open_alert_from_before_a_restart_is_settled_on_first_sight() {
        let db = leaf_db(true);
        store::raise_alert(&db, "disk:wwn-leaf:health", "critical", "disk", "wwn-leaf", "Disk sde: critical", "ZFS reports this disk FAULTED")
            .unwrap();
        let mut live = restarted_live("ok", "");
        assert_eq!(live.disk.health, "ok");

        // The pools could not be read: no word on the leaf, nothing settled.
        assert_eq!(regrade_leaf(&mut live, None), None);
        assert!(open_health_alert(&db).is_some());

        let first = regrade_leaf(&mut live, Some(&HashMap::new())).expect("first sight settles");
        assert_eq!(first.prior_leaf, None);
        assert_eq!(first.leaf, None);
        assert_eq!(first.health, "ok");
        assert!(settle_leaf_alerts(&db, vec![first]).is_empty());
        assert!(open_health_alert(&db).is_none(), "the stale alert is closed");
        // Settled once: the same picture on the next pass writes nothing.
        assert_eq!(regrade_leaf(&mut live, Some(&HashMap::new())), None);

        // A first-sight write that fails has no leaf change to be found by
        // again; the rollback leaves the disk unsettled instead.
        let broken = leaf_db(false);
        let mut live = restarted_live("ok", "");
        let first = regrade_leaf(&mut live, Some(&HashMap::new())).expect("first sight settles");
        assert_eq!(settle_leaf_alerts(&broken, vec![first.clone()]).len(), 1);
        roll_back_leaf(&mut live, &first);
        assert_eq!(regrade_leaf(&mut live, Some(&HashMap::new())), Some(first));
    }

    /// The disk of a pool, as lsblk reports it whether or not the pool is
    /// imported: its `zfs_member` label names the pool.
    fn tank_member() -> NasDisk {
        NasDisk {
            disk_id: "wwn-leaf".to_string(),
            name: "sde".to_string(),
            role: "pool_member".to_string(),
            member_of: Some("tank".to_string()),
            ..used_disk()
        }
    }

    /// One inventory read: the disk, the leaf states (`None` = pools
    /// unreadable) and the pools `zpool list` names.
    fn tank_read(
        leaf: Option<&'static str>,
        pools: &'static [&'static str],
    ) -> impl FnOnce() -> std::future::Ready<Result<InventoryRead>> {
        move || {
            let states = match leaf {
                Some(state) => leaves(state),
                None => HashMap::new(),
            };
            std::future::ready(Ok((
                vec![tank_member()],
                HashMap::new(),
                Some(states),
                pools.iter().map(|p| p.to_string()).collect(),
            )))
        }
    }

    fn seen_ok(db: &DbPool) {
        let disk = tank_member();
        store::upsert_disk_seen(
            db,
            &DiskIdentity {
                disk_id: &disk.disk_id,
                name: &disk.name,
                model: &disk.model,
                serial: &disk.serial,
                wwn: disk.wwn.as_deref(),
                size_bytes: disk.size_bytes,
                kind: &disk.kind,
            },
        )
        .unwrap();
        store::store_smart(db, "wwn-leaf", "{}", "ok", &[]).unwrap();
    }

    /// C1 (wave 6): after a boot this process can read `zpool list` before
    /// the node has imported its pools. The read succeeds and names no pool,
    /// so a FAULTED disk read as "no leaf" and the alert the previous process
    /// left open was closed — then raised again, as a NEW alert, when the pool
    /// imported. The disk's label says which pool it belongs to; a pool on the
    /// disk and not in `zpool list` is not imported YET, and the alert waits
    /// for it: neither the inventory pass nor a SMART read in between closes
    /// it, and the import refreshes the SAME row.
    #[tokio::test]
    async fn a_faulted_alert_waits_for_its_pool_to_be_imported_after_a_boot() {
        let db = leaf_db(true);
        seen_ok(&db);
        store::raise_alert(&db, "disk:wwn-leaf:health", "critical", "disk", "wwn-leaf", "Disk sde: critical", "ZFS reports this disk FAULTED")
            .unwrap();
        let before = open_health_alert(&db).expect("the previous process left it open");
        let cell = RwLock::new(State::new());
        let gate = tokio::sync::Mutex::new(());

        // The boot: `zpool list` answers, and no pool is imported yet.
        inventory_pass(&db, &cell, &gate, tank_read(None, &[])).await.unwrap();
        assert!(open_health_alert(&db).is_some(), "closed before the pool was imported");
        assert!(cell.read().disks["wwn-leaf"].awaiting_import);

        // A SMART read before the import grades without a leaf state: it
        // stores its verdict and writes no alert.
        settle_smart_read(&db, &cell, "wwn-leaf", &serde_json::json!({}), SmartSummary::default(), "ok", vec![]);
        assert!(open_health_alert(&db).is_some(), "closed by a SMART read before the import");

        // The pool imports, the leaf is still FAULTED: the same alert stands.
        inventory_pass(&db, &cell, &gate, tank_read(Some("faulted"), &["tank"])).await.unwrap();
        let after = open_health_alert(&db).expect("the faulted disk keeps its alert");
        assert_eq!(after.alert_id, before.alert_id, "one alert, not a new one");
        assert_eq!(after.raised_at, before.raised_at, "its time is when the fault began");
        let live = &cell.read().disks["wwn-leaf"];
        assert!(!live.awaiting_import && live.alert_settled);
        assert_eq!(live.disk.health, "critical");
    }

    /// The other side of the same rule: a disk this process has already
    /// settled follows its leaf (an export takes the pool away and the disk
    /// with it), and past the boot window a pool that never imported is gone
    /// — a destroyed pool leaves its labels on the disks, and they must not
    /// hold an alert for ever.
    #[tokio::test]
    async fn only_a_first_sight_inside_the_boot_window_waits_for_an_import() {
        let db = leaf_db(true);
        seen_ok(&db);
        store::raise_alert(&db, "disk:wwn-leaf:health", "critical", "disk", "wwn-leaf", "Disk sde: critical", "ZFS reports this disk FAULTED")
            .unwrap();
        let cell = RwLock::new(State::new());
        let gate = tokio::sync::Mutex::new(());
        let Some(long_ago) = Instant::now().checked_sub(BOOT_IMPORT_GRACE + Duration::from_secs(1)) else {
            return;
        };
        cell.write().started = long_ago;
        inventory_pass(&db, &cell, &gate, tank_read(None, &[])).await.unwrap();
        assert!(open_health_alert(&db).is_none(), "past the boot window the disk settles as no leaf");
        assert!(!cell.read().disks["wwn-leaf"].awaiting_import);

        // Settled in this process: an import and a later export are leaf
        // changes like any other.
        let db = leaf_db(true);
        seen_ok(&db);
        let cell = RwLock::new(State::new());
        inventory_pass(&db, &cell, &gate, tank_read(Some("faulted"), &["tank"])).await.unwrap();
        assert!(open_health_alert(&db).is_some());
        inventory_pass(&db, &cell, &gate, tank_read(None, &[])).await.unwrap();
        assert!(open_health_alert(&db).is_none(), "an exported pool's disk has no leaf");
    }

    /// A disk SMART still calls critical keeps its ONE alert across a
    /// restart: the first-sight settle refreshes the open row in place (same
    /// id, same `raised_at`), and neither further passes nor a SMART read
    /// add a row.
    #[test]
    fn a_first_sight_settle_never_duplicates_the_open_alert() {
        let db = leaf_db(true);
        store::raise_alert(&db, "disk:wwn-leaf:health", "critical", "disk", "wwn-leaf", "Disk sde: critical", "8 reallocated sectors")
            .unwrap();
        let old = open_health_alert(&db).unwrap();
        let mut live = restarted_live("critical", "8 reallocated sectors");
        for _ in 0..3 {
            if let Some(regrade) = regrade_leaf(&mut live, Some(&HashMap::new())) {
                assert!(settle_leaf_alerts(&db, vec![regrade]).is_empty());
            }
        }
        let cell = RwLock::new(State::new());
        cell.write().disks.insert("wwn-leaf".to_string(), live);
        settle_smart_read(
            &db,
            &cell,
            "wwn-leaf",
            &serde_json::json!({}),
            SmartSummary::default(),
            "critical",
            vec![coded_reason("reallocated", &[("count", "8".to_string())])],
        );

        let all = store::alerts_for_subject(&db, "disk", "wwn-leaf").unwrap();
        assert_eq!(all.len(), 1, "one row, never a second one: {all:?}");
        assert_eq!(all[0].alert_id, old.alert_id);
        assert_eq!(all[0].raised_at, old.raised_at);
        assert!(all[0].resolved_at.is_none());
    }

    /// `refresh_smart` over two disks while the database refuses the writes
    /// (the stored verdict and the alert both): each disk still takes its new
    /// verdict — the first failure ends nothing — and is left unsettled, so
    /// the next inventory pass writes both alerts once the database is back.
    #[test]
    fn a_failed_smart_alert_write_neither_ends_the_pass_nor_is_lost() {
        let broken = leaf_db(false);
        broken.write().unwrap().execute_batch("DROP TABLE nas_disks").expect("drop");
        let cell = RwLock::new(State::new());
        let ids = ["wwn-leaf", "wwn-other"];
        for (id, name) in ids.into_iter().zip(["sde", "sdf"]) {
            let mut live = restarted_live("ok", "");
            live.disk.disk_id = id.to_string();
            live.disk.name = name.to_string();
            // Settled by an earlier pass.
            assert!(regrade_leaf(&mut live, Some(&HashMap::new())).is_some());
            cell.write().disks.insert(id.to_string(), live);
        }
        for id in ids {
            settle_smart_read(
                &broken,
                &cell,
                id,
                &serde_json::json!({}),
                SmartSummary::default(),
                "critical",
                vec![coded_reason("smart_failed", &[])],
            );
        }
        for live in cell.read().disks.values() {
            assert_eq!(live.disk.health, "critical", "{} took its verdict", live.disk.disk_id);
            assert!(!live.alert_settled, "{} is left for the next pass", live.disk.disk_id);
        }

        let good = leaf_db(true);
        let regrades: Vec<LeafRegrade> = cell
            .write()
            .disks
            .values_mut()
            .filter_map(|l| regrade_leaf(l, Some(&HashMap::new())))
            .collect();
        assert_eq!(regrades.len(), 2, "both unchanged leaves are settled again");
        assert!(settle_leaf_alerts(&good, regrades).is_empty());
        for id in ids {
            let open: Vec<_> = store::alerts_for_subject(&good, "disk", id)
                .unwrap()
                .into_iter()
                .filter(|a| a.resolved_at.is_none())
                .collect();
            assert_eq!(open.len(), 1, "{id}");
            assert_eq!(open[0].severity, "critical");
        }
    }

    /// Two inventory passes at once: the sampler's and a dispatch call's.
    /// The first read the disk FAULTED and is still inside its read when the
    /// second starts; the second reads it ONLINE. The second may not read
    /// before the first has written its alert, so what is left is the newer
    /// reading: no open alert, the disk graded by SMART again. Without the
    /// gate the second pass runs to the end first, and the first then raises
    /// a critical alert from the older reading that nothing ever closes.
    #[tokio::test]
    async fn concurrent_inventory_passes_cannot_leave_the_alert_in_the_older_state() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let db = leaf_db(true);
        let cell = RwLock::new(State::new());
        let gate = tokio::sync::Mutex::new(());
        let disk = NasDisk { disk_id: "wwn-leaf".to_string(), name: "sde".to_string(), ..used_disk() };
        // Known from the previous process lifetime, SMART-healthy.
        store::upsert_disk_seen(
            &db,
            &DiskIdentity {
                disk_id: &disk.disk_id,
                name: &disk.name,
                model: &disk.model,
                serial: &disk.serial,
                wwn: disk.wwn.as_deref(),
                size_bytes: disk.size_bytes,
                kind: &disk.kind,
            },
        )
        .unwrap();
        store::store_smart(&db, "wwn-leaf", "{}", "ok", &[]).unwrap();

        let disk = &disk;
        let reading = move |state: &str| -> Result<InventoryRead> {
            Ok((vec![disk.clone()], HashMap::new(), Some(leaves(state)), HashSet::new()))
        };
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let second_read_flag = AtomicBool::new(false);
        let second_read = &second_read_flag;

        let first = inventory_pass(&db, &cell, &gate, move || async move {
            started_tx.send(()).unwrap();
            release_rx.await.unwrap();
            reading("faulted")
        });
        let second = async {
            started_rx.await.unwrap();
            inventory_pass(&db, &cell, &gate, move || async move {
                second_read.store(true, Ordering::SeqCst);
                reading("online")
            })
            .await
        };
        let driver = async {
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            assert!(
                !second_read.load(Ordering::SeqCst),
                "the second pass read the node while the first was still in flight"
            );
            release_tx.send(()).unwrap();
        };
        let (first, second, ()) = tokio::join!(first, second, driver);
        first.unwrap();
        second.unwrap();

        {
            let st = cell.read();
            let live = &st.disks["wwn-leaf"];
            assert_eq!(live.leaf_state.as_deref(), Some("online"));
            assert_eq!(live.disk.health, "ok");
        }
        assert!(open_health_alert(&db).is_none(), "the newer reading closed the older one's alert");
        let all = store::alerts_for_subject(&db, "disk", "wwn-leaf").unwrap();
        assert_eq!(all.len(), 1, "the first pass's alert, raised once and resolved: {all:?}");
    }

    /// The critic's MINOR 4: every pass read every disk's persisted row and
    /// its week-old sample, under the health gate, and threw both away for
    /// any disk already live. Proven through the real pass: once the row can
    /// no longer be read at all, a pass over a disk it already holds still
    /// succeeds, and a pass that meets a NEW disk fails on that very read —
    /// so the first half is not passing because the read was never broken.
    #[tokio::test]
    async fn an_inventory_pass_reads_the_persisted_health_only_of_a_disk_it_has_not_seen() {
        let db = leaf_db(true);
        let cell = RwLock::new(State::new());
        let gate = tokio::sync::Mutex::new(());
        let disk = NasDisk { disk_id: "wwn-leaf".to_string(), name: "sde".to_string(), ..used_disk() };
        let seen = |d: &NasDisk| {
            store::upsert_disk_seen(
                &db,
                &DiskIdentity {
                    disk_id: &d.disk_id,
                    name: &d.name,
                    model: &d.model,
                    serial: &d.serial,
                    wwn: d.wwn.as_deref(),
                    size_bytes: d.size_bytes,
                    kind: &d.kind,
                },
            )
            .unwrap()
        };
        seen(&disk);
        store::store_smart(&db, "wwn-leaf", "{}", "warning", &[coded_reason("reallocated", &[("count", "8".to_string())])])
            .unwrap();
        let reading = |disks: Vec<NasDisk>| {
            move || async move { Ok::<InventoryRead, anyhow::Error>((disks, HashMap::new(), None, HashSet::new())) }
        };

        inventory_pass(&db, &cell, &gate, reading(vec![disk.clone()])).await.unwrap();
        assert_eq!(cell.read().disks["wwn-leaf"].disk.health, "warning", "seeded from the persisted row");

        db.write()
            .unwrap()
            .execute_batch("ALTER TABLE nas_disks RENAME COLUMN health_reason TO health_reason_gone")
            .unwrap();
        inventory_pass(&db, &cell, &gate, reading(vec![disk.clone()]))
            .await
            .expect("a disk already live is not looked up again");
        assert_eq!(cell.read().disks["wwn-leaf"].disk.health, "warning", "the live record is kept");

        let fresh = NasDisk { disk_id: "wwn-new".to_string(), name: "sdf".to_string(), ..used_disk() };
        seen(&fresh);
        assert!(
            inventory_pass(&db, &cell, &gate, reading(vec![disk, fresh])).await.is_err(),
            "a disk first seen IS looked up, and the broken read says so"
        );
    }

    #[test]
    fn smart_summary_for_nvme() {
        let doc: Value = serde_json::json!({
            "smart_status": {"passed": true},
            "temperature": {"current": 38},
            "nvme_smart_health_information_log": {
                "critical_warning": 0, "temperature": 38, "available_spare": 100,
                "available_spare_threshold": 10, "percentage_used": 3, "media_errors": 0,
                "power_on_hours": 900, "unsafe_shutdowns": 2, "num_err_log_entries": 0
            }
        });
        let s = summarize_smart(&doc);
        assert_eq!(s.wear_pct, Some(3));
        assert_eq!(s.power_on_hours, Some(900));
        assert_eq!(scored(&s, "nvme", None).0, "ok");
        let attrs = smart_attributes(&doc);
        assert!(attrs.iter().any(|a| a.name == "Unsafe Shutdowns" && a.status == "warning"));
    }

    /// A real SAS disk, captured verbatim from a node where every such disk
    /// showed as "unknown" before the exit-code fix: `smartctl --json=c -x`
    /// exits 4 (bit 2 set, because an ATA-only command does not apply to a SCSI
    /// disk) and prints a complete document with no ATA attribute table in it.
    /// Pinning the fixture to the capture keeps the SCSI field names honest —
    /// every key read below is a key the disk really emitted.
    #[test]
    fn smart_summary_for_scsi() {
        let doc: Value = serde_json::from_str(SMART_SAS).expect("captured SAS document");
        assert_eq!(doc.pointer("/smartctl/exit_status").and_then(Value::as_i64), Some(4));
        assert_eq!(doc.pointer("/device/type").and_then(Value::as_str), Some("scsi"));

        let s = summarize_smart(&doc);
        assert_eq!(s.passed, Some(true));
        assert_eq!(s.temperature_c, Some(46));
        assert_eq!(s.power_on_hours, Some(32392));
        // The SCSI analogue of reallocated sectors: present, not silently missing.
        assert_eq!(s.reallocated, Some(0));
        // No ATA attribute table, and no honest SCSI equivalent for these two.
        assert_eq!(s.pending, None);
        assert_eq!(s.crc_errors, None);
        // A SAS disk reports no `firmware_version` at all; the revision it does
        // report lives in `scsi_revision`.
        assert!(doc.get("firmware_version").is_none());
        assert_eq!(s.firmware.as_deref(), Some("0101"));
        // Unrecovered errors, summed over the classes the disk reported — this
        // document carries `read` and `write` and no `verify` block.
        assert!(doc.pointer("/scsi_error_counter_log/verify").is_none());
        assert_eq!(s.media_errors, Some(0));
        // Every counter clean, so the disk the node called "unknown" is "ok".
        assert_eq!(scored(&s, "hdd", None), ("ok", String::new()));

        // SCSI keeps its self-test log in numbered keys, not a table.
        let tests = smart_self_tests(&doc);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].kind, "Background short");
        assert_eq!(tests[0].status, "passed");
        assert_eq!(tests[0].lifetime_hours, 32392);
        assert_eq!(tests[0].detail, "Completed");

        // Known gap, asserted so it cannot change unnoticed: the attribute grid
        // of the detail view stays empty for SAS. SCSI has no attribute table
        // and no thresholds, so filling the grid means inventing both.
        assert!(smart_attributes(&doc).is_empty());
    }

    /// SCSI counts unrecovered errors per command class, and smartctl emits
    /// only the classes the disk answered for, so the summary has to add up
    /// what is there without assuming all three keys exist.
    #[test]
    fn scsi_uncorrected_errors_become_media_errors() {
        let counted = |log: Value| {
            summarize_smart(&serde_json::json!({
                "smart_status": {"passed": true},
                "scsi_error_counter_log": log,
            }))
        };

        // All three classes: the disk's total, not the worst single class.
        let s = counted(serde_json::json!({
            "read": {"total_uncorrected_errors": 2},
            "write": {"total_uncorrected_errors": 3},
            "verify": {"total_uncorrected_errors": 4},
        }));
        assert_eq!(s.media_errors, Some(9));
        assert_eq!(scored(&s, "hdd", None), ("warning", "9 media errors".to_string()));

        // `verify` absent — the shape of every disk in the fleet.
        let s = counted(serde_json::json!({
            "read": {"total_uncorrected_errors": 0},
            "write": {"total_uncorrected_errors": 0},
        }));
        assert_eq!(s.media_errors, Some(0));
        assert_eq!(scored(&s, "hdd", None).0, "ok");

        // A log with corrected errors but no uncorrected counter at all is not
        // evidence of zero: "not reported" must stay None, or the UI would show
        // a clean count the disk never gave.
        assert_eq!(counted(serde_json::json!({"read": {"total_errors_corrected": 7}})).media_errors, None);
        // Neither is a document with no counter log.
        assert_eq!(
            summarize_smart(&serde_json::json!({"smart_status": {"passed": true}})).media_errors,
            None
        );
    }

    /// The SCSI self-test result is an SPC code, not a pass/fail flag; it maps
    /// onto the same status vocabulary the ATA and NVMe logs already use.
    #[test]
    fn scsi_self_test_results_map_to_the_shared_vocabulary() {
        let entry = |value: u64, string: &str| {
            serde_json::json!({
                "code": {"value": 1, "string": "Background short"},
                "result": {"value": value, "string": string},
                "power_on_time": {"hours": 100},
            })
        };
        // EVERY code in each range appears, not a representative of it. A
        // mutation run narrowed `4..=7` to `4..=6` with the whole suite still
        // green, because this test asserted 5 but never 4, 6 or 7 — so code 7,
        // a real disk failure, could be silently dropped. Both ends of every
        // range are therefore spelled out below.
        let doc = serde_json::json!({
            "scsi_self_test_0": entry(15, "Self test in progress ..."),
            "scsi_self_test_1": entry(1, "Aborted (by user command)"),
            "scsi_self_test_2": entry(2, "Aborted (device reset ?)"),
            "scsi_self_test_3": entry(4, "Completed, segment failed"),
            "scsi_self_test_4": entry(5, "Failed in first segment"),
            "scsi_self_test_5": entry(6, "Failed in second segment"),
            "scsi_self_test_6": entry(7, "Failed in segment -->"),
            "scsi_self_test_7": entry(3, "Unknown error, incomplete"),
            "scsi_self_test_8": entry(8, "Reserved"),
            "scsi_self_test_9": entry(14, "Reserved"),
            "scsi_self_test_10": entry(0, "Completed"),
        });
        let statuses: Vec<String> = smart_self_tests(&doc).into_iter().map(|t| t.status).collect();
        // Both abort codes are covered: 1 is the user's command, 2 is a device
        // reset. Code 3 is "unknown error, incomplete" and 8–14 are reserved —
        // neither is a segment failure, and the self-test job fails the run on
        // "failed", so calling them one would report a healthy disk as broken.
        assert_eq!(
            statuses,
            [
                "running", "aborted", "aborted", // 15, 1, 2
                "failed", "failed", "failed", "failed", // 4, 5, 6, 7 — both ends
                "unknown", "unknown", "unknown", // 3, 8, 14 — both ends of reserved
                "passed", // 0
            ]
        );
    }

    /// The self-test list reads the SCSI log, so the health score has to read it
    /// too — otherwise a SAS disk shows a red "failed" row in the detail view
    /// while `score_health` calls it "ok" and no alert is ever raised.
    #[test]
    fn a_failed_scsi_self_test_reaches_the_health_score() {
        let entry = |value: u64, string: &str| {
            serde_json::json!({
                "code": {"value": 2, "string": "Background long"},
                "result": {"value": value, "string": string},
                "power_on_time": {"hours": 100},
            })
        };
        let doc = |newest: Value, older: Value| {
            summarize_smart(&serde_json::json!({
                "smart_status": {"passed": true},
                "scsi_grown_defect_list": 0,
                "scsi_self_test_0": newest,
                "scsi_self_test_1": older,
            }))
        };

        let failed = doc(entry(5, "Failed in first segment"), entry(0, "Completed"));
        assert!(failed.self_test_failed);
        assert_eq!(scored(&failed, "hdd", None).0, "critical");

        // A test still running does not hide the verdict underneath it.
        let running = doc(
            entry(15, "Self test in progress ..."),
            entry(5, "Failed in first segment"),
        );
        assert!(running.self_test_failed);

        // A clean newest entry clears it — the last verdict is the one that counts.
        let passed = doc(entry(0, "Completed"), entry(5, "Failed in first segment"));
        assert!(!passed.self_test_failed);
        assert_eq!(scored(&passed, "hdd", None).0, "ok");

        // An incomplete test (SPC 3) is not a disk fault.
        let incomplete = doc(entry(3, "Unknown error, incomplete"), entry(0, "Completed"));
        assert!(!incomplete.self_test_failed);
    }

    /// smartctl appends informational lines to the same `messages` array it
    /// reports errors through, so a failure must lead with what actually failed.
    #[test]
    fn a_failure_reason_leads_with_the_message_that_failed() {
        let out = |messages: &str| broker::CommandOutput {
            code: 2,
            stdout: format!(r#"{{"smartctl":{{"exit_status":2,"messages":{messages}}}}}"#),
            stderr: String::new(),
        };

        let mixed = out(
            r#"[{"string":"Note: mode page changed","severity":"information"},
                {"string":"Smartctl open device: /dev/sdz failed: No such device","severity":"error"}]"#,
        );
        assert_eq!(
            smartctl_failure_detail(&mixed),
            "Smartctl open device: /dev/sdz failed: No such device"
        );

        // Informational is all there is: say it rather than pretending smartctl
        // gave no reason at all.
        let only_info = out(r#"[{"string":"Note: mode page changed","severity":"information"}]"#);
        assert_eq!(smartctl_failure_detail(&only_info), "Note: mode page changed");

        // No severity at all is an unknown shape, not an informational one.
        let bare = out(r#"[{"string":"something went wrong"}]"#);
        assert_eq!(smartctl_failure_detail(&bare), "something went wrong");
    }

    /// smartctl's exit code is a bitmask: only bits 0–1 mean "the command did
    /// not run". Bit 2 and the findings bits still come with a full document.
    #[test]
    fn a_smart_document_is_kept_unless_the_command_itself_failed() {
        const DOC: &str =
            r#"{"smartctl":{"exit_status":4},"smart_status":{"passed":true},"scsi_grown_defect_list":0}"#;
        let out = |code: i32, stdout: &str| broker::CommandOutput {
            code,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };

        // Bit 2 on a SAS disk: the document is complete and must be used.
        let v = smart_document_from_output(&out(4, DOC)).expect("exit 4 must be accepted");
        assert_eq!(
            v.pointer("/smart_status/passed").and_then(Value::as_bool),
            Some(true)
        );
        // Findings bits were already accepted; keep it that way.
        assert!(smart_document_from_output(&out(64, DOC)).is_ok());
        assert!(smart_document_from_output(&out(0, DOC)).is_ok());

        // Bits 0 and 1 genuinely mean there is no usable document.
        assert!(smart_document_from_output(&out(1, DOC)).is_err());
        assert!(smart_document_from_output(&out(2, DOC)).is_err());
        // Nothing came back at all.
        assert!(smart_document_from_output(&out(0, "   ")).is_err());
    }

    /// With `--json` smartctl reports through stdout and leaves stderr empty by
    /// design, so the failure text must come from the document — and must never
    /// claim there was no output when output was there.
    #[test]
    fn a_failed_smart_read_reports_an_honest_reason() {
        // smartctl explained itself in the document: use its own words.
        let spoken = broker::CommandOutput {
            code: 2,
            stdout: r#"{"smartctl":{"exit_status":2,"messages":[{"string":"Smartctl open device: /dev/sdz failed: No such device","severity":"error"}]}}"#.to_string(),
            stderr: String::new(),
        };
        let e = smart_document_from_output(&spoken).unwrap_err().to_string();
        assert!(e.contains("No such device"), "{e}");

        // Nothing at all came back: "no output" is then the truth.
        let silent = broker::CommandOutput {
            code: 2,
            stdout: String::new(),
            stderr: String::new(),
        };
        let e = smart_document_from_output(&silent).unwrap_err().to_string();
        assert!(e.contains("no output"), "{e}");

        // Output was there but smartctl gave no reason for it.
        let mute = broker::CommandOutput {
            code: 1,
            stdout: r#"{"smartctl":{"exit_status":1,"messages":null}}"#.to_string(),
            stderr: String::new(),
        };
        let e = smart_document_from_output(&mute).unwrap_err().to_string();
        assert!(!e.contains("no output"), "must not claim there was none: {e}");
        assert!(e.contains("bytes on stdout"), "{e}");
    }

    fn warned(days: i64, reallocated: Option<u64>) -> (NasDisk, String) {
        let disk = NasDisk {
            disk_id: "wwn-0x5000c500a1b2c3d4".to_string(),
            name: "sdd".to_string(),
            health: "warning".to_string(),
            health_reason: "8 reallocated sectors".to_string(),
            reallocated_sectors: reallocated,
            role: "pool".to_string(),
            member_of: Some("tank".to_string()),
            ..Default::default()
        };
        let since = (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        (disk, since)
    }

    /// §5.10: the recommendation fires on a real history shape and stays quiet
    /// on everything else — a disk that has been unhealthy long enough, or one
    /// whose reallocation count is moving.
    #[test]
    fn a_disk_is_recommended_for_replacement_only_once_its_history_says_so() {
        // Unhealthy since this morning: too early to call.
        let (disk, since) = warned(0, Some(8));
        assert!(replacement_advice(&disk, Some(&since), Some(8), false).is_none());

        // Unhealthy for three days: worth planning a replacement.
        let (disk, since) = warned(3, Some(8));
        let advice = replacement_advice(&disk, Some(&since), Some(8), false).expect("advice");
        assert_eq!(advice.severity, "advice");
        assert_eq!(advice.warning_days, 3);
        assert_eq!(advice.disk_id, "wwn-0x5000c500a1b2c3d4");
        assert_eq!(advice.member_of, "tank");
        assert!(!advice.spare_available);
        assert!(advice.reason.contains("warning for 3 days"), "{}", advice.reason);
        assert!(advice.reason.contains("8 reallocated sectors"), "{}", advice.reason);

        // Reallocations growing: urgent on the FIRST day, because the counter
        // moving is the disk failing now.
        let (disk, since) = warned(0, Some(11));
        let advice = replacement_advice(&disk, Some(&since), Some(8), true).expect("advice");
        assert_eq!(advice.severity, "urgent");
        assert_eq!(advice.warning_days, 0);
        assert!(advice.spare_available);
        assert!(
            advice.reason.contains("grew from 8 to 11 in the last 7 days"),
            "{}",
            advice.reason
        );

        // A healthy disk is never recommended, however long its history.
        let (mut healthy, since) = warned(30, Some(0));
        healthy.health = "ok".to_string();
        healthy.health_reason = String::new();
        assert!(replacement_advice(&healthy, Some(&since), Some(0), false).is_none());

        // Neither is a disk with no history to judge by.
        let (disk, _) = warned(0, None);
        assert!(replacement_advice(&disk, None, None, false).is_none());

        // A critical disk that has been critical for days is urgent.
        let (mut critical, since) = warned(4, Some(8));
        critical.health = "critical".to_string();
        critical.health_reason = "2 pending sectors".to_string();
        let advice = replacement_advice(&critical, Some(&since), Some(8), false).expect("advice");
        assert_eq!(advice.severity, "urgent");
    }

    /// The advice travels as codes too: the growth, then the days the disk
    /// has been unhealthy — from the first whole day, which the English
    /// sentence only says from `ADVICE_AFTER_DAYS` — then the disk's own
    /// codes, without its "growing" when the growth part already says it.
    #[test]
    fn replacement_advice_carries_codes_without_repeating_the_growth() {
        let (mut disk, since) = warned(1, Some(11));
        disk.health_reasons = vec![
            coded_reason("reallocated_growing", &[("from", "8".to_string()), ("to", "11".to_string())]),
            coded_reason("temperature_high", &[("celsius", "54".to_string())]),
        ];
        let advice = replacement_advice(&disk, Some(&since), Some(8), false).expect("advice");
        assert_eq!(advice.severity, "urgent");
        assert_eq!(
            codes(&advice.reasons),
            vec![
                code("reallocated_grew", &[("from", "8"), ("to", "11")]),
                code("unhealthy_for_days", &[("days", "1"), ("health", "warning")]),
                code("temperature_high", &[("celsius", "54")]),
            ]
        );
        assert!(!advice.reason.contains("for 1 days"), "the sentence keeps its own threshold: {}", advice.reason);

        // Old enough, not growing: the disk's codes follow the days whole.
        let (mut disk, since) = warned(3, Some(8));
        disk.health_reasons = vec![coded_reason("reallocated", &[("count", "8".to_string())])];
        let advice = replacement_advice(&disk, Some(&since), Some(8), false).expect("advice");
        assert_eq!(
            codes(&advice.reasons),
            vec![
                code("unhealthy_for_days", &[("days", "3"), ("health", "warning")]),
                code("reallocated", &[("count", "8")]),
            ]
        );
        assert_eq!(advice.reason, "warning for 3 days; 8 reallocated sectors");
    }

    /// The whole list against a real database: the trigger reads the disk's
    /// OPEN health alert and its week-old sample, and the urgent disk sorts
    /// above the merely old one.
    #[test]
    fn the_advice_list_reads_the_alert_and_the_sample_this_node_already_stores() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let conn = rusqlite::Connection::open(dir.path().join("t.db")).expect("db");
        store::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));

        let mut old_disk = NasDisk {
            disk_id: "d-old".to_string(),
            name: "sdb".to_string(),
            health: "warning".to_string(),
            health_reason: "54°C".to_string(),
            role: "pool".to_string(),
            member_of: Some("tank".to_string()),
            ..Default::default()
        };
        let mut growing = old_disk.clone();
        growing.disk_id = "d-growing".to_string();
        growing.name = "sdd".to_string();
        growing.health_reason = "8 reallocated sectors".to_string();
        growing.reallocated_sectors = Some(8);
        let spare = NasDisk {
            disk_id: "d-spare".to_string(),
            name: "sdz".to_string(),
            health: "ok".to_string(),
            // As the inventory really builds it: a pool member whose GROUP is
            // the spares section. The old fixture said `role: "spare"`, a value
            // `role_of` never produces, and so passed while production failed.
            role: "pool_member".to_string(),
            vdev_role: "spare".to_string(),
            member_of: Some("tank".to_string()),
            ..Default::default()
        };

        for disk in [&old_disk, &growing] {
            store::upsert_disk_seen(
                &db,
                &DiskIdentity {
                    disk_id: &disk.disk_id,
                    name: &disk.name,
                    model: "TEST",
                    serial: &disk.disk_id,
                    wwn: None,
                    size_bytes: 0,
                    kind: "hdd",
                },
            )
            .expect("disk");
            store::raise_alert(
                &db,
                &format!("disk:{}:health", disk.disk_id),
                "warning",
                "disk",
                &disk.disk_id,
                "Disk: warning",
                &disk.health_reason,
            )
            .expect("alert");
        }
        // `sdb` has been in warning for five days; the alert row is the age of
        // the verdict, so the fixture moves it back like five days would.
        {
            let conn = db.write().expect("write");
            conn.execute(
                "UPDATE nas_alerts SET raised_at = ?2 WHERE subject_id = ?1",
                rusqlite::params![
                    "d-old",
                    (chrono::Utc::now() - chrono::Duration::days(5))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ],
            )
            .expect("age the alert");
        }
        // …and `sdd`'s week-old sample had five reallocations, so its counter
        // has moved by three.
        let week_ago = (chrono::Utc::now() - chrono::Duration::days(8))
            .format("%Y-%m-%dT%H:%M:00Z")
            .to_string();
        store::insert_samples(
            &db,
            &[SampleInsert {
                disk_id: "d-growing",
                at: &week_ago,
                temperature_c: Some(40),
                reallocated: Some(5),
                pending: Some(0),
                crc_errors: Some(0),
                media_errors: Some(0),
                read_bps: 0,
                write_bps: 0,
                await_ms: 0.0,
            }],
        )
        .expect("sample");

        old_disk.health_reason = "54°C".to_string();
        let rows = advice(&db, &[old_disk.clone(), growing.clone(), spare.clone()]);
        assert_eq!(rows.len(), 2, "the healthy spare is not advice");
        assert_eq!(rows[0].name, "sdd", "the urgent one comes first");
        assert_eq!(rows[0].severity, "urgent");
        assert_eq!(rows[0].reallocated_week_ago, Some(5));
        assert!(rows[0].spare_available, "tank has a healthy spare");
        assert_eq!(rows[1].name, "sdb");
        assert_eq!(rows[1].severity, "advice");
        assert_eq!(rows[1].warning_days, 5);

        // Once the disk is healthy again its alert is resolved and the advice
        // goes with it.
        store::resolve_alert(&db, "disk:d-old:health").expect("resolve");
        let mut healthy = old_disk;
        healthy.health = "ok".to_string();
        let rows = advice(&db, &[healthy, spare]);
        assert!(rows.is_empty());
    }

    /// A disk that has LEFT the live inventory is the one whose alert matters
    /// most, and its title used to fall back to the id (`Disk wwn-5000…`).
    /// The database keeps the row, so the title names the disk as it was last
    /// seen — and says so, because the kernel may have given that name to
    /// another disk since.
    #[test]
    fn a_health_alert_for_a_disk_gone_from_the_inventory_uses_its_last_known_name() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        // An id no test puts into the process-wide live map.
        let id = "wwn-left-the-inventory-5000cca27dc7a4c6";
        store::upsert_disk_seen(
            &db,
            &DiskIdentity {
                disk_id: id,
                name: "sdq",
                model: "HGST",
                serial: "S1",
                wwn: None,
                size_bytes: 1,
                kind: "hdd",
            },
        )
        .expect("record the disk");
        assert!(disk_name(id).is_none(), "the disk is not in the live inventory");
        let reasons = [coded_reason("reallocated", &[("count", "2".to_string())])];
        sync_health_alert(&db, id, "ok", "warning", "2 reallocated sectors", &reasons, &HealthAlertPlace::default()).expect("raise");
        let alert = store::list_alerts(&db, false)
            .expect("alerts")
            .into_iter()
            .find(|a| a.subject_id == id)
            .expect("the alert is raised");
        assert_eq!(alert.title, "Disk last seen as sdq: warning");
        assert!(!alert.title.contains("wwn-"), "{}", alert.title);
        // The same, as the codes a screen words: the name as last known.
        assert_eq!(alert.code, "disk_health");
        assert_eq!(
            alert.params.clone().into_iter().collect::<Vec<_>>(),
            vec![
                ("health".to_string(), "warning".to_string()),
                ("name".to_string(), "sdq".to_string()),
                ("name_source".to_string(), "last_known".to_string()),
            ]
        );
        assert_eq!(alert.reasons, reasons.to_vec());

        // A disk the node never recorded: still never the id.
        let unknown = "wwn-never-recorded-5000cca27dc7a4c7";
        sync_health_alert(&db, unknown, "ok", "critical", "8 reallocated sectors", &[], &HealthAlertPlace::default()).expect("raise");
        let alert = store::list_alerts(&db, false)
            .expect("alerts")
            .into_iter()
            .find(|a| a.subject_id == unknown)
            .expect("the alert is raised");
        assert_eq!(alert.title, "Disk: critical");
        assert_eq!(alert.params.get("name_source").map(String::as_str), Some("unknown"));
        assert!(!alert.params.contains_key("name"), "no name, and never the id: {:?}", alert.params);
    }

    #[test]
    fn a_disk_is_named_live_first_then_last_known_and_never_by_its_id() {
        let never = || -> Option<String> { panic!("a live name must not consult the database") };
        assert_eq!(pick_shown_name(Some("sdg".into()), Some("sdh"), never), ShownDiskName::Live("sdg".into()));
        // The inventory does not hold it yet, the helper saw the device now.
        assert_eq!(pick_shown_name(None, Some("sdh"), never), ShownDiskName::Live("sdh".into()));
        assert_eq!(pick_shown_name(Some(String::new()), Some(" sdh "), never), ShownDiskName::Live("sdh".into()));
        // Nothing live: the remembered name, as last-known — never as live.
        assert_eq!(pick_shown_name(None, None, || Some("sdq".into())), ShownDiskName::LastKnown("sdq".into()));
        assert_eq!(pick_shown_name(None, Some(""), || Some("sdq".into())), ShownDiskName::LastKnown("sdq".into()));
        assert_eq!(pick_shown_name(None, None, || None), ShownDiskName::Unknown);
        assert_eq!(pick_shown_name(None, None, || Some("  ".into())), ShownDiskName::Unknown);
    }

    /// n02 names a disk alert's pool ("tank · raidz2") and n01 adds "—
    /// zaplanuj wymianę dysku" — both only from what the node put on the
    /// alert when it wrote it: the pool of a ZFS member (never an Elastic
    /// Array, which is an organisation's), and the node's own replacement
    /// advice, whose clock is the open row's age.
    #[test]
    fn a_disk_alert_names_its_pool_and_carries_the_nodes_replacement_advice() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        store::migrate(&conn).expect("migrate");
        let db: DbPool = Arc::new(crate::db::Db::from_connection(conn));
        let id = "wwn-place-5000cca27dc7a4c8";
        let disk = NasDisk {
            disk_id: id.into(),
            name: "sdd".into(),
            role: "pool_member".into(),
            member_of: Some("tank".into()),
            vdev_role: "data".into(),
            vdev_kind: "raidz2".into(),
            health: "warning".into(),
            ..Default::default()
        };
        let alert = |db: &DbPool| {
            store::alerts_for_subject(db, "disk", id)
                .unwrap()
                .into_iter()
                .find(|a| a.resolved_at.is_none())
                .expect("an open alert")
        };
        let param = |a: &tentaflow_protocol::tentanas::NasAlert, k: &str| a.params.get(k).cloned();

        write_health_alert(&db, id, "warning", "2 reallocated sectors", &[], Some(&disk)).expect("raise");
        let fresh = alert(&db);
        assert_eq!(param(&fresh, "pool").as_deref(), Some("tank"));
        assert_eq!(param(&fresh, "layout").as_deref(), Some("raidz2"));
        assert_eq!(param(&fresh, "advice"), None, "a fresh warning is not yet advice");

        // Three days in the same verdict: the node's advice holds, and the
        // row says so without being raised again.
        {
            let conn = db.write().expect("write");
            conn.execute(
                "UPDATE nas_alerts SET raised_at = ?2 WHERE subject_id = ?1",
                rusqlite::params![
                    id,
                    (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ],
            )
            .expect("age the alert");
        }
        write_health_alert(&db, id, "warning", "2 reallocated sectors", &[], Some(&disk)).expect("refresh");
        let aged = alert(&db);
        assert_eq!(aged.alert_id, fresh.alert_id, "the same row");
        assert_eq!(param(&aged, "advice").as_deref(), Some("replace"));
        assert!(replacement_advice(&disk, Some(&aged.raised_at), None, false).is_some(), "the advice list agrees");

        // A changed grade is a new row, and a new row has no age yet.
        let critical = NasDisk { health: "critical".into(), ..disk.clone() };
        write_health_alert(&db, id, "critical", "FAULTED", &[], Some(&critical)).expect("escalate");
        let escalated = alert(&db);
        assert_ne!(escalated.alert_id, fresh.alert_id);
        assert_eq!(param(&escalated, "advice"), None, "no two days in this verdict yet");

        // An Elastic member: its array is never named on a node-wide alert.
        let member = NasDisk {
            role: "array_member".into(),
            member_of: Some("media".into()),
            vdev_kind: String::new(),
            ..disk.clone()
        };
        write_health_alert(&db, id, "critical", "FAULTED", &[], Some(&member)).expect("refresh");
        let array = alert(&db);
        assert_eq!(param(&array, "pool"), None);
        assert_eq!(param(&array, "layout"), None);
        assert!(array.params.values().all(|v| v != "media"), "{:?}", array.params);

        // An Elastic member whose own advice holds (three days in warning):
        // still no "replace" on the alert — n03/n04 say "watch it", and
        // replacing an array member is not offered in this version.
        let member_warning = NasDisk { health: "warning".into(), ..member };
        write_health_alert(&db, id, "warning", "2 reallocated sectors", &[], Some(&member_warning)).expect("re-raise");
        {
            let conn = db.write().expect("write");
            conn.execute(
                "UPDATE nas_alerts SET raised_at = ?2 WHERE subject_id = ?1 AND resolved_at IS NULL",
                rusqlite::params![
                    id,
                    (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                ],
            )
            .expect("age the alert");
        }
        let aged_since = alert(&db).raised_at;
        assert!(replacement_advice(&member_warning, Some(&aged_since), None, false).is_some(), "the advice itself holds");
        write_health_alert(&db, id, "warning", "2 reallocated sectors", &[], Some(&member_warning)).expect("refresh");
        assert_eq!(param(&alert(&db), "advice"), None, "no replacement suffix for an Elastic member");
    }
}
