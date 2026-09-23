// =============================================================================
// File: modules/tentanas/format.test.js
// Description: The per-disk batch helper shared by "SMART all disks"
// (tasks.js) and "SMART selected" (tentanas.js): `runDiskBatch` keeps going
// past a disk-specific refusal but stops the whole batch at once on a
// privilege/credential error (`ProtocolErrorCode::NotAvailable`, the code
// `broker_error` in dispatch/tentanas.rs gives a rejected sudo password, an
// unarmed channel or a helper/core version mismatch), plus `refusedBatchNames`
// naming the disks a batch refused; and `replacementAdviceText`, the
// replacement advice rebuilt in the reader's language.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { isBatchHaltError, runDiskBatch, refusedBatchNames, jobAuthor } = await import('./format.js');

const disk = (diskId) => ({ diskId, name: diskId });

// A rejected/expired sudo password and an unarmed channel are both
// `BrokerError::Unarmed`; a helper/core version mismatch is
// `BrokerError::HelperVersion`. `broker_error` maps every one of them to
// `ProtocolErrorCode::NotAvailable`, and `api-binary-shim.js` copies that
// code onto the thrown Error as `.code` — this is the one marker the front
// classifies on, never the (partly Polish) message text.
const credentialError = (message = 'sudo rejected the password') => Object.assign(new Error(message), { code: 'NotAvailable' });
// A per-disk refusal the node reports with no such code (e.g. the disk
// vanished from the live inventory: `ProtocolErrorCode::NotFound`), or a
// bare Error the way the disk-busy fixtures in tasks.test.js / tentanas.test.js
// use it.
const perDiskError = (message = 'dysk zajęty') => new Error(message);

test('isBatchHaltError is true only for the NotAvailable code, never guessed from text', () => {
  assert.equal(isBatchHaltError(credentialError()), true);
  assert.equal(isBatchHaltError(perDiskError('unarmed privilege channel')), false, 'text alone is never enough');
  assert.equal(isBatchHaltError(perDiskError()), false);
  assert.equal(isBatchHaltError(Object.assign(new Error('disk not found'), { code: 'NotFound' })), false);
  assert.equal(isBatchHaltError(null), false);
  assert.equal(isBatchHaltError(undefined), false);
});

test('runDiskBatch tries every disk when refusals are per-disk', async () => {
  const attempted = [];
  const { started, refused } = await runDiskBatch([disk('sda'), disk('sdb'), disk('sdc')], async (d) => {
    attempted.push(d.diskId);
    if (d.diskId === 'sdb') throw perDiskError('dysk zajęty');
  });
  assert.deepEqual(attempted, ['sda', 'sdb', 'sdc'], 'sdc is still tried after sdb refuses');
  assert.deepEqual(started.map((d) => d.diskId), ['sda', 'sdc']);
  assert.equal(refused.length, 1);
  assert.equal(refused[0].disk.diskId, 'sdb');
  assert.equal(refused[0].error.message, 'dysk zajęty');
});

test('runDiskBatch stops at the first privilege/credential error and sends no further request', async () => {
  const attempted = [];
  const halt = credentialError('sudo rejected the password');
  await assert.rejects(
    runDiskBatch([disk('sda'), disk('sdb'), disk('sdc')], async (d) => {
      attempted.push(d.diskId);
      if (d.diskId === 'sda') throw halt;
    }),
    (e) => e === halt,
  );
  assert.deepEqual(attempted, ['sda'], 'exactly one request — sdb and sdc are never tried');
});

test('refusedBatchNames names the disks, never a disk id', () => {
  const text = refusedBatchNames([
    { disk: { diskId: 'wwn-0x5000c500a1b2c3d4', name: 'sdb' }, error: perDiskError('dysk zajęty') },
    { disk: { diskId: 'wwn-0x5000c500a1b2c3d5', name: 'sdc' }, error: perDiskError('test już trwa') },
  ]);
  assert.equal(text, 'sdb: dysk zajęty · sdc: test już trwa');
  assert.doesNotMatch(text, /wwn-/, 'no disk id leaks into the toast');
});

