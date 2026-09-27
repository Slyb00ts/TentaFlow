// =============================================================================
// File: modules/tentabus/topic-settings.test.js
// Description: A topic's Ustawienia section (U2): the values the page reads
// out, with locks where the page does not change them; who may change the
// topic; the "Co się stanie po zapisaniu" sentences of the four windows,
// computed from what the server does with each change; the update that sends
// only what a window changed; the windows themselves (a save, a field that
// is not valid, a refusal that stays in the window).
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  retentionChoices, limitChoices, backoffChoices, quorumOf, backoffSchedule, storageFacts,
  retentionImpact, writeImpact, retryImpact, patternImpact, acksHint,
  buildTopicUpdateRequest, whoCanChange, settingsHtml, openSettingsWindow, partitionCount, attemptCount,
} = await import('./topic-settings.js');

const DAY = 86_400_000;
const GIB = 1024 ** 3;
const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 0));

const topic = {
  name: 'wyniki-badan',
  partitions: 6,
  retentionMs: 30 * DAY,
  retentionBytesPerPartition: 8 * GIB,
  replicationFactor: 3,
  acks: 'quorum',
  durabilityClass: 'standard',
  compression: 'lz4',
  maxDeliveryAttempts: 5,
  retryBackoffMs: 2000,
  contentType: 'application/json',
  schemaId: 'wizyta',
  validation: 'dlq',
};
const subjects = [
  { subject: 'wizyta', schemaType: 'json_schema', latestVersion: 3, deprecatedAtMs: null },
  { subject: 'wizyta-2025', schemaType: 'json_schema', latestVersion: 1, deprecatedAtMs: 1 },
  { subject: 'faktura', schemaType: 'xsd', latestVersion: 1, deprecatedAtMs: null },
];

test('the lists offer the standard values and keep a current value that is not one of them', () => {
  assert.ok(retentionChoices(30 * DAY).includes(30 * DAY));
  assert.deepEqual(retentionChoices(2 * 3_600_000)[0], 2 * 3_600_000, 'a two-hour retention stays choosable');
  assert.ok(limitChoices(10 * GIB).includes(10 * GIB));
  assert.ok(limitChoices(3 * GIB).includes(3 * GIB));
  assert.ok(backoffChoices(1500).includes(1500));
});

test('quorum is a majority of the copies; the retry pauses double up to a minute', () => {
  assert.equal(quorumOf(3), 2);
  assert.equal(quorumOf(2), 2);
  assert.equal(quorumOf(1), 1);
  assert.deepEqual(backoffSchedule(2000, 5), [2000, 4000, 8000, 16_000]);
  assert.deepEqual(backoffSchedule(30_000, 4), [30_000, 60_000, 60_000]);
  assert.deepEqual(backoffSchedule(1000, 1), []);
});

test('storage facts: bytes, the largest partition and the oldest message kept', () => {
  const f = storageFacts([
    { sizeBytes: 100, earliestTimestampMs: 5000 },
    { sizeBytes: 300, earliestTimestampMs: 3000 },
    { sizeBytes: 0, earliestTimestampMs: null },
  ]);
  assert.deepEqual(f, { bytes: 400, largest: 300, oldestMs: 3000 });
  assert.deepEqual(storageFacts([]), { bytes: null, largest: null, oldestMs: null });
});

test('shortening the retention says what goes, about how much, and that consumers cannot rewind to it', () => {
  const now = 100 * DAY;
  const facts = { bytes: 30 * GIB, largest: 6 * GIB, oldestMs: now - 30 * DAY };
  const lines = retentionImpact({
    current: { retentionMs: 30 * DAY, limitBytes: 8 * GIB },
    next: { retentionMs: 14 * DAY, limitBytes: 8 * GIB },
    facts,
    nowMs: now,
  }).map(norm);
  assert.equal(lines[0], 'Wiadomości starsze niż 14 dni znikną przy najbliższym sprzątaniu (działa co 5 min) — ok. 16,0 GB z 30,0 GB. Zasady zgodności organizacji mogą kazać trzymać je dłużej.');
  assert.match(lines[1], /nie będą już mogli wrócić/);
});

