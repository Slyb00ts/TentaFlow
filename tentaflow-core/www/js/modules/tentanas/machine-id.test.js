// =============================================================================
// File: modules/tentanas/machine-id.test.js
// Description: `isOpaqueId` and `isDiskIdShape` are the two homes for every
// identifier shape the owner's rule hides from the screen — but the rule
// cuts both ways, so which one a caller uses matters: `isOpaqueId` (UUIDs,
// long digit-run GUIDs, 64-hex node ids) is safe everywhere because nothing
// shaped like that is ever a name a human chose, while `isDiskIdShape` (the
// by-id/by-path prefixes, plus everything `isOpaqueId` covers) must only be
// applied where the value is known to name a disk — a share, pool, target or
// dataset can be named `dev-backups`, `usb-backup`, `pci-store` or `2024`,
// and `isDiskIdShape` would wrongly call every one of those an id.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { isOpaqueId, isDiskIdShape, scrubIds } = await import('./machine-id.js');

// Disk-id shapes: the by-id/by-path prefixes `disks.rs` and `zfs.rs` build
// disk ids and by-id ZFS leaf basenames from. Valid disk ids, but NOT opaque
// on their own — a caller that only knows the value might be a name (a pool,
// share, target, dataset) must not apply this rule to it.
const DISK_ID_SHAPES = [
  'wwn-0x5000c500a1b2c3d4',
  'sn-WD-WCC7K0000000',
  'dev-sdd',
  'eui.0000000000000001',
  'nqn.2014-08.org.nvmexpress:uuid:1234',
  'ata-WDC_WD40EFRX-68N32N0_WD-WCC7K0000000-part1',
  'scsi-35000c500a1b2c3d4',
  'nvme-eui.0000000000000001',
  'usb-WD_Elements_25A3-0:0',
  'dm-name-luks-root',
  'dm-uuid-CRYPT-LUKS2-abc',
  'virtio-serial-0',
  'mmc-SD16G_0xabcdef01',
  'md-name-orion:0',
  'md-uuid-abcdef01:23456789:abcdef01:23456789',
  'lvm-pv-uuid-AbCd1234EfGh5678IjKl9012MnOp',
  'pci-0000:00:1f.2-ata-1',
];

// Opaque ids: valid EVERYWHERE, because nothing shaped like this is ever a
// name a human chose.
const OPAQUE_IDS = [
  '3fa85f64-5717-4562-b3fc-2c963f66afa6', // UUID
  '4fa78b34-9e2a-4c1d-8f3a-1b2c3d4e5f6a', // bare partition-UUID leaf name (TrueNAS)
  '1283746501928374650', // 19-digit ZFS device GUID
  'a'.repeat(64), // 64-hex node id
];

// Names a human plausibly chose that happen to start with a disk-id prefix,
// or are all-digit, or are short — a pool/share/target/dataset with one of
// these names must stay visible wherever the caller does not know the value
// is a disk. `isDiskIdShape` still calls the by-id-shaped ones ids (that
// predicate is only safe in a disk context); `isOpaqueId` must reject all of
// them, since it is applied even where the value could be a name.
const PLAUSIBLE_NAMES = ['dev-backups', 'usb-backup', 'pci-store', '2024', 'sn-archive', 'mmc-media', '42'];

// Real kernel names the owner's rule must keep visible as the disk they are.
const KERNEL_NAMES = ['sdd', 'nvme0n1', 'dm-0', 'dm-12', 'md0', 'mmcblk0', 'vda', 'xvda', 'loop0'];

test('isDiskIdShape recognises every by-id/by-path prefix and every opaque shape', () => {
  for (const id of [...DISK_ID_SHAPES, ...OPAQUE_IDS]) {
    assert.equal(isDiskIdShape(id), true, `${id} should be a disk id shape`);
  }
});

test('isDiskIdShape leaves real kernel names visible', () => {
  for (const kernel of KERNEL_NAMES) {
    assert.equal(isDiskIdShape(kernel), false, `${kernel} is a real kernel name`);
  }
});

test('isOpaqueId is true only for UUIDs, long digit-run GUIDs and 64-hex ids', () => {
  for (const id of OPAQUE_IDS) {
    assert.equal(isOpaqueId(id), true, `${id} should be an opaque id`);
  }
});

// This is the false-positive the owner's rule now protects against in both
// directions: a by-id-shaped or short numeric NAME must not read as an id to
// a caller that only knows opaque ids can appear (job authors, and job/alert
// subjects for every kind that is not a disk).
test('isOpaqueId never mistakes a disk-id shape or a short/plausible name for an id', () => {
  for (const name of [...DISK_ID_SHAPES, ...PLAUSIBLE_NAMES, ...KERNEL_NAMES]) {
    assert.equal(isOpaqueId(name), false, `${name} must stay a name under the opaque rule`);
  }
});

test('a short digit run (a year, a small counter) is not an opaque id, but a long one is', () => {
  assert.equal(isOpaqueId('2024'), false);
  assert.equal(isOpaqueId('42'), false);
  assert.equal(isOpaqueId('123456789012'), true, '12 digits is the chosen floor');
  assert.equal(isOpaqueId('12345678901'), false, '11 digits stays below the floor');
});

test('empty, null and whitespace-only values are not ids under either rule', () => {
  for (const fn of [isOpaqueId, isDiskIdShape]) {
    assert.equal(fn(''), false);
    assert.equal(fn(null), false);
    assert.equal(fn(undefined), false);
    assert.equal(fn('   '), false);
  }
});

// Wave-4 round-2 critic minor 4: the "Treść węzła" section shows a node's
// own text, and nothing makes that text name things by name. `scrubIds`
// takes the ids out of it and leaves every name and number a reader needs.
test('scrubIds replaces the ids in a node\'s free text and keeps the names', () => {
  const P = '[id]';
  const uuid = '0191f2c0-4b1e-7c3a-9f2d-8ac41b5e9d70';
  const nodeId = '9f'.repeat(32);
  // A helper's transfer path carries the operation's uuid inside a token.
  assert.equal(
    scrubIds(`rename failed: /srv/tentanas/elastic/media/data/d1/.tentanas-transfer-${uuid}-3: No space left`, P),
    `rename failed: /srv/tentanas/elastic/media/data/d1/.tentanas-transfer-${P}-3: No space left`,
  );
  // A node id: its name when the caller knows one, the placeholder when not.
  assert.equal(scrubIds(`forward to ${nodeId} timed out`, P), `forward to ${P} timed out`);
  assert.equal(scrubIds(`forward to '${nodeId}' timed out`, P, (id) => (id === nodeId ? 'atlas' : '')), "forward to 'atlas' timed out");
  // Disk ids: a by-id path, a bare wwn with a trailing colon, a ZFS GUID.
  assert.equal(scrubIds('cannot open /dev/disk/by-id/usb-WD_Elements_25A3-0:0-part1', P), `cannot open /dev/disk/by-id/${P}`);
  assert.equal(scrubIds('wwn-0x5000c500a1b2c3d4: I/O error', P), `${P}: I/O error`);
  assert.equal(scrubIds('vdev 11805298034538519219 is UNAVAIL', P), `vdev ${P} is UNAVAIL`);
  // What stays: kernel names, human names in a disk-id shape, byte counts.
  const kept = 'sdd, nvme0n1, dm-0, share dev-backups, pool nvme-cache, target pci-store: need 2000398934016 bytes in 2024';
  assert.equal(scrubIds(kept, P), kept);
  assert.equal(scrubIds('', P), '');
  assert.equal(scrubIds(null, P), '');
});
