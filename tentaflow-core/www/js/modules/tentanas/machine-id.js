// ===== File: modules/tentanas/machine-id.js — the shapes of an identifier that must never be printed as text =====
//
// The owner's rule cuts both ways: a machine identifier is never visible
// text (a tooltip at most), and a real NAME is never hidden because it
// happens to look like one. Those two halves used to be a single regex
// (`MACHINE_ID`) applied everywhere, which satisfied the first half at the
// cost of the second: a share named `dev-backups`, a pool `usb-backup`, a
// target `pci-store` or a dataset `2024` would vanish from alert/job
// sublines as if they were ids, because the by-id disk-shape prefixes and
// the "all digits" rule are also things a human sometimes types as a name.
//
// So there are now two predicates, and every caller picks the one that
// matches what it actually knows about the value:
//
// - `isOpaqueId` — valid EVERYWHERE, because nothing shaped like this is
//   ever a name a human chose: a UUID, a long-enough all-digit run (a ZFS
//   device GUID is 15-20 digits; the bound here is 12 so it never mistakes
//   a year or a small counter for one), or a 64-hex node id.
// - `isDiskIdShape` — only valid where the value is KNOWN to name a disk
//   (a SMART job's subject, a disk alert's subject, a ZFS pool leaf's
//   name): the by-id / by-path prefixes `disks.rs` and `zfs.rs` build disk
//   ids and by-id leaf basenames from, plus everything `isOpaqueId` already
//   covers (a bare device GUID or partition UUID is also a disk id, just
//   without a prefix).
//
// Applying `isDiskIdShape` to a share/pool/target/dataset name would hide a
// real name that starts with one of these prefixes; applying it there was
// exactly last version's bug. Applying `isOpaqueId` to a disk subject is
// fine but incomplete — it would leave `wwn-…`/`ata-…`/etc. visible as text,
// which is the bug this file exists to prevent for disks. Callers:
//
// - `isUnresolvedLeafName` (pool-detail.js): a ZFS leaf name is only ever a
//   kernel name or a disk id/by-id basename, never a human-chosen name →
//   `isDiskIdShape`.
// - `alertSubjectName` (tentanas.js): the subject is a disk id only when
//   `subjectKind === 'disk'` → `isDiskIdShape` there, `isOpaqueId` for every
//   other kind (an approval's subject is a request UUID, which `isOpaqueId`
//   already catches; a pool/target/array/dataset subject is a name).
// - `jobSubject` (tasks.js): the subject is a disk id only for
//   `kind === 'smart_test'` (spawned on `disk_id`) → `isDiskIdShape` there,
//   `isOpaqueId` for every other kind (a scrub/snapshot/share job's subject
//   is a pool/dataset/share name).
// - `jobAuthor` (format.js): `startedBy` is always a user id or a system
//   token, never a name that could collide with a disk-id prefix →
//   `isOpaqueId`.
//
// Real kernel names must never match either predicate: `sdd`, `nvme0n1`,
// `dm-0` (a LUKS/LVM device — only the `dm-name-`/`dm-uuid-` LINKS are
// ids), `md0`, `mmcblk0`, `vda`, `xvda`, `loop0`.

const OPAQUE_ID = /^\d{12,}$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$|^[0-9a-f]{64}$/i;

const DISK_ID_PREFIX = /^(wwn-|sn-|dev-|eui\.|nqn\.|ata-|scsi-|nvme-|usb-|dm-name-|dm-uuid-|virtio-|mmc-|md-name-|md-uuid-|lvm-pv-uuid-|pci-)/i;

/** Whether `value` is an opaque id shape (UUID, long digit-run GUID, 64-hex
 *  node id) — never a name a human chose, so this is valid everywhere. */
export const isOpaqueId = (value) => OPAQUE_ID.test(String(value || '').trim());

/** Whether `value` is a disk id shape — a by-id/by-path prefix, or anything
 *  `isOpaqueId` already covers (a bare GUID/partition-UUID disk id). Only
 *  apply this where the value is known to name a disk: elsewhere a real
 *  name (a share, pool, target or dataset) can take the same shape. */
export const isDiskIdShape = (value) => {
  const v = String(value || '').trim();
  return DISK_ID_PREFIX.test(v) || OPAQUE_ID.test(v);
};

