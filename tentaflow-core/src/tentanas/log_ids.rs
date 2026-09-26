// =============================================================================
// File: tentanas/log_ids.rs — a job log names things; it never carries an id
//       (owner decision 2026-09-26).
//
// Every line a job writes goes through `JobHandle::log`, and every line is
// passed through `LogNames::scrub` there, whoever composed it: the core's own
// sentences, a catalog command's argv (`$ zpool create … /dev/disk/by-id/…`)
// and whatever a tool printed on stdout/stderr. What an id is, and what it
// is replaced with:
//
// - a `/dev/disk/by-*/…` path names its disk by the kernel name
//   (`/dev/sdb`, `/dev/nvme0n1p1`): from the live inventory, else from the
//   link itself on this node; a gone disk is its model; a link nothing
//   resolves keeps its directory and loses its basename;
// - a bare by-id name (`ata-…`, `nvme-…`, `usb-…`, `virtio-…`, `wwn-…`,
//   `scsi-…`), a filesystem or partition UUID of any length and a `zfs-<hex>`
//   partition label are named the same way: the inventory first, then the
//   link under `/dev/disk/by-{id,uuid,partuuid,partlabel}` on this node;
// - an Elastic Array's id is its name, a member filesystem's UUID the member
//   disk's kernel name, a known pool's GUID the pool's name;
// - whatever is still shaped like an id — a UUID (a job, an operation, a
//   filesystem), an MBR PARTUUID or vfat UUID, a 32+ hex run, a by-id
//   name, a `0x…`/`naa.`/`eui.`/`t10.` WWN, a `zfs-<hex>` label — is
//   `HIDDEN`.
//
// Never hidden (critic wave 9a, MINOR 1/2): a plain decimal number (a size,
// a count, a timestamp), and a name a person chose — a pool, array, share or
// target name this node knows is kept as it is, and a by-id prefix is an id
// only with a by-id body behind it (`ata-archive` is a pool name, `ata-WDC_…
// _WD-WCC7K1234567` is a disk).
//
// `HIDDEN` is a language-neutral token the screen words in the reader's
// language (`jobLogLines`, format.js). The screen scrubs every line again
// when it paints it (`scrubIds`, machine-id.js): the backstop for rows
// written before this, not the rule.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;

use crate::db::DbPool;

/// What an id nothing names reads as: a token, not a word — the screen shows
/// it in the reader's language.
pub const HIDDEN: &str = "⟦id⟧";

/// Characters that end a token in free text. `:` is one of them so the
/// value of a `"wwn":"…"` pair or a `…:uuid:…` NQN is its own token.
const DELIMITERS: &[char] = &['\'', '"', '`', '„', '”', '«', '»', '(', ')', '[', ']', '{', '}', '<', '>', ',', ';', '|', '=', ':'];

/// Where a bare id may be looked up as a link on this node.
const LINK_DIRS: &[&str] = &["/dev/disk/by-id", "/dev/disk/by-uuid", "/dev/disk/by-partuuid", "/dev/disk/by-partlabel"];

fn uuid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").expect("uuid regex")
    })
}

