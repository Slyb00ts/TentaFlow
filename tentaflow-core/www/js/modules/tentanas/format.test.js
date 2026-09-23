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

// ----- Health reasons worded from the node's codes ---------------------------

const { readFileSync } = await import('node:fs');
const { join } = await import('node:path');
const { WWW_ROOT, I18n } = await import('./_test-setup.js');
const {
  diskReasonWord, firstDiskReasonWord, diskReasonsText, diskHealthChipLabel, replacementAdviceText, poolReasonsText,
} = await import('./format.js');

function R(code, params = {}) {
  return { code, params };
}

// Every code the node can produce, read from the Rust source that produces
// it — `coded_reason("…")` in tentanas/disks.rs (disk and advice codes) and
// tentanas/pools.rs (pool codes, plus the per-kind scan codes picked by a
// `match`). A code added there without a word here fails this test instead
// of silently showing the bare grade on screen.
function producedCodes(file) {
  const source = readFileSync(join(WWW_ROOT, '..', 'src', 'tentanas', file), 'utf8');
  const codes = new Set([...source.matchAll(/coded_reason\(\s*"(\w+)"/g)].map((m) => m[1]));
  for (const m of source.matchAll(/=> "(\w+_found_errors)"/g)) codes.add(m[1]);
  return [...codes];
}
// Parameters every code may carry, all well-formed, so only the CODE decides.
const ALL_PARAMS = { count: '2', from: '1', to: '3', celsius: '61', limit: '60', pct: '91', days: '2', health: 'warning', state: 'degraded', kind: 'scrub', detail: 'x' };

test('every disk and advice code the node produces has words in every locale', async () => {
  const codes = producedCodes('disks.rs');
  assert.ok(codes.length >= 15, `the scan found the codes (${codes.join(', ')})`);
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      for (const code of codes) {
        const word = diskReasonWord(R(code, ALL_PARAMS));
        assert.ok(word, `${code} has a word in ${lang}`);
        assert.doesNotMatch(word, /tentanas\.|\{/, `${code} in ${lang} is a real sentence: ${word}`);
      }
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});

test('every pool code the node produces has words in every locale', async () => {
  const codes = producedCodes('pools.rs');
  assert.ok(codes.length >= 10, `the scan found the codes (${codes.join(', ')})`);
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      const grade = I18n.t('tentanas.health.warning');
      for (const code of codes) {
        const { text } = poolReasonsText({ health: 'warning', healthReason: 'English', healthReasons: [R(code, ALL_PARAMS)] });
        assert.notEqual(text, grade, `${code} has a word in ${lang}`);
        assert.doesNotMatch(text, /tentanas\.|\{|English/, `${code} in ${lang} is a real sentence: ${text}`);
      }
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});

test('a disk reason is worded from its code and parameters, never from the English', () => {
  const disk = {
    health: 'critical',
    healthReason: 'ZFS reports this disk FAULTED; 61°C (over the 60°C limit); 3 reallocated sectors',
    healthReasons: [R('zfs_faulted'), R('temperature_over_limit', { celsius: '61', limit: '60' }), R('reallocated', { count: '3' })],
  };
  assert.deepEqual(diskReasonsText(disk), {
    text: 'ZFS: awaria (FAULTED); 61°C, ponad limit 60°C; 3 realok.',
    title: disk.healthReason,
  });
  assert.equal(firstDiskReasonWord(disk), 'ZFS: awaria (FAULTED)', 'the chip takes the FIRST reason');
  assert.deepEqual(diskHealthChipLabel(disk), { label: 'Awaria: ZFS: awaria (FAULTED)', title: disk.healthReason });
  assert.equal(diskReasonWord(R('temperature_high', { celsius: '54' })), '54°C');
  assert.equal(diskReasonWord(R('reallocated_growing', { from: '0', to: '3' })), 'realok. 0 → 3');
});

test('an unknown code or a malformed parameter falls back to the grade, with the English only as the title', () => {
  const unknown = { health: 'warning', healthReason: 'spindle motor stalled', healthReasons: [R('spindle_stall')] };
  assert.deepEqual(diskReasonsText(unknown), { text: 'Uwaga', title: 'spindle motor stalled' });
  assert.equal(firstDiskReasonWord(unknown), null);
  assert.deepEqual(diskHealthChipLabel(unknown), { label: 'Uwaga', title: 'spindle motor stalled' });
  // A number that is not one is no word: "NaN realok." is no better than English.
  assert.equal(diskReasonWord(R('reallocated', { count: 'many' })), null);
  assert.equal(diskReasonWord(R('reallocated')), null);
  // A prototype name is not a code.
  assert.equal(diskReasonWord(R('constructor')), null);
  // A known reason after an unknown one is still said; the unknown one is not.
  const mixed = { health: 'warning', healthReason: 'x; 1 media errors', healthReasons: [R('spindle_stall'), R('media_errors', { count: '1' })] };
  assert.equal(diskReasonsText(mixed).text, '1 bł. nośnika');
  assert.equal(firstDiskReasonWord(mixed), null, 'the chip does not skip to a lesser reason');
  // An older node sends the sentence and no codes: the grade, sentence as title.
  assert.deepEqual(diskReasonsText({ health: 'warning', healthReason: '3 reallocated sectors' }), { text: 'Uwaga', title: '3 reallocated sectors' });
  // No reason at all: nothing to say.
  assert.deepEqual(diskReasonsText({ health: 'ok', healthReason: '', healthReasons: [] }), { text: '', title: '' });
});

// `replacement_advice` (tentanas/disks.rs) sends the growth, the days the
// disk has been unhealthy and the disk's own reasons as codes; the English
// `reason` is only the tooltip.
test('replacementAdviceText words the advice from its codes', () => {
  const cases = [
    [{ severity: 'urgent', reason: 'reallocated sectors grew from 3 to 8 in the last 7 days; reallocated sectors growing (3 → 8 in 7 days)',
      reasons: [R('reallocated_grew', { from: '3', to: '8' })] },
    'realokacje wzrosły z 3 do 8 w 7 dni'],
    [{ severity: 'advice', reason: 'warning for 5 days; 54°C; 1 UDMA CRC errors (cable/backplane)',
      reasons: [R('unhealthy_for_days', { health: 'warning', days: '5' }), R('temperature_high', { celsius: '54' }), R('crc_errors', { count: '1' })] },
    'Uwaga od 5 dni; 54°C; 1 CRC'],
    [{ severity: 'urgent', reason: 'reallocated sectors grew from 4 to 9 in the last 7 days; critical for 2 days; SMART overall status FAILED',
      reasons: [R('reallocated_grew', { from: '4', to: '9' }), R('unhealthy_for_days', { health: 'critical', days: '2' }), R('smart_failed')] },
    'realokacje wzrosły z 4 do 9 w 7 dni; Awaria od 2 dni; SMART: awaria'],
    // one whole day: the Polish singular form
    [{ severity: 'advice', reason: '87% worn', reasons: [R('unhealthy_for_days', { health: 'warning', days: '1' }), R('wear', { pct: '87' })] },
    'Uwaga od 1 dnia; zużycie 87%'],
  ];
  for (const [advice, text] of cases) {
    const out = replacementAdviceText(advice);
    assert.deepEqual(out, { known: true, text, title: advice.reason });
  }
  const generic = { known: false, text: 'węzeł zaleca wymianę tego dysku' };
  // An advice kind this build does not know.
  assert.deepEqual(replacementAdviceText({ severity: 'retire_soon', reason: 'firmware recall', reasons: [R('smart_failed')] }), { ...generic, title: 'firmware recall' });
  // An older node: no codes at all.
  assert.deepEqual(replacementAdviceText({ severity: 'urgent', reason: 'critical for 3 days' }), { ...generic, title: 'critical for 3 days' });
  // Codes this build has no word for — including a grade that is no grade.
  assert.deepEqual(
    replacementAdviceText({ severity: 'advice', reason: 'x', reasons: [R('spindle_stall'), R('unhealthy_for_days', { health: 'ok', days: '3' })] }),
    { ...generic, title: 'x' },
  );
});

test('poolReasonsText words the pool card reason from its codes', () => {
  const pool = {
    health: 'critical',
    healthReason: 'pool is faulted; 7 permanent data errors; 1 unusable disks; 93% full',
    healthReasons: [R('pool_state', { state: 'faulted' }), R('permanent_data_errors', { count: '7' }), R('unusable_disks', { count: '1' }), R('capacity', { pct: '93' })],
  };
  assert.deepEqual(poolReasonsText(pool), {
    text: 'stan puli: Uszkodzona; 7 trwałych błędów danych; 1 dysk nie do użycia; zapełniona w 93%',
    title: pool.healthReason,
  });
  // zpool's own words for uncounted errors never reach the text.
  const odd = { health: 'critical', healthReason: 'list of errors unavailable', healthReasons: [R('data_errors_reported', { detail: 'list of errors unavailable' })] };
  assert.deepEqual(poolReasonsText(odd), { text: 'ZFS zgłasza błędy danych', title: 'list of errors unavailable' });
  // A state word this build has no label for is not printed raw.
  assert.equal(poolReasonsText({ health: 'warning', healthReason: 'pool is suspended', healthReasons: [R('pool_state', { state: 'suspended' })] }).text, 'Uwaga');
  // Singular / few / many in Polish.
  const scrub = (n) => poolReasonsText({ health: 'warning', healthReasons: [R('scrub_found_errors', { count: String(n), kind: 'scrub' })] }).text;
  assert.equal(scrub(1), 'ostatni scrub znalazł 1 błąd');
  assert.equal(scrub(3), 'ostatni scrub znalazł 3 błędy');
  assert.equal(scrub(12), 'ostatni scrub znalazł 12 błędów');
  // A healthy pool has nothing to say.
  assert.deepEqual(poolReasonsText({ health: 'ok', healthReason: '', healthReasons: [] }), { text: '', title: '' });
});
