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
const TEXT_DISK_ID_PREFIX = /^(wwn-|sn-|eui\.|nvme-eui\.|nvme-uuid\.|ata-|scsi-|dm-uuid-|md-uuid-|lvm-pv-uuid-)/i;
const BY_DISK_PATH = /^(\/dev\/disk\/by-[a-z]+\/)(.+)$/i;

function isTextId(token) {
  if (/^\d+$/.test(token)) return token.length >= 16;
  return isOpaqueId(token) || /^[0-9a-f]{32,}$/i.test(token) || TEXT_DISK_ID_PREFIX.test(token);
}

/** `text` with every id replaced by `placeholder` — or by `nameOf(id)` when
 *  that returns a name. See the section comment for what counts as an id. */
export function scrubIds(text, placeholder, nameOf = () => '') {
  const shown = (id) => String(nameOf(id) || '').trim() || placeholder;
  return String(text ?? '')
    .replace(EMBEDDED_UUID, (id) => shown(id))
    .replace(TEXT_TOKEN, (token) => {
      const byPath = BY_DISK_PATH.exec(token);
      if (byPath) return byPath[1] + placeholder;
      // Path segments are judged one by one ("…/wwn-0x5…/part1").
      return token.split('/').map((segment) => {
        const bare = segment.replace(/[:.!?]+$/, '');
        return bare && isTextId(bare) ? shown(bare) + segment.slice(bare.length) : segment;
      }).join('/');
    });
}