// `startedBy` is always a user id or a system token, never a name that could
// collide with a disk-id prefix, so `jobAuthor` uses the opaque rule alone
// (never the disk rule) — an unresolved account's UUID is hidden, but a
// display name shaped like a disk id (this can't happen for a real account,
// but the routing must not accidentally hide one if it ever did) stays text.
test('jobAuthor hides an opaque id (UUID, long GUID, 64-hex) behind "nieznane konto"', () => {
  const ids = ['3fa85f64-5717-4562-b3fc-2c963f66afa6', '1283746501928374650', 'a'.repeat(64)];
  for (const id of ids) {
    const { label, title } = jobAuthor(id);
    assert.equal(label, 'nieznane konto', `${id} must read as an unknown account`);
    assert.equal(title, id);
  }
});

test('jobAuthor never hides a disk-id-shaped or short-digit display name', () => {
  for (const name of ['dev-backups', 'usb-backup', 'pci-store', '2024', 'sn-archive']) {
    const { label, title } = jobAuthor(name);
    assert.equal(label, name, `${name} is not an opaque id, so it stays the label`);
    assert.equal(title, '');
  }
});

test('jobAuthor keeps the system authors translated, with no tooltip', () => {
  assert.equal(jobAuthor('scheduler').label, 'harmonogram');
  assert.equal(jobAuthor('scheduler').title, '');
  assert.equal(jobAuthor('startup').title, '');
});

// `replacement_advice` (tentanas/disks.rs) joins at most three parts with
// "; ": the growth sentence, "{health} for {days} days" and the disk's whole
// health reason. Every combination it can write is rebuilt from fields, and
// none of its English survives into the text.
test('replacementAdviceText rebuilds every part of the node\'s advice sentence', async () => {
  const { replacementAdviceText } = await import('./format.js');
  const cases = [
    // growth only (fewer than ADVICE_AFTER_DAYS days, reason repeats the growth)
    [{ severity: 'urgent', reallocated: 8, reallocatedWeekAgo: 3, warningDays: 0,
      reason: 'reallocated sectors grew from 3 to 8 in the last 7 days; reallocated sectors growing (3 → 8 in 7 days)' },
    { health: 'warning', healthReason: 'reallocated sectors growing (3 → 8 in 7 days)' },
    'realokacje wzrosły z 3 do 8 w 7 dni'],
    // days + reason, a warning disk (the `advice` kind)
    [{ severity: 'advice', reallocated: 3, reallocatedWeekAgo: 3, warningDays: 5, reason: 'warning for 5 days; 54°C; 1 UDMA CRC errors (cable/backplane)' },
      { health: 'warning', healthReason: '54°C; 1 UDMA CRC errors (cable/backplane)' },
      'Uwaga od 5 dni; 54°C; 1 CRC'],
    // all three parts, a critical disk
    [{ severity: 'urgent', reallocated: 9, reallocatedWeekAgo: 4, warningDays: 2, reason: 'reallocated sectors grew from 4 to 9 in the last 7 days; critical for 2 days; SMART overall status FAILED' },
      { health: 'critical', healthReason: 'SMART overall status FAILED' },
      'realokacje wzrosły z 4 do 9 w 7 dni; Awaria od 2 dni; SMART: awaria'],
    // one whole day: the Polish singular form, and no counter at all
    [{ severity: 'advice', reallocated: null, reallocatedWeekAgo: null, warningDays: 1, reason: 'warning for 1 days; 87% worn' },
      { health: 'warning', healthReason: '87% worn' },
      'Uwaga od 1 dnia; zużycie 87%'],
    // the disk's reason has nothing this build can name: nothing English leaks
    [{ severity: 'advice', reallocated: 0, reallocatedWeekAgo: 0, warningDays: 0, reason: 'spindle motor stalled' },
      { health: 'warning', healthReason: 'spindle motor stalled' },
      'Uwaga'],
  ];
  for (const [advice, disk, text] of cases) {
    const out = replacementAdviceText(advice, disk);
    assert.equal(out.text, text, advice.reason);
    assert.equal(out.title, advice.reason, 'the node\'s sentence is the tooltip');
    assert.equal(out.known, true);
  }
  const unknown = replacementAdviceText({ severity: 'retire_soon', reason: 'firmware recall' }, { health: 'warning', healthReason: '' });
  assert.deepEqual(unknown, { known: false, text: 'węzeł zaleca wymianę tego dysku', title: 'firmware recall' });
  const noDisk = replacementAdviceText({ severity: 'urgent', reason: 'critical for 3 days' }, undefined);
  assert.equal(noDisk.known, false, 'without the disk there is no reason to translate');
  assert.equal(noDisk.text, 'węzeł zaleca wymianę tego dysku');
});