// ----- Ids inside a node's free text -----------------------------------------
//
// A node's own text — a helper's journal note, a failed step's error chain,
// the kernel's refusal — is shown in the "Treść węzła" section and as a
// tooltip, and nothing guarantees it names things by name: a transfer path
// carries `.tentanas-transfer-<operation uuid>-`, a by-id path a `wwn-…`, a
// forwarded error a 64-hex node id. `scrubIds` replaces every such id in
// the text with `placeholder`, or with the name `nameOf(id)` returns for it
// (a node id the fleet knows).
//
// Free text is not a value, so the two predicates above are applied per
// TOKEN, with the shapes that are unambiguous in prose:
// - a UUID anywhere, even inside a longer token (the transfer path);
// - a token that `isOpaqueId` calls an id, except a short all-digit run —
//   in prose a 12-15 digit number is a byte count (a 2 TB disk is 13
//   digits), so a digit run counts from 16 digits, as a ZFS GUID does;
// - a hex run of 32 or more (a node id, a hash);
// - a token with a by-id prefix that no prose word starts with. `dev-`,
//   `usb-`, `pci-`, `nvme-`, `virtio-` or `md-name-` also begin real names
//   (`dev-backups`, `nvme-cache`), so, as `isDiskIdShape` says, they are
//   taken for ids only where the text KNOWS it names a disk: right after
//   `/dev/disk/by-…/`.
// A kernel name (`sdd`, `nvme0n1`, `dm-0`) and a human name are never ids.

const EMBEDDED_UUID = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi;
const TEXT_TOKEN = /[^\s'"`„”«»()[\]{}<>,;|=]+/g;
// Prefixes that are an id whatever follows them.
const TEXT_ID_ALWAYS = /^(nvme-eui\.|nvme-uuid\.|dm-uuid-|md-uuid-|lvm-pv-uuid-)/i;
// Prefixes a name can start with too (a pool `ata-archive`, a share
// `scsi-luns`, a pool `eui.lab`): an id only with an id body behind them —
// hex for a WWN/EUI (critic wave 9a, MINOR 1), `<model>_<serial>` for a
// by-id disk name.
const TEXT_ID_HEX = /^(wwn-|eui\.|naa\.)(0x)?[0-9a-f]{16,}$/i;
// `<prefix><model>_<serial>`: the serial part has a digit and six or more
// characters (`ata-data_2025` is a dataset name, not a disk).
const TEXT_ID_BY_ID = /^(ata-|scsi-|sn-)[^_]+(_[^_]+)*_([A-Za-z0-9.-]*\d[A-Za-z0-9.-]*?)(-part\d+)?$|^scsi-[0-9a-f]{12,}$/i;
// TentaNas's own disk id for a disk with a serial and no WWN: `sn-<serial>`.
const TEXT_ID_OWN_SERIAL = /^sn-(?=[A-Za-z0-9._-]*\d)[A-Za-z0-9._-]{6,}$/i;
const BY_DISK_PATH = /^(\/dev\/disk\/by-[a-z]+\/)(.+)$/i;

function isByIdName(token) {
  const m = TEXT_ID_BY_ID.exec(token);
  return Boolean(m) && (m[3] === undefined || m[3].length >= 6);
}

// A decimal is an id from `digits[0]` to `digits[1]` digits.
function isTextId(token, digits) {
  if (/^\d+$/.test(token)) return token.length >= digits[0] && token.length <= digits[1];
  return isOpaqueId(token) || /^[0-9a-f]{32,}$/i.test(token)
    || TEXT_ID_ALWAYS.test(token) || TEXT_ID_HEX.test(token) || isByIdName(token) || TEXT_ID_OWN_SERIAL.test(token);
}

/** `text` with every id replaced by `placeholder` — or by `nameOf(id)` when
 *  that returns a name. See the section comment for what counts as an id.
 *  `guidDigits`: a plain decimal is an id only at a ZFS GUID's length, 17-20
 *  digits — for a job log, whose sizes and timestamps must stay readable
 *  (critic wave 9a, R2-BLOCKER 1); otherwise from 16 digits on. */
export function scrubIds(text, placeholder, nameOf = () => '', { guidDigits = false } = {}) {
  const digits = guidDigits ? [17, 20] : [16, Infinity];
  const shown = (id) => String(nameOf(id) || '').trim() || placeholder;
  return String(text ?? '')
    .replace(EMBEDDED_UUID, (id) => shown(id))
    .replace(TEXT_TOKEN, (token) => {
      const byPath = BY_DISK_PATH.exec(token);
      if (byPath) return byPath[1] + placeholder;
      // Path segments are judged one by one ("…/wwn-0x5…/part1").
      return token.split('/').map((segment) => {
        const bare = segment.replace(/[:.!?]+$/, '');
        return bare && isTextId(bare, digits) ? shown(bare) + segment.slice(bare.length) : segment;
      }).join('/');
    });
}