fn by_path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"/dev/disk/by-([a-z]+)/([^\s'"`()\[\]{}<>,;|=]+)"#).expect("by-path regex")
    })
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `base` without a by-id partition suffix (`-part2`), and that suffix.
fn split_partition(base: &str) -> (&str, Option<&str>) {
    match base.rsplit_once("-part") {
        Some((disk, part)) if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) => (disk, Some(part)),
        _ => (base, None),
    }
}

/// A by-id body: `<model>_<serial>` whose last part has a digit (udev's
/// `ata-`, `scsi-SATA_`, `nvme-`, `usb-…-0:0` names), or a long hex run
/// (`scsi-3…`, `wwn-…`).
fn by_id_body(rest: &str) -> bool {
    let rest = rest.split(':').next().unwrap_or(rest);
    let rest = rest.strip_suffix("-0").unwrap_or(rest);
    if is_hex(rest.trim_start_matches("0x")) && rest.trim_start_matches("0x").len() >= 12 {
        return true;
    }
    rest.rsplit_once('_').is_some_and(|(model, serial)| {
        !model.is_empty() && serial.len() >= 6 && serial.chars().any(|c| c.is_ascii_digit())
            && serial.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    })
}

/// Whether one bare token (no delimiters, no path separator) is shaped like
/// an id. A plain decimal number never is.
fn is_id_token(token: &str) -> bool {
    let (token, _) = split_partition(token);
    if token.is_empty() {
        return false;
    }
    // A decimal is an id only at a ZFS GUID's length (a random 64-bit
    // number, 17-20 digits nearly always): a size of 10^16 bytes is 8.9 PiB,
    // so sizes, counts and timestamps stay readable (critic wave 9a, R2).
    if token.chars().all(|c| c.is_ascii_digit()) {
        return (17..=20).contains(&token.len());
    }
    if token.len() >= 32 && is_hex(token) {
        return true;
    }
    let lower = token.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        return hex.len() >= 16 && is_hex(hex);
    }
    // MBR PARTUUID (`0a1b2c3d-01`) and vfat UUID (`ABCD-1234`): only with a
    // hex letter, so a range of plain numbers stays.
    let has_letter = lower.chars().any(|c| ('a'..='f').contains(&c));
    if let Some((a, b)) = lower.split_once('-') {
        if has_letter && is_hex(a) && is_hex(b) && ((a.len() == 8 && b.len() == 2) || (a.len() == 4 && b.len() == 4)) {
            return true;
        }
    }
    if let Some(hex) = lower.strip_prefix("zfs-") {
        return hex.len() >= 12 && is_hex(hex);
    }
    for prefix in ["dm-uuid-", "md-uuid-", "lvm-pv-uuid-", "nvme-eui.", "nvme-uuid.", "nvme-nvme."] {
        if lower.starts_with(prefix) {
            return true;
        }
    }
    for prefix in ["eui.", "naa.", "wwn-"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_start_matches("0x");
            return rest.len() >= 16 && is_hex(rest);
        }
    }
    if let Some(rest) = lower.strip_prefix("t10.") {
        return rest.len() >= 8;
    }
    // TentaNas's own disk id for a disk with a serial and no WWN
    // (`disks::disk_id`): `sn-<serial>`.
    if let Some(serial) = lower.strip_prefix("sn-") {
        return serial.len() >= 6 && serial.chars().any(|c| c.is_ascii_digit())
            && serial.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    }
    for prefix in ["ata-", "scsi-", "nvme-", "usb-", "virtio-", "mmc-", "ieee1394-", "memstick-"] {
        if lower.starts_with(prefix) {
            return by_id_body(&token[prefix.len()..]);
        }
    }
    false
}

/// Whether a token is worth looking up as a link on this node: an id shape,
/// or a by-id prefix whose body is not recognisable (a `virtio-` serial the
/// admin chose).
fn link_candidate(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    is_id_token(token)
        || ["ata-", "scsi-", "nvme-", "usb-", "virtio-", "mmc-", "wwn-", "sn-"].iter().any(|p| lower.starts_with(p))
}

/// `name` with partition `part` appended the way the kernel names it:
/// `sdb` + 1 → `sdb1`, `nvme0n1` + 1 → `nvme0n1p1`.
fn partition_name(name: &str, part: &str) -> String {
    if name.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{name}p{part}")
    } else {
        format!("{name}{part}")
    }
}

/// How a link is resolved on this node: a path → the device it points at.
type Resolve<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The names one scrub replaces ids with. Built per `JobHandle::log` call:
/// a handful of keyed reads, and the inventory is already in memory.
#[derive(Debug, Default, Clone)]
pub struct LogNames {
    /// id (lower-cased for UUIDs and hex) → the name shown instead.
    names: HashMap<String, String>,
    /// The ids whose name is a kernel device name (`sdb`), which a by-id
    /// path turns into `/dev/sdb`; a model or an array name is not one.
    devices: HashSet<String>,
    /// Serial → (the name shown, whether it is a kernel device name): a by-id
    /// name of any scheme that ends in a disk's serial is that disk.
    serials: Vec<(String, String, bool)>,
    /// Names a person chose (pools, arrays, shares, targets): never hidden.
    keep: HashSet<String>,
}

impl LogNames {
    /// Everything this node can name: the live disks, the disks it has seen
    /// before, its known pools, its Elastic Arrays and their members'
    /// filesystems, its shares and targets. A read that fails leaves those
    /// ids to `HIDDEN` — never to themselves.
    pub fn load(db: &DbPool) -> Self {
        let mut out = Self::default();
        let live = super::disks::snapshot().0;
        let mut kernel_of_disk: HashMap<String, String> = HashMap::new();
        for d in &live {
            if d.name.is_empty() {
                continue;
            }
            kernel_of_disk.insert(d.disk_id.clone(), d.name.clone());
            out.insert_device(&d.disk_id, &d.name);
            if let Some(wwn) = d.wwn.as_deref().filter(|w| !w.is_empty()) {
                out.insert_device(wwn, &d.name);
                out.insert_device(&format!("wwn-{wwn}"), &d.name);
            }
            // Six characters at least: a short serial (a USB bridge's
            // `0000`) is also an ordinary word or number of tool output.
            if d.serial.len() >= 6 {
                out.insert_device(&d.serial, &d.name);
                out.serials.push((d.serial.clone(), d.name.clone(), true));
            }
        }
        // A leaf zpool names by its GUID: the name the node knows it by.
        for (guid, name) in super::pools::named_leaf_guids() {
            out.insert(&guid, &name);
        }
        if let Ok(pools) = super::db::known_pools(db) {
            for (name, guid) in pools {
                out.insert(&guid, &name);
                out.keep.insert(name);
            }
        }
        let Ok(conn) = db.read() else {
            return out;
        };
        // A disk that is gone: its model, never its old kernel name.
        if let Ok(mut stmt) = conn.prepare("SELECT disk_id, model, serial, wwn FROM nas_disks") {
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?))
            });
            if let Ok(rows) = rows {
                for (disk_id, model, serial, wwn) in rows.flatten() {
                    if kernel_of_disk.contains_key(&disk_id) || model.trim().is_empty() {
                        continue;
                    }
                    let model = model.trim().to_string();
                    out.insert_missing(&disk_id, &model);
                    if let Some(wwn) = wwn.filter(|w| !w.is_empty()) {
                        out.insert_missing(&wwn, &model);
                        out.insert_missing(&format!("wwn-{wwn}"), &model);
                    }
                    if serial.len() >= 6 {
                        out.insert_missing(&serial, &model);
                        out.serials.push((serial, model, false));
                    }
                }
            }
        }
        let mut arrays: HashMap<String, String> = HashMap::new();
        if let Ok(mut stmt) = conn.prepare("SELECT array_id, name FROM nas_elastic_arrays") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for (id, name) in rows.flatten() {
                    out.insert(&id, &name);
                    out.keep.insert(name.clone());
                    arrays.insert(id, name);
                }
            }
        }
        for table in ["nas_shares", "nas_targets"] {
            if let Ok(mut stmt) = conn.prepare(&format!("SELECT name FROM {table}")) {
                if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                    out.keep.extend(rows.flatten());
                }
            }
        }
        if let Ok(mut stmt) = conn.prepare("SELECT array_id, disk_id, expected_uuid FROM nas_elastic_disks") {
            if let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            }) {
                for (array_id, disk_id, uuid) in rows.flatten() {
                    match kernel_of_disk.get(&disk_id) {
                        Some(kernel) => out.insert_device(&uuid, kernel),
                        None => {
                            if let Some(shown) = out.names.get(&disk_id).cloned().or_else(|| arrays.get(&array_id).cloned()) {
                                out.insert(&uuid, &shown);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn key(id: &str) -> String {
        if uuid_re().is_match(id) || is_hex(id) || id.contains('-') && is_hex(&id.replace('-', "")) {
            id.to_ascii_lowercase()
        } else {
            id.to_string()
        }
    }

    /// `id` reads as `name` in every line scrubbed with this set.
    pub fn insert(&mut self, id: &str, name: &str) {
        if !id.is_empty() && !name.is_empty() {
            self.names.insert(Self::key(id), name.to_string());
        }
    }

    /// `id` reads as the kernel device `name` (`sdb`).
    pub fn insert_device(&mut self, id: &str, name: &str) {
        if !id.is_empty() && !name.is_empty() {
            self.names.insert(Self::key(id), name.to_string());
            self.devices.insert(Self::key(id));
        }
    }

    /// `name` is a name a person chose: it is never hidden.
    pub fn keep_name(&mut self, name: &str) {
        if !name.is_empty() {
            self.keep.insert(name.to_string());
        }
    }

    fn insert_missing(&mut self, id: &str, name: &str) {
        if !id.is_empty() && !name.is_empty() {
            self.names.entry(Self::key(id)).or_insert_with(|| name.to_string());
        }
    }

    /// The name of an id, and whether it is a kernel device name. A by-id
    /// name not known as such is matched by the serial it ends in.
    fn lookup(&self, id: &str) -> Option<(String, bool)> {
        if let Some(name) = self.names.get(&Self::key(id)) {
            return Some((name.clone(), self.devices.contains(&Self::key(id))));
        }
        self.serials
            .iter()
            .find(|(serial, _, _)| id.ends_with(&format!("_{serial}")))
            .map(|(_, name, device)| (name.clone(), *device))
    }

    /// A disk (or partition) id as the kernel names its device: `sdb`,
    /// `nvme0n1p1` — or the model of a gone disk, or what the link on this
    /// node resolves to. `None` when nothing names it.
    fn device_name(&self, base: &str, dirs: &[&str], resolve: Resolve<'_>) -> Option<String> {
        let (disk, part) = split_partition(base);
        let known = self.lookup(disk).map(|n| (n, part)).or_else(|| self.lookup(base).map(|n| (n, None)));
        if let Some(((name, device), part)) = known {
            return Some(match part.filter(|_| device) {
                Some(part) => partition_name(&name, part),
                None => name,
            });
        }
        dirs.iter().find_map(|dir| resolve(&format!("{dir}/{base}"))).map(|real| {
            real.strip_prefix("/dev/").map(str::to_string).unwrap_or(real)
        })
    }

    /// A by-id / by-path / by-uuid path as the kernel names its device.
    fn device_path(&self, kind: &str, base: &str, resolve: Resolve<'_>) -> String {
        let dir = format!("/dev/disk/by-{kind}");
        let known_device = |id: &str| self.lookup(split_partition(id).0).is_some_and(|(_, device)| device);
        match self.device_name(base, &[dir.as_str()], resolve) {
            Some(name) if known_device(base) || !self.lookup(split_partition(base).0).is_some() => format!("/dev/{name}"),
            // A gone disk: its model, which is no device path.
            Some(model) => model,
            // A filesystem label is a name somebody chose, not an id.
            None if kind == "label" => format!("{dir}/{base}"),
            None => format!("{dir}/{HIDDEN}"),
        }
    }

    /// `line` with every id named or hidden. See the file comment.
    pub fn scrub(&self, line: &str) -> String {
        self.scrub_with(line, &|path: &str| {
            std::fs::canonicalize(path)
                .ok()
                .map(|p| p.display().to_string())
                .filter(|p| p.starts_with("/dev/") && !p.starts_with("/dev/disk/"))
        })
    }

    /// `scrub` with the link resolution injected, for tests.
    fn scrub_with(&self, line: &str, resolve: Resolve<'_>) -> String {
        let by_path = by_path_re().replace_all(line, |c: &regex::Captures<'_>| self.device_path(&c[1], &c[2], resolve));
        let uuids = uuid_re().replace_all(&by_path, |c: &regex::Captures<'_>| {
            self.lookup(&c[0])
                .map(|(name, _)| name)
                .or_else(|| self.device_name(&c[0], LINK_DIRS, resolve))
                .unwrap_or_else(|| HIDDEN.to_string())
        });
        let mut out = String::with_capacity(uuids.len());
        let mut token = String::new();
        for ch in uuids.chars() {
            if ch.is_whitespace() || DELIMITERS.contains(&ch) {
                self.push_token(&mut out, &token, resolve);
                token.clear();
                out.push(ch);
            } else {
                token.push(ch);
            }
        }
        self.push_token(&mut out, &token, resolve);
        out
    }

    /// One token, judged path segment by path segment
    /// (`…/wwn-0x5…/part1`); trailing sentence punctuation stays.
    fn push_token(&self, out: &mut String, token: &str, resolve: Resolve<'_>) {
        if token.is_empty() {
            return;
        }
        if self.keep.contains(token) {
            out.push_str(token);
            return;
        }
        // A dataset path of a pool this node knows (`tank/usb-backup_2024`,
        // `tank/vm@auto-1`): every segment after the pool is a name a person
        // chose.
        if let Some((pool, _)) = token.split_once('/') {
            if self.keep.contains(pool) {
                out.push_str(token);
                return;
            }
        }
        for (i, segment) in token.split('/').enumerate() {
            if i > 0 {
                out.push('/');
            }
            let bare = segment.trim_end_matches(['.', '!', '?']);
            let tail = &segment[bare.len()..];
            if bare.is_empty() || self.keep.contains(bare) || bare == HIDDEN {
                out.push_str(segment);
                continue;
            }
            let named = self.lookup(bare).map(|(name, _)| name).or_else(|| {
                link_candidate(bare).then(|| self.device_name(bare, LINK_DIRS, resolve)).flatten()
            });
            match named {
                Some(name) => {
                    out.push_str(&name);
                    out.push_str(tail);
                }
                None if is_id_token(bare) => {
                    out.push_str(HIDDEN);
                    out.push_str(tail);
                }
                None => out.push_str(segment),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> LogNames {
        let mut n = LogNames::default();
        n.insert_device("wwn-0x5000c500a1b2c3d4", "sdb");
        n.insert_device("0x5000c500a1b2c3d4", "sdb");
        n.insert_device("ata-WDC_WD40EFRX-68N32N0_WD-WCC7K1234567", "sdb");
        n.insert_device("WD-WCC7K1234567", "sdb");
        n.serials.push(("WD-WCC7K1234567".into(), "sdb".into(), true));
        n.insert_device("nvme-Samsung_SSD_980_S64ANJ0R123456", "nvme0n1");
        n.insert("0191f2c0-7a3b-7c11-9d2e-1234567890ab", "media");
        n.keep_name("ata-archive");
        n
    }

    const NO_LINKS: Resolve<'static> = &|_| None;

    /// Critic wave 9a, R2-MINOR 5: TentaNas's own `sn-<serial>` disk id is
    /// named when the disk is known and hidden when not; a dataset path of a
    /// known pool keeps its names; a short serial renames nothing.
    #[test]
    fn own_disk_ids_dataset_paths_and_short_serials() {
        let mut n = names();
        n.insert_device("sn-ZA1B2C3D", "sdq");
        n.keep_name("tank");
        assert_eq!(n.scrub_with("disk sn-ZA1B2C3D: FAULTED", NO_LINKS), "disk sdq: FAULTED");
        assert_eq!(n.scrub_with("disk sn-WD-WCC7K7654321 gone", NO_LINKS), format!("disk {H} gone"));
        assert_eq!(n.scrub_with("share sn-backups, sn-1", NO_LINKS), "share sn-backups, sn-1");
        assert_eq!(n.scrub_with("zfs create tank/usb-backup_202405/sn-A1B2C3D4E5", NO_LINKS), "zfs create tank/usb-backup_202405/sn-A1B2C3D4E5");
        let loaded = |serial: &str| {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            super::super::db::migrate(&conn).unwrap();
            let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
            super::super::db::upsert_disk_seen(&db, &super::super::db::DiskIdentity {
                disk_id: "sn-gone0001", name: "sdz", model: "USB Bridge", serial, wwn: None, size_bytes: 1, kind: "hdd",
            }).unwrap();
            LogNames::load(&db)
        };
        assert_eq!(loaded("0000").scrub_with("read 0000 blocks, ata-X_Y_0000", NO_LINKS), "read 0000 blocks, ata-X_Y_0000", "a 4-character serial names nothing");
        assert_eq!(loaded("ABC123").scrub_with("serial ABC123", NO_LINKS), "serial USB Bridge", "six characters name the disk");
    }

    const H: &str = HIDDEN;

    #[test]
    fn a_disk_path_names_the_kernel_device() {
        let n = names();
        assert_eq!(
            n.scrub_with("$ zpool create tank mirror /dev/disk/by-id/wwn-0x5000c500a1b2c3d4 /dev/disk/by-id/nvme-Samsung_SSD_980_S64ANJ0R123456-part1", NO_LINKS),
            "$ zpool create tank mirror /dev/sdb /dev/nvme0n1p1"
        );
        assert_eq!(n.scrub_with("/dev/disk/by-id/ata-WDC_WD40EFRX-68N32N0_WD-WCC7K1234567-part2", NO_LINKS), "/dev/sdb2");
        // Another scheme's name for the same disk, by the serial it ends in.
        assert_eq!(n.scrub_with("/dev/disk/by-id/scsi-SATA_WDC_WD40EFRX-68N_WD-WCC7K1234567", NO_LINKS), "/dev/sdb");
        // A link the inventory does not know is resolved on the node.
        let resolve: Resolve<'_> = &|p| (p == "/dev/disk/by-partuuid/0a1b2c3d-01").then(|| "/dev/sdc1".to_string());
        assert_eq!(n.scrub_with("mount /dev/disk/by-partuuid/0a1b2c3d-01 /mnt/x", resolve), "mount /dev/sdc1 /mnt/x");
        // Nothing names it: the directory stays, the id goes.
        assert_eq!(n.scrub_with("open /dev/disk/by-id/wwn-0x5000c500ffffffff failed", NO_LINKS), format!("open /dev/disk/by-id/{H} failed"));
        assert_eq!(n.scrub_with("/dev/disk/by-label/backup", NO_LINKS), "/dev/disk/by-label/backup", "a label is a name");
    }

    /// Critic wave 9a, MINOR 3: the ids that used to pass — named where the
    /// node can name them, hidden where it cannot.
    #[test]
    fn short_and_unprefixed_ids_are_named_or_hidden() {
        let n = names();
        let resolve: Resolve<'_> = &|p| match p {
            "/dev/disk/by-id/virtio-vm-data" => Some("/dev/vdb".to_string()),
            "/dev/disk/by-uuid/ABCD-1234" => Some("/dev/sdc1".to_string()),
            "/dev/disk/by-partlabel/zfs-2f3c4d5e6f7a8b9c" => Some("/dev/sdd1".to_string()),
            _ => None,
        };
        assert_eq!(n.scrub_with("vdev virtio-vm-data ONLINE", resolve), "vdev vdb ONLINE");
        assert_eq!(n.scrub_with("UUID=\"ABCD-1234\" TYPE=\"vfat\"", resolve), "UUID=\"sdc1\" TYPE=\"vfat\"");
        assert_eq!(n.scrub_with("label zfs-2f3c4d5e6f7a8b9c", resolve), "label sdd1");
        for (line, want) in [
            ("lun naa.60014054d1a2b3c4d5e6f7a8b9c0d1e2", format!("lun {H}")),
            ("t10.ATA_WDC_WD40EFRX-68N32N0_WD-WCC7K7654321", H.to_string()),
            ("PARTUUID=\"0a1b2c3d-01\" UUID=\"BEEF-12AB\"", format!("PARTUUID=\"{H}\" UUID=\"{H}\"")),
            ("zfs-2f3c4d5e6f7a8b9c", H.to_string()),
            ("vdev nvme-Samsung_SSD_990_S7KGNU0X123456 ONLINE", format!("vdev {H} ONLINE")),
            ("usb-SanDisk_Cruzer_4C530001234567-0:0", format!("{H}:0")),
            ("wwn-0x5000c500deadbeef", H.to_string()),
            ("uuid 5f1e2d3c-aaaa-bbbb-cccc-1234567890ab", format!("uuid {H}")),
            ("node 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", format!("node {H}")),
            ("nqn.2014-08.org.nvmexpress:uuid:5f1e2d3c-aaaa-bbbb-cccc-1234567890ab", format!("nqn.2014-08.org.nvmexpress:uuid:{H}")),
            ("disk eui.0025385b71b0a1c2 gone", format!("disk {H} gone")),
        ] {
            assert_eq!(n.scrub_with(line, NO_LINKS), want, "{line}");
        }
    }

    #[test]
    fn known_ids_are_named() {
        let n = names();
        assert_eq!(n.scrub_with("rm /var/lib/tentanas/0191f2c0-7a3b-7c11-9d2e-1234567890ab.json", NO_LINKS), "rm /var/lib/tentanas/media.json");
        assert_eq!(n.scrub_with(r#"{"guid":9876543210987654321,"wwn":"0x5000c500a1b2c3d4"}"#, NO_LINKS), format!(r#"{{"guid":{H},"wwn":"sdb"}}"#));
        assert_eq!(n.scrub_with("serial WD-WCC7K1234567.", NO_LINKS), "serial sdb.");
    }

    /// Critic wave 9a, R2-BLOCKER 1: a ZFS GUID never reaches the log. A leaf
    /// GUID the node knows is the disk's name, a known pool's GUID the pool's
    /// name, any other 17-20 digit number is hidden — and a size stays.
    #[test]
    fn zfs_guids_are_named_or_hidden_and_sizes_stay() {
        let mut n = names();
        n.insert("11427865429582413522", "sdk");
        n.insert("3847561029384756102", "tank");
        for (line, want) in [
            ("$ zpool replace tank 11427865429582413522 /dev/disk/by-id/wwn-0x5000c500a1b2c3d4", "$ zpool replace tank sdk /dev/sdb".to_string()),
            ("$ zpool detach tank 12345678901234567890", format!("$ zpool detach tank {H}")),
            ("$ zpool offline tank 98765432109876543", format!("$ zpool offline tank {H}")),
            ("$ zpool online tank 11427865429582413522", "$ zpool online tank sdk".to_string()),
            ("   pool: backup\n     id: 12156453278383891134", format!("   pool: backup\n     id: {H}")),
            ("     id: 3847561029384756102", "     id: tank".to_string()),
            ("tank 1234567890123456 used, 9007199254740992 bytes", "tank 1234567890123456 used, 9007199254740992 bytes".to_string()),
        ] {
            assert_eq!(n.scrub_with(line, NO_LINKS), want, "{line}");
        }
    }

    /// Critic wave 9a, MINOR 1/2: names people chose and plain numbers are
    /// never taken for ids.
    #[test]
    fn names_sizes_and_kernel_devices_stay() {
        let n = names();
        for line in [
            "zpool create ata-archive mirror sdb sdc",
            "share scsi-luns created",
            "dataset tank/sn-backups: created",
            "pool eui.lab imported",
            "ATA-8 device, sn-1 is short",
            "tank 1234567890123456 used",
            "range 1000-2000 and 20260926-01",
            "share dev-backups: created",
            "trim of nvme-cache finished",
            "virtio-disk is a name here",
            "$ /usr/sbin/zpool scrub tank",
            "wrote 4000787030016 bytes to /dev/sdd1",
            "dataset tank/2024: created",
            "channel: helper",
            "/dev/dm-0 and /dev/md0 are busy",
            "Samsung SSD 870 EVO 4TB, WDC WD40EFRX-68N32N0",
            "dataset tank/usb-backup_2024 and tank/ata-data_2025: created",
            "serial 0000 and bridge 1234",
            "⟦id⟧ stays as it is",
        ] {
            assert_eq!(n.scrub_with(line, NO_LINKS), line);
        }
    }

    /// Critic wave 9a, MINOR 10: through the REAL loader — a gone disk is its
    /// model under every by-id name it had, a known pool's GUID is its name,
    /// and the node's own names are kept.
    #[test]
    fn the_loader_names_a_gone_disk_by_its_model_and_keeps_the_nodes_names() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::super::db::migrate(&conn).unwrap();
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        super::super::db::upsert_disk_seen(
            &db,
            &super::super::db::DiskIdentity {
                disk_id: "wwn-0x5000c500deadbe01",
                name: "sdq",
                model: "ST8000VN004-2M2101",
                serial: "ZA1B2C3D",
                wwn: Some("0x5000c500deadbe01"),
                size_bytes: 8_000_000_000_000,
                kind: "hdd",
            },
        )
        .unwrap();
        super::super::db::remember_pools(&db, &HashMap::from([("ata-archive".to_string(), "11427865429582413522".to_string())])).unwrap();
        db.write().unwrap().execute(
            "INSERT INTO nas_shares (share_id, name, protocol, source_path, created_at, updated_at, org_id) \
             VALUES ('s1', 'scsi-luns', 'smb', '/tank/x', 'now', 'now', 'org-a'), \
                    ('s2', 't10.archive2026', 'nfs', '/tank/y', 'now', 'now', 'org-a')",
            [],
        ).unwrap();
        let n = LogNames::load(&db);
        assert_eq!(n.scrub_with("/dev/disk/by-id/ata-ST8000VN004-2M2101_ZA1B2C3D-part1", NO_LINKS), "ST8000VN004-2M2101");
        assert_eq!(n.scrub_with("/dev/disk/by-id/wwn-0x5000c500deadbe01", NO_LINKS), "ST8000VN004-2M2101");
        assert_eq!(n.scrub_with("vdev ata-ST8000VN004-2M2101_ZA1B2C3D FAULTED", NO_LINKS), "vdev ST8000VN004-2M2101 FAULTED");
        assert_eq!(n.scrub_with("pool 11427865429582413522 imported", NO_LINKS), "pool ata-archive imported");
        // A leaf zpool can only name by its GUID, as the pool view resolved it.
        super::super::pools::remember_leaf_guid("16051979283746501928", Some("sdk"));
        let n = LogNames::load(&db);
        assert_eq!(n.scrub_with("$ zpool replace tank 16051979283746501928 /dev/sdx", NO_LINKS), "$ zpool replace tank sdk /dev/sdx");
        assert_eq!(n.scrub_with("zpool destroy ata-archive; share scsi-luns", NO_LINKS), "zpool destroy ata-archive; share scsi-luns");
        // A name the node knows is kept even when it is shaped like an id.
        assert_eq!(n.scrub_with("share t10.archive2026: created", NO_LINKS), "share t10.archive2026: created");
        assert_eq!(LogNames::default().scrub_with("t10.archive2026", NO_LINKS), HIDDEN, "unknown, the same text is an id");
    }
}