test('a retention nothing is old enough for removes nothing now; a longer one cannot bring messages back', () => {
  const now = 100 * DAY;
  const facts = { bytes: GIB, largest: GIB, oldestMs: now - 2 * DAY };
  const shorter = retentionImpact({ current: { retentionMs: 30 * DAY, limitBytes: 8 * GIB }, next: { retentionMs: 7 * DAY, limitBytes: 8 * GIB }, facts, nowMs: now });
  assert.match(shorter[0], /teraz nic nie zniknie/);
  // Nothing goes, so nothing is out of a reader's reach.
  assert.equal(shorter.length, 1);
  const longer = retentionImpact({ current: { retentionMs: 7 * DAY, limitBytes: 8 * GIB }, next: { retentionMs: 30 * DAY, limitBytes: 8 * GIB }, facts, nowMs: now });
  assert.match(longer[0], /nie da się odzyskać/);
});

test('a lower space limit a partition already exceeds takes its oldest messages at the next sweep', () => {
  const facts = { bytes: 12 * GIB, largest: 6 * GIB, oldestMs: null };
  const exceeded = retentionImpact({ current: { retentionMs: DAY, limitBytes: 8 * GIB }, next: { retentionMs: DAY, limitBytes: 4 * GIB }, facts }).map(norm);
  assert.deepEqual(exceeded, ['Partycje większe niż 4,0 GB stracą najstarsze wiadomości przy najbliższym sprzątaniu (działa co 5 min).']);
  const lower = retentionImpact({ current: { retentionMs: DAY, limitBytes: 16 * GIB }, next: { retentionMs: DAY, limitBytes: 8 * GIB }, facts });
  assert.match(lower[0], /straci najstarsze wiadomości wcześniej/);
});

test('the write window: added partitions, a different confirmation, compression', () => {
  const current = { partitions: 6, acks: 'quorum', compression: 'lz4' };
  const lines = writeImpact({ current, next: { partitions: 8, acks: 'all', compression: 'none' }, rf: 3 }).map(norm);
  assert.match(lines[0], /^Topik będzie miał 8 partycji\. .*innej partycji.*po ponownym połączeniu.*nie da się potem zmniejszyć\.$/);
  assert.match(lines[1], /wszystkie zgodne kopie/);
  assert.match(lines[2], /bez kompresji/);
  assert.deepEqual(writeImpact({ current, next: { ...current, acks: 'leader' }, rf: 1 }), ['Topik ma jedną kopię, więc zapis dalej będzie czekał tylko na node prowadzący.']);
  assert.equal(norm(writeImpact({ current: { ...current, acks: 'leader' }, next: current, rf: 3 })[0]), 'Każdy zapis poczeka, aż wiadomość będzie na 2 z 3 nodów.');
});

test('what a confirmation mode makes a write wait for', () => {
  assert.match(acksHint('quorum', 3), /na 2 z 3 nodów/);
  assert.match(acksHint('leader', 3), /nie czeka na kopie/);
  assert.match(acksHint('all', 3), /każdą zgodną kopię/);
  assert.match(acksHint('all', 1), /jedną kopię/);
});

test('the retry window: after how many attempts, and the pauses the consumer is handed', () => {
  const lines = retryImpact({ current: { attempts: 5, backoffMs: 2000 }, next: { attempts: 3, backoffMs: 5000 } }).map(norm);
  // Only a consumer that reports failed attempts counts them, and flows
  // retry at once: the pause is what the server hands the consumer.
  assert.equal(lines[0], 'Wiadomość, której odbiorca nie przetworzy i zgłosi nieudaną próbę, trafi do nieprzetworzonych po 3 próbach zamiast 5.');
  assert.equal(lines[1], 'Przed kolejnymi próbami serwer poda programowi odbiorcy przerwy ok. 5 s i 10 s. Przepływy TentaFlow ponawiają od razu.');
  assert.match(retryImpact({ current: { attempts: 5, backoffMs: 2000 }, next: { attempts: 1, backoffMs: 2000 } })[0], /bez ponawiania/);
});

