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
test('jobAuthor hides an opaque id (UUID, long GUID, 64-hex) behind "nieznane konto", with no tooltip', () => {
  const ids = ['3fa85f64-5717-4562-b3fc-2c963f66afa6', '1283746501928374650', 'a'.repeat(64)];
  for (const id of ids) {
    const author = jobAuthor(id);
    assert.deepEqual(author, { label: 'nieznane konto' }, `${id} must read as an unknown account and go nowhere else`);
  }
});

test('jobAuthor never hides a disk-id-shaped or short-digit display name', () => {
  for (const name of ['dev-backups', 'usb-backup', 'pci-store', '2024', 'sn-archive']) {
    assert.deepEqual(jobAuthor(name), { label: name }, `${name} is not an opaque id, so it stays the label`);
  }
});

test('jobAuthor keeps the system authors translated, with no tooltip', () => {
  assert.deepEqual(jobAuthor('scheduler'), { label: 'harmonogram' });
  assert.deepEqual(jobAuthor('startup'), { label: 'start noda' });
});

// ----- Health reasons worded from the node's codes ---------------------------

const { readFileSync } = await import('node:fs');
const { join } = await import('node:path');
const { WWW_ROOT, I18n } = await import('./_test-setup.js');
const {
  diskReasonWord, firstDiskReasonWord, diskReasonsText, diskHealthChipLabel, replacementAdviceText, poolReasonsText,
  DISK_SENTENCE_CODES, DISK_WORD_CODES, fmtCoarseWait, errCode,
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

// The n01/n02 disk alert and the replacement advice word the same codes as
// whole phrases (DISK_REASON_SENTENCES); a code with a chip word but no
// phrase would drop out of the alert's title.
test('every disk and advice code the node produces has a whole phrase in every locale', async () => {
  const codes = producedCodes('disks.rs');
  assert.deepEqual([...DISK_SENTENCE_CODES].sort(), [...DISK_WORD_CODES].sort(), 'the two dictionaries word the same codes');
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      for (const code of codes) {
        assert.ok(DISK_SENTENCE_CODES.includes(code), `${code} has a phrase`);
        const text = alertText(A('disk_health', { health: 'warning', name: 'sdd', name_source: 'live' }, { reasons: [R(code, ALL_PARAMS)] }));
        const phrase = code === 'reallocated_grew' || code === 'unhealthy_for_days'
          ? replacementAdviceText({ severity: 'advice', reasons: [R(code, ALL_PARAMS)] }).text
          : text.title;
        assert.ok(phrase, `${code} is worded in ${lang}`);
        assert.doesNotMatch(phrase, /tentanas\.|\{|\.\.$/, `${code} in ${lang} is a real phrase: ${phrase}`);
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
      reasons: [R('unhealthy_for_days', { health: 'warning', days: '5' }), R('temperature_high', { celsius: '54', limit: '50' }), R('crc_errors', { count: '1' })] },
    'Uwaga od 5 dni; temperatura 54°C (próg ostrzeżenia 50°C); 1 błąd CRC (kabel lub backplane)'],
    [{ severity: 'urgent', reason: 'reallocated sectors grew from 4 to 9 in the last 7 days; critical for 2 days; SMART overall status FAILED',
      reasons: [R('reallocated_grew', { from: '4', to: '9' }), R('unhealthy_for_days', { health: 'critical', days: '2' }), R('smart_failed')] },
    'realokacje wzrosły z 4 do 9 w 7 dni; Awaria od 2 dni; SMART zgłasza awarię dysku'],
    // The critic's "3 oczek. sekt.. Wymiana…": a phrase, no abbreviation.
    [{ severity: 'urgent', reason: 'critical for 3 days; 3 pending sectors',
      reasons: [R('unhealthy_for_days', { health: 'critical', days: '3' }), R('pending_sectors', { count: '3' })] },
    'Awaria od 3 dni; 3 sektory czekają na realokację'],
    // one whole day: the Polish singular form
    [{ severity: 'advice', reason: '87% worn', reasons: [R('unhealthy_for_days', { health: 'warning', days: '1' }), R('wear', { pct: '87' })] },
    'Uwaga od 1 dnia; zużycie 87%'],
  ];
  for (const [advice, text] of cases) {
    const out = replacementAdviceText(advice);
    assert.deepEqual(out, { known: true, text, title: advice.reason });
  }
  // Who the generic words are for — a pool disk or an array disk — is the
  // caller's to say (n03 card, n04 box): no text of its own.
  const generic = { known: false, text: '' };
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

// ----- Alerts worded from the node's codes -----------------------------------

const { alertText, ALERT_CODES } = await import('./format.js');

// Every alert code the node raises, read from the Rust raisers
// (`AlertText::new("…")`) — a code added there without words here fails
// this test instead of showing the generic "Alert węzła" on screen.
function raisedAlertCodes() {
  const codes = new Set();
  for (const file of ['disks.rs', 'elastic.rs', 'scheduler.rs', 'approvals.rs', 'targets.rs']) {
    const source = readFileSync(join(WWW_ROOT, '..', 'src', 'tentanas', file), 'utf8').split('mod tests {')[0];
    for (const m of source.matchAll(/AlertText::new\(\s*"(\w+)"/g)) codes.add(m[1]);
  }
  return [...codes];
}
// Every code NasAlert::code documents, the reserved ones included.
function documentedAlertCodes() {
  const source = readFileSync(join(WWW_ROOT, '..', '..', 'tentaflow-protocol', 'src', 'tentanas.rs'), 'utf8');
  const start = source.indexOf('pub struct NasAlert {');
  const doc = source.slice(start, source.indexOf('pub code: String', start));
  // One bullet per code: `/// - 'code' {params}`.
  return [...new Set([...doc.matchAll(/\/\/\/ - '([a-z_]+)' \{/g)].map((m) => m[1]))];
}
// Parameters every alert code may carry, all well-formed, so only the CODE decides.
const ALERT_PARAMS = {
  health: 'warning', name: 'sdq', name_source: 'live', operation: 'pool_destroy', subject: 'tank', array: 'media',
  runs: '4', oldest_secs: '32400', limit_secs: '28800', cause: 'files_busy', count: '2', alerted: '1',
  sweep_failed: 'true', error: 'EIO', target: 'vm-a',
};
const A = (code, params = ALERT_PARAMS, extra = {}) => ({
  alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'x', title: 'English title', detail: 'English detail', code, params, reasons: [], ...extra,
});

test('every alert code the node raises or documents has words in every locale', async () => {
  const raised = raisedAlertCodes();
  const documented = documentedAlertCodes();
  assert.ok(raised.length >= 11, `the scan found the raisers (${raised.join(', ')})`);
  assert.ok(documented.length >= 17, `the scan found the documented codes (${documented.join(', ')})`);
  for (const code of raised) assert.ok(documented.includes(code), `${code} is raised but not documented on NasAlert`);
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      for (const code of documented) {
        assert.ok(ALERT_CODES.includes(code), `${code} has a composer`);
        const text = alertText(A(code));
        assert.equal(text.known, true, `${code} is worded in ${lang}`);
        for (const part of [text.title, text.detail]) {
          assert.doesNotMatch(part, /tentanas\.|\{|English/, `${code} in ${lang} is a real sentence: ${part}`);
        }
        assert.ok(text.tooltip.startsWith('English title — English detail'), `the English is only the tooltip: ${text.tooltip}`);
      }
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});

test('a disk health alert is worded from its grade, its name and its reason codes', () => {
  const live = A('disk_health', { health: 'critical', name: 'sde', name_source: 'live' }, {
    title: 'Disk sde: critical', detail: 'ZFS reports this disk FAULTED; 3 reallocated sectors',
    reasons: [R('zfs_faulted'), R('reallocated', { count: '3' })],
  });
  // Wave-4 critic M4: the mockups' form (n01:298, n02:315) — the name and
  // the first reason as a phrase, the rest as the detail; the grade is the
  // row's severity chip, not a word of the title.
  assert.deepEqual(alertText(live), {
    title: 'sde: ZFS wyłączył dysk jako uszkodzony (FAULTED)',
    detail: '3 realokowane sektory',
    // No pool and no advice on the alert: the node sent neither.
    place: '',
    advice: '',
    copies: [],
    tooltip: 'Disk sde: critical — ZFS reports this disk FAULTED; 3 reallocated sectors',
    known: true,
    nodeText: false,
  });
  // With them, as disks.rs `HealthAlertPlace` writes them.
  const placed = alertText({ ...live, params: { ...live.params, pool: 'tank', layout: 'raidz2', advice: 'replace' } });
  assert.equal(placed.place, 'tank · RAIDZ2');
  assert.equal(placed.advice, 'zaplanuj wymianę dysku');
  assert.equal(alertText({ ...live, params: { ...live.params, advice: 'maybe' } }).advice, '', 'only the one advice the node sends');
  const growing = A('disk_health', { health: 'warning', name: 'sdd', name_source: 'live' }, {
    reasons: [R('reallocated_growing', { from: '5', to: '8' }), R('temperature_high', { celsius: '54', limit: '50' })],
  });
  assert.equal(alertText(growing).title, 'sdd: 3 nowe realokowane sektory w 7 dni', 'the mockup, word for word');
  assert.equal(alertText(growing).detail, 'temperatura 54°C (próg ostrzeżenia 50°C)');
  const hot = (limit) => alertText(A('disk_health', { health: 'warning', name: 'sdf', name_source: 'live' }, {
    reasons: [R('temperature_high', limit ? { celsius: '54', limit } : { celsius: '54' })],
  })).title;
  assert.equal(hot('50'), 'sdf: temperatura 54°C (próg ostrzeżenia 50°C)');
  assert.equal(hot(null), 'sdf: temperatura 54°C', 'a row stored before the threshold was sent');
  // Polish plural forms of the counted phrases.
  const count = (code, n) => alertText(A('disk_health', { health: 'warning', name: 'sdd', name_source: 'live' }, { reasons: [R(code, { count: String(n) })] })).title;
  assert.equal(count('reallocated', 1), 'sdd: 1 realokowany sektor');
  assert.equal(count('reallocated', 22), 'sdd: 22 realokowane sektory');
  assert.equal(count('reallocated', 12), 'sdd: 12 realokowanych sektorów');
  assert.equal(count('media_errors', 1), 'sdd: 1 błąd nośnika');
  assert.equal(count('pending_sectors', 5), 'sdd: 5 sektorów czeka na realokację');
  assert.doesNotMatch(alertText(live).title, /Awaria|Uwaga/, 'no disk-health grade word in the title');
  const lastKnown = A('disk_health', { health: 'warning', name: 'sdq', name_source: 'last_known' }, { reasons: [R('wear', { pct: '91' })] });
  assert.equal(alertText(lastKnown).title, 'Dysk ostatnio widziany jako sdq: zużycie 91%');
  assert.equal(alertText(A('disk_health', { health: 'warning', name_source: 'unknown' }, { reasons: [R('no_smart_data')] })).title, 'Dysk: brak danych SMART');
  // With no reason this build can word, the title falls back to the grade.
  assert.equal(alertText(A('disk_health', { health: 'warning', name: 'sdq', name_source: 'last_known' })).title, 'Dysk ostatnio widziany jako sdq: Uwaga');
  assert.equal(alertText(A('disk_health', { health: 'warning', name_source: 'unknown' })).title, 'Dysk: Uwaga');
  // A backfilled row (migration 21) has its grade and name, no reason codes:
  // the detail points at the node's text rather than printing the English,
  // and the row offers that text on touch too.
  const backfilled = alertText(A('disk_health', { health: 'warning', name: 'sdg', name_source: 'last_known' }, { detail: '8 reallocated sectors' }));
  assert.equal(backfilled.detail, 'Szczegóły są w treści węzła');
  assert.equal(backfilled.nodeText, true);
  assert.match(backfilled.tooltip, /8 reallocated sectors/);
  // A grade that is no alert grade, or a name that is missing, is no word.
  assert.equal(alertText(A('disk_health', { health: 'ok', name: 'sdq', name_source: 'live' })).known, false);
  assert.equal(alertText(A('disk_health', { health: 'warning', name_source: 'live' })).known, false);
});

// Wave-4 round-2 critic minor 4 and M-B: the tooltip and the "Treść węzła"
// section are the node's own text, so every id in it is taken out — the
// operation uuid in a helper's transfer path, a node id in a title or an
// error — and a node id the caller's fleet knows reads as the node's name.
test('the node text of an alert carries no id: a placeholder, or the node\'s name', () => {
  const uuid = '0191f2c0-4b1e-7c3a-9f2d-8ac41b5e9d70';
  const nodeId = '9f'.repeat(32);
  const attention = A('elastic_needs_attention', { array: 'media', helper_detail: `rename stuck on .tentanas-transfer-${uuid}-2` });
  const text = alertText(attention);
  assert.equal(text.nodeText, true);
  assert.doesNotMatch(text.tooltip, /0191f2c0|[0-9a-f]{32,}/i, text.tooltip);
  assert.match(text.tooltip, /\.tentanas-transfer-\[identyfikator\]-2/);

  // A parked import stored before the title was resolved (M-B), and a
  // forwarded error that names a node.
  const parked = A('approval_pending', { operation: 'config_import' }, {
    title: `a red-path operation on '${nodeId}' waits for a second admin`,
    detail: `overwrites 1: nightly (from ${nodeId})`,
  });
  assert.doesNotMatch(alertText(parked).tooltip, /[0-9a-f]{32,}/i);
  const named = alertText(parked, { nameOf: (id) => (id === nodeId ? 'atlas' : '') });
  assert.equal(named.tooltip, "a red-path operation on 'atlas' waits for a second admin — overwrites 1: nightly (from atlas)");
  // The generic fallback's tooltip goes through the same filter.
  const old = { alertId: 'a1', severity: 'warning', subjectKind: 'approval', subjectId: uuid, title: `request ${uuid} waits`, detail: '' };
  assert.equal(alertText(old).tooltip, 'request [identyfikator] waits');
});

test('an unknown code, an old uncoded row or broken parameters fall back to a translated generic alert', () => {
  const generic = { title: 'Alert węzła', detail: 'Pełny alert jest w treści węzła — ta wersja nie ma jego tłumaczenia', place: '', advice: '', copies: [], known: false, nodeText: true };
  // A row from an older node: no code, no params, no reasons at all.
  const old = { alertId: 'a1', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'media', title: 'Macierz wymaga interwencji', detail: 'x' };
  assert.deepEqual(alertText(old), { ...generic, tooltip: 'Macierz wymaga interwencji — x' });
  assert.deepEqual(alertText(A('pool_on_fire')), { ...generic, tooltip: 'English title — English detail' });
  assert.equal(alertText(A('constructor')).known, false, 'a prototype name is not a code');
  // A number that is not one, a cause this build does not know.
  assert.equal(alertText(A('elastic_mover_settle_stopped', { array: 'media', runs: 'many' })).known, false);
  assert.equal(alertText(A('elastic_cache_stuck', { ...ALERT_PARAMS, cause: 'gremlins' })).known, false);
  assert.equal(alertText(A('approval_pending', { subject: 'tank' })).known, false, 'no operation');
  // Only a title: no detail and no tooltip separator.
  assert.equal(alertText({ title: 'only title' }).tooltip, 'only title');
});

test('the Elastic and approval alerts word their parameters, never the node text', () => {
  const stuck = alertText(A('elastic_cache_stuck', { array: 'media', oldest_secs: '32400', limit_secs: '28800', cause: 'unresolved_operation' }));
  assert.equal(stuck.title, 'Pliki zbyt długo czekają na cache macierzy media');
  // Wave-4 critic minor 2: the node cut the wait to one unit, and only that
  // unit is worded — "9 h", not the "9 h 0 min" of a precision it threw away.
  assert.match(stuck.detail, /czeka na dysku cache 9 h \(alarm po 8 h\)/);
  assert.match(stuck.detail, /nierozwiązana operacja/);
  assert.equal(fmtCoarseWait(3 * 86400), '3 d');
  assert.equal(fmtCoarseWait(47 * 3600), '47 h', 'below two days the node cut to hours');
  assert.equal(fmtCoarseWait(3600), '1 h');
  assert.equal(fmtCoarseWait(50 * 60), '50 min');

  // Wave-4 critic M2: WHERE the other version is, as a place — never the
  // helper's quarantine name, which carries the operation's uuid.
  const uuid = '01a0cf8c-5a61-7283-8410-924a0fceb01f';
  const conflict = alertText(A('elastic_conflict', { array: 'media', count: '3' }, {
    detail: `docs/a.odt: … /mnt/tentanas-branches/media/cache/nvme2n1/.tentanas-quarantine-${uuid}-3`,
    reasons: [
      { code: 'conflict_file', params: { path: 'docs/a.odt', visible: '/mnt/media/docs/a.odt', kept_kind: 'quarantine', kept_disk: 'nvme2n1' } },
      { code: 'conflict_file', params: { path: 'foto/b.jpg', visible: '/mnt/media/foto/b.jpg', kept_kind: 'data', kept_disk: 'd2' } },
      { code: 'conflict_file', params: { path: 'c.txt', visible: '/mnt/media/c.txt', kept_kind: 'branch' } },
      { code: 'conflict_file', params: { path: 'd.txt', visible: '/mnt/media/d.txt', kept: `/x/.tentanas-quarantine-${uuid}-1` } },
      { code: 'mystery_line', params: {} },
    ],
  }));
  assert.equal(conflict.title, 'Macierz media: 3 pliki zachowane w dwóch wersjach');
  assert.match(conflict.detail, /docs\/a\.odt: wersja widoczna \/mnt\/media\/docs\/a\.odt, druga — kopia w kwarantannie na dysku cache nvme2n1/);
  assert.match(conflict.detail, /foto\/b\.jpg: wersja widoczna \/mnt\/media\/foto\/b\.jpg, druga pod tą samą ścieżką na dysku danych d2/);
  assert.match(conflict.detail, /c\.txt: wersja widoczna \/mnt\/media\/c\.txt, druga na jednym z dysków macierzy$/);
  assert.doesNotMatch(conflict.detail, /quarantine-|d\.txt/, 'an old-shape line (a raw `kept` path) is left out, not printed');
  assert.ok(!`${conflict.title} ${conflict.detail}`.includes(uuid), 'no uuid on screen');
  assert.equal(alertText(A('elastic_conflict', { array: 'media', count: '5' })).title, 'Macierz media: 5 plików zachowanych w dwóch wersjach');

  const approval = alertText(A('approval_pending', { operation: 'pool_destroy', subject: 'tank' }));
  assert.equal(approval.title, 'Zniszczenie puli „tank” czeka na drugiego administratora');
  // Wave-4 critic minor 12: a config import whose node the fleet cannot name
  // arrives with no subject (dispatch `config_import_subject`) and names the
  // operation alone — never the node's id.
  const unnamed = alertText(A('approval_pending', { operation: 'config_import' }));
  assert.equal(unnamed.known, true);
  assert.equal(unnamed.title, "Import konfiguracji (nadpisujący) czeka na drugiego administratora");
  // An operation this build has no label for reads as the queue's generic one.
  assert.match(alertText(A('approval_pending', { operation: 'warp_drive', subject: 'tank' })).title, /^Operacja „tank”/);

  // Wave-4 critic M3: the node's free text — the helper's note, a step's
  // error, the kernel's refusal — is never the detail line. The line is a
  // translated sentence; the raw text is the tooltip (and the touch text).
  const raws = [
    ['elastic_needs_attention', { array: 'media', helper_detail: 'nazwa tymczasowa bez prefiksu transferu' }, 'nazwa tymczasowa bez prefiksu transferu'],
    ['elastic_result_unconfirmed', { array: 'media', error: 'Helper Elastic zwrócił błąd 1' }, 'Helper Elastic zwrócił błąd 1'],
    ['elastic_replace_unconfirmed', { array: 'media', error: 'service: EIO' }, 'service: EIO'],
    ['elastic_add_disk_unconfirmed', { array: 'media', error: 'EIO on sdq' }, 'EIO on sdq'],
    ['target_not_applied', { target: 'vm-a', error: 'configfs: Invalid argument' }, 'configfs: Invalid argument'],
    ['target_still_in_kernel', { target: 'vm-a', error: 'Device or resource busy' }, 'Device or resource busy'],
  ];
  for (const [code, params, raw] of raws) {
    const text = alertText(A(code, params));
    assert.equal(text.known, true, code);
    assert.ok(!text.detail.includes(raw) && !text.title.includes(raw), `${code}: the raw text is not the line (${text.detail})`);
    assert.ok(text.tooltip.includes(raw), `${code}: the raw text is the tooltip (${text.tooltip})`);
    assert.equal(text.nodeText, true, `${code}: and reachable on touch`);
    assert.doesNotMatch(text.detail, /\{|tentanas\./, code);
  }
  assert.equal(alertText(A('elastic_needs_attention', { array: 'media', helper_detail: 'x' })).detail, 'Macierz wymaga interwencji; rezerwacje zachowane. Opis helpera jest w treści węzła');
  assert.equal(alertText(A('elastic_needs_attention', { array: 'media' })).detail, 'Macierz wymaga interwencji; rezerwacje zachowane');
  assert.equal(alertText(A('elastic_needs_attention', { array: 'media' })).nodeText, false);
  // A raw text the node's own detail already carries is not said twice.
  assert.equal(alertText(A('target_not_applied', { target: 'vm-a', error: 'busy' }, { detail: 'the kernel refused: busy' })).tooltip, 'English title — the kernel refused: busy');

  // The node sweep alert says who can read the names only as far as it holds.
  const sweep = (alerted) => alertText(A('targets_sweep_failing', { count: '3', alerted: String(alerted), sweep_failed: 'false' })).detail;
  assert.match(sweep(3), /organizacja każdego z nich ma alert/);
  assert.match(sweep(1), /1 z nich ma alert/);
  const sweepOf = (count, alerted) => alertText(A('targets_sweep_failing', { count: String(count), alerted: String(alerted), sweep_failed: 'false' })).detail;
  assert.match(sweepOf(5, 2), /2 z nich mają alert/, 'Polish plural');
  assert.match(sweepOf(9, 5), /5 z nich ma alert/);
  assert.match(sweep(0), /log węzła je nazywa$/);
  assert.match(alertText(A('target_not_applied', { target: 'vm-a' })).detail, /pełny błąd jest w logu węzła/);
});

// ----- Refusals the node sends as codes ------------------------------------

const { errMessage } = await import('./format.js');

test('every refusal code the node sends is worded in every locale, an unknown one is shown as sent', async () => {
  const codes = new Set();
  for (const file of ['tentanas/db.rs', 'tentanas/approvals.rs', 'tentanas/jobs.rs', 'tentanas/elastic.rs', 'dispatch/tentanas.rs']) {
    const source = readFileSync(join(WWW_ROOT, '..', 'src', file), 'utf8');
    for (const m of source.matchAll(/"refusal:([a-z0-9_]+)"/g)) codes.add(m[1]);
  }
  assert.ok(codes.has('elastic_one_cache_disk') && codes.has('pool_detach_not_allowed'), 'the dispatcher\'s own refusals are scanned too');
  assert.ok(codes.has('share_user_in_use_elsewhere') && codes.has('approval_own_request'), `the scan found the codes (${[...codes].join(', ')})`);
  assert.ok(codes.size >= 8, [...codes].join(', '));
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      for (const code of codes) {
        const text = errMessage(new Error(`refusal:${code}`));
        assert.doesNotMatch(text, /refusal:|tentanas\./, `${code} is worded in ${lang}: ${text}`);
      }
    }
  } finally {
    await I18n.setLanguage('pl');
  }
  assert.equal(errMessage(new Error('refusal:approval_own_request')), 'Autor zgłoszenia nie może go zatwierdzić — potrzebny jest drugi administrator');
  // A code this build does not know, and a plain message, pass through as sent.
  assert.equal(errMessage(new Error('refusal:quota_exceeded')), 'refusal:quota_exceeded');
  assert.equal(errMessage(new Error('Macierz nie istnieje w tej instancji')), 'Macierz nie istnieje w tej instancji');
  // A lost node is worded whatever the forwarder wrote, and no 64-hex id is
  // ever passed through (critic wave 7, BLOCKER 1).
  const id = 'b'.repeat(64);
  assert.equal(errMessage(new Error(`protocol error NodeUnreachable: node '${id}' did not answer: timeout`)), 'Węzeł nie odpowiada — utracono połączenie przez mesh');
  assert.equal(errMessage(Object.assign(new Error(`node '${id}' did not answer`), { code: 'NodeUnreachable' })), 'Węzeł nie odpowiada — utracono połączenie przez mesh');
  const other = errMessage(new Error(`protocol error Internal: peer '${id}' closed the stream`));
  assert.ok(!other.includes(id), other);
  assert.match(other, /closed the stream/);
  // Where the word-by-word scrubber sees no id (glued to other text), the hex
  // backstop still takes it out.
  for (const glued of [`peer_${id} gone`, `${id}—gone`]) {
    assert.ok(!errMessage(new Error(glued)).includes(id), glued);
  }
  assert.equal(errMessage(new Error(`peer '${id}' closed`), (x) => (x === id ? 'atlas' : '')), "peer 'atlas' closed", 'a known node reads as its name');
  assert.equal(errMessage('mesh timeout'), 'mesh timeout');
  // A refusal code inside a longer message is not a refusal code — but the
  // client's own `protocol error <Code>: ` wrapping is not "a longer message"
  // (the real wrapped error is produced by the client itself in
  // refusal-wire.test.js).
  assert.equal(errMessage(new Error('failed: refusal:approval_expired')), 'failed: refusal:approval_expired');
});

test('errCode reads the wire enum from the client wrapping or from .code', () => {
  assert.equal(errCode(new Error('protocol error NotFound: target not found on this node')), 'NotFound');
  assert.equal(errCode(Object.assign(new Error('x'), { code: 'Conflict' })), 'Conflict');
  assert.equal(errCode(new Error('mesh timeout')), '');
  assert.equal(errCode('protocol error PolicyDenied: refusal:approval_own_request'), 'PolicyDenied');
});

test('the sweep alert counts in English agree with their verb', async () => {
  try {
    await I18n.setLanguage('en');
    const sweep = (alerted) => alertText(A('targets_sweep_failing', { count: '3', alerted: String(alerted), sweep_failed: 'false' })).detail;
    assert.match(sweep(1), /1 of them has an alert/);
    assert.match(sweep(2), /2 of them have an alert/);
  } finally {
    await I18n.setLanguage('pl');
  }
});

// Minor 8 of the release review: the held Sync alert says WHICH fault holds
// it — counted scrub errors, or a fault the helper records with no counts (a
// scrub whose log broke, a failed repair) — in every locale; a row raised
// before `cause` existed reads the scrub-errors text it always had.
test('the held Sync alert words its cause in every locale', async () => {
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      const errors = alertText(A('elastic_sync_held', { array: 'media', cause: 'scrub_errors' }));
      const fault = alertText(A('elastic_sync_held', { array: 'media', cause: 'parity_fault' }));
      const legacy = alertText(A('elastic_sync_held', { array: 'media' }));
      assert.notEqual(fault.detail, errors.detail, lang);
      assert.equal(legacy.detail, errors.detail, lang);
      assert.doesNotMatch(fault.detail, /tentanas\.|\{/, lang);
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});