test('the pattern window: checking starts, changes mode, or stops', () => {
  const versionOf = (name) => (name === 'wizyta' ? 3 : 0);
  const none = { schemaId: '', validation: 'off' };
  const on = patternImpact({ current: none, next: { schemaId: 'wizyta', validation: 'dlq' }, versionOf }).map(norm);
  assert.match(on[0], /wzorem wizyta \(wersja 3\); niepasujące trafią do nieprzetworzonych/);
  assert.match(on[1], /już zapisane nie są sprawdzane ponownie/);
  assert.match(patternImpact({ current: { schemaId: 'wizyta', validation: 'dlq' }, next: { schemaId: 'wizyta', validation: 'warn' }, versionOf })[0], /ostrzeżenie w swoim dzienniku/);
  assert.deepEqual(patternImpact({ current: { schemaId: 'wizyta', validation: 'dlq' }, next: none, versionOf }), ['Wiadomości przestaną być sprawdzane.']);
  assert.deepEqual(patternImpact({ current: none, next: none, versionOf }), []);
});

test('an update carries only what the window changed', () => {
  assert.deepEqual(
    buildTopicUpdateRequest('tentabus-1a2b3c4d', 'wyniki-badan', { retentionMs: 1, retentionBytesPerPartition: 2 }, { retentionMs: 5, retentionBytesPerPartition: 2 }),
    { instanceId: 'tentabus-1a2b3c4d', name: 'wyniki-badan', options: { retentionMs: 5 } },
  );
});

test('who can change the topic: its administrators by name, else the instance administrator', () => {
  assert.equal(whoCanChange(['Anna Kowalska']), 'Zmiany w tym topiku może robić administrator topiku (Anna Kowalska).');
  assert.match(whoCanChange(['Anna', 'Tomasz']), /\(Anna i Tomasz\)/);
  assert.equal(whoCanChange([]), 'Zmiany w tym topiku może robić administrator instancji.');
});

test('typed counts: only whole numbers in range', () => {
  assert.equal(partitionCount('8', 6), 8);
  assert.equal(partitionCount('5', 6), null, 'partitions only grow');
  assert.equal(partitionCount('257', 6), null);
  assert.equal(partitionCount('7.5', 6), null);
  assert.equal(attemptCount('0'), null);
  assert.equal(attemptCount('100'), 100);
});

function host(html) {
  const el = document.createElement('div');
  el.innerHTML = html;
  return el;
}

test('the section for an administrator: four cards with "Zmień", locks with their reasons, the delete card', () => {
  const el = host(settingsHtml({ topic, partitions: [], subjects, access: { canRead: true, canAdmin: true }, adminLabels: [] }));
  assert.equal(el.querySelectorAll('[data-go="change"]').length, 4);
  assert.deepEqual([...el.querySelectorAll('[data-go="change"]')].map((b) => b.dataset.card), ['retention', 'write', 'retry', 'pattern']);
  assert.ok(el.querySelector('[data-go="delete"]'));
  assert.equal(el.querySelector('.tb-who-can'), null);
  const text = norm(el.textContent);
  assert.match(text, /Jak długo trzymać wiadomości30 dni/);
  assert.match(text, /Kiedy zapis jest potwierdzonygdy zapisze większość kopiiZapis czeka, aż wiadomość będzie na 2 z 3 nodów\./);
  assert.match(text, /wizyta \(JSON Schema\)Sprawdzana jest zawsze najnowsza wersja — teraz 3\./);
  assert.equal(el.querySelectorAll('.tb-vr-lock').length, 4, 'cleanup, copies, durability, content kind');
});

test('the section for a reader: no buttons, who can change it, and why the rest is closed without read access', () => {
  const el = host(settingsHtml({ topic, partitions: [], subjects, access: { canRead: false, canAdmin: false }, adminLabels: ['Anna Kowalska'] }));
  assert.equal(el.querySelectorAll('tf-button').length, 0);
  const lines = [...el.querySelectorAll('.tb-who-can')].map((n) => norm(n.textContent));
  assert.equal(lines.length, 2);
  assert.match(lines[0], /Nie masz prawa czytania topiku wyniki-badan/);
  assert.equal(lines[1], 'Zmiany w tym topiku może robić administrator topiku (Anna Kowalska).');
});

test('a withdrawn pattern the topic still uses is said so', () => {
  const el = host(settingsHtml({ topic: { ...topic, schemaId: 'wizyta-2025' }, partitions: [], subjects, access: { canRead: true, canAdmin: true }, adminLabels: [] }));
  assert.match(el.textContent, /Wzór jest wycofany: topik dalej sprawdza nim wiadomości/);
});

function openWindow(card, overrides = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const sent = [];
  const saved = [];
  const win = openSettingsWindow(card, {
    instanceId: 'tentabus-1a2b3c4d',
    view: { topic, partitions: [], subjects, capabilities: { schemaTypes: ['json_schema'] }, nowMs: Date.now() },
    update: async (request) => { sent.push(request); },
    describeError: () => 'Odmowa serwera.',
    onSaved: (notice) => saved.push(notice),
    ...overrides,
  });
  return { win, sent, saved };
}

const pick = (el, value) => {
  el.value = value;
  el.dispatchEvent(new CustomEvent('change', { detail: { value } }));
};
const type = (el, value) => {
  el.value = value;
  el.dispatchEvent(new Event('input'));
};

test('retention window: nothing to save until something changes, then the change alone is sent', async () => {
  const { win, sent, saved } = openWindow('retention');
  assert.equal(win.getAttribute('modal'), '');
  const save = win.querySelector('[data-act="save"]');
  assert.ok(save.hasAttribute('disabled'));
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Nic jeszcze nie zmieniono/);
  pick(win.querySelector('#tb-set-retention'), String(14 * DAY));
  assert.equal(save.hasAttribute('disabled'), false);
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Co się stanie po zapisaniu: Wiadomości starsze niż 14 dni/);
  save.click();
  await tick();
  assert.deepEqual(sent, [{ instanceId: 'tentabus-1a2b3c4d', name: 'wyniki-badan', options: { retentionMs: 14 * DAY } }]);
  assert.equal(saved[0].title, 'Zapisano przechowywanie.');
  assert.equal(norm(saved[0].text), 'Wiadomości są teraz trzymane 14 dni, najwyżej 8,0 GB na partycję.');
});

test('write window: a partition count below the current one cannot be saved', () => {
  const { win } = openWindow('write');
  const field = win.querySelector('#tb-set-partitions');
  type(field, '4');
  assert.match(field.getAttribute('error'), /od 6 do 256/);
  assert.ok(win.querySelector('[data-act="save"]').hasAttribute('disabled'));
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Popraw zaznaczone pole/);
  type(field, '8');
  assert.equal(field.hasAttribute('error'), false);
  assert.equal(win.querySelector('[data-act="save"]').hasAttribute('disabled'), false);
});

test('a refusal stays in the window with the reason, and nothing is reported saved', async () => {
  const { win, saved } = openWindow('retry', { update: async () => { throw new Error('bus.invalid_topic_config'); } });
  type(win.querySelector('#tb-set-attempts'), '3');
  win.querySelector('[data-act="save"]').click();
  await tick();
  await tick();
  assert.equal(saved.length, 0);
  assert.equal(win.isConnected, true);
  assert.equal(win.querySelector('[data-role="error"]').hidden, false);
  assert.match(win.querySelector('[data-role="error"]').textContent, /Odmowa serwera/);
});

test('pattern window: only patterns for the topic\'s content, "bez wzoru" clears the binding', async () => {
  const { win, sent, saved } = openWindow('pattern');
  const schema = win.querySelector('#tb-set-schema');
  const labels = [...schema.querySelectorAll('select option')].map((o) => o.textContent);
  assert.deepEqual(labels, ['wizyta · JSON Schema', 'bez wzoru']);
  assert.ok(!labels.some((l) => String(l).startsWith('faktura')), 'an XSD pattern is not offered for JSON');
  assert.ok(!labels.some((l) => String(l).startsWith('wizyta-2025')), 'a withdrawn pattern is not offered anew');
  pick(schema, '');
  assert.equal(win.querySelector('[data-role="mode-box"]').hidden, true);
  win.querySelector('[data-act="save"]').click();
  await tick();
  assert.deepEqual(sent[0].options, { schemaId: '' });
  assert.match(saved[0].text, /nie ma wzoru/);
});
