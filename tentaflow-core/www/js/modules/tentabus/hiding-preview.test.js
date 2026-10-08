// =============================================================================
// File: modules/tentabus/hiding-preview.test.js
// Description: "Podgląd, jak widzi…" (U7): the window opens on the newest
// message of the busiest partition, asks for the record as the chosen subject
// reads it (the exact FieldPolicyPreviewRequest), shows the record with the
// hidden fields named and marked, says that every preview is audited, and
// explains a preview narrowed by the administrator's own rule instead of
// presenting it as the subject's view.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { openHidingPreview, previewResultHtml, whoPhrase, displayPayload } = await import('./hiding-preview.js');
const { fieldSource } = await import('./topic-hiding.js');
const { newestOffset } = await import('./message-preview.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const words = (html) => { const d = document.createElement('div'); d.innerHTML = String(html).replace(/</g, ' <'); return norm(d.textContent); };
const tick = (ms = 20) => new Promise((r) => setTimeout(r, ms));
const closeAll = () => document.querySelectorAll('tf-window').forEach((w) => w.remove());
const pick = (el, value) => {
  el.value = value;
  el.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value } }));
};
const bytes = (text) => new TextEncoder().encode(text);

const INSTANCE = 'tentabus-a1b2c3d4';
const TOPIC = 'wizyty';
const NOW = new Date(2026, 8, 30, 14, 30, 0).getTime();
const SCHEMA = {
  subject: 'wizyta',
  version: 5,
  text: JSON.stringify({ type: 'object', properties: { pacjent: { title: 'dane pacjenta' }, lekarz: {}, powod: { title: 'powód wizyty' } } }),
};
const json = fieldSource({ format: 'json', schema: SCHEMA });
const record = { partition: 1, offset: 41, timestampMs: NOW - 60_000, key: new Uint8Array(), headers: [], payloadPreview: bytes('{"pacjent":{"pesel":"x"},"lekarz":"Maria Zając"}'), isBlobRef: false, truncated: false };
const RESPONSE = { record, applied: [{ field: 'lekarz', action: 'show' }, { field: 'pacjent', action: 'show' }, { field: 'powod', action: 'hide' }], limitedByCaller: false };
const PARTITIONS = [
  { partition: 0, earliestOffset: 0, highWatermark: 120 },
  { partition: 1, earliestOffset: 10, highWatermark: 900 },
  { partition: 2, earliestOffset: 0, highWatermark: 0 },
];

test('the number of the newest message of a partition, none for an empty one', () => {
  assert.equal(newestOffset(PARTITIONS[1]), 899);
  assert.equal(newestOffset(PARTITIONS[2]), null);
  assert.equal(newestOffset({ partition: 3, earliestOffset: 50, highWatermark: 50 }), null);
  assert.equal(newestOffset(undefined), null);
});

test('who is named in the sentences: the kind and the name, or everyone without a rule of their own', () => {
  assert.equal(whoPhrase({ subjectType: 'group', label: 'Księgowość' }), 'grupa Księgowość');
  assert.equal(whoPhrase({ subjectType: 'addon', label: 'Asystent lekarza' }), 'addon Asystent lekarza');
  assert.equal(whoPhrase({ subjectType: 'user', label: 'Tomasz Nowak' }), 'użytkownik Tomasz Nowak');
  assert.equal(whoPhrase({ subjectType: 'any', label: 'Wszyscy' }), 'wszyscy bez własnej zasady');
});

test('the record is shown as text: JSON indented, an HL7 message one segment per line, anything else as it is', () => {
  assert.equal(displayPayload(bytes('{"a":1,"b":[2]}')), '{\n  "a": 1,\n  "b": [\n    2\n  ]\n}');
  assert.equal(displayPayload(bytes('MSH|^~\\&|LIS\rPID|1||123\rOBX|1|NM')), 'MSH|^~\\&|LIS\nPID|1||123\nOBX|1|NM');
  assert.equal(displayPayload(bytes('<a><b/></a>')), '<a><b/></a>');
  assert.equal(displayPayload(new Uint8Array([0xff, 0x01])), 'ff 01', 'bytes that are not text are shown as hex');
});

test('the answer names the hidden fields, marks each field shown or hidden and counts what the rules hid', () => {
  const html = previewResultHtml({ resp: RESPONSE, subject: { subjectType: 'group', label: 'Księgowość' }, source: json, nowMs: NOW });
  const host = document.createElement('div');
  host.innerHTML = html;
  assert.match(norm(host.querySelector('.tb-preview-record-head').textContent), /^Tak widzi ją: grupa Księgowość wiadomość 41 · partycja 1 · zapisana dziś 14:29:00$/);
  assert.equal(host.querySelector('[data-role="payload"]').textContent, '{\n  "pacjent": {\n    "pesel": "x"\n  },\n  "lekarz": "Maria Zając"\n}');
  assert.equal(norm(host.querySelector('[data-role="summary"]').textContent), 'Zasady ukryły 1 pole: powod.');
  const rows = [...host.querySelectorAll('[data-role="applied"] .tb-right-row')].map((r) => [r.dataset.fieldAction, words(r.innerHTML)]);
  assert.deepEqual(rows, [['show', 'lekarz widoczne'], ['show', 'pacjent dane pacjenta widoczne'], ['hide', 'powod powód wizyty ukryte']]);
  assert.equal(host.querySelector('[data-role="limited"]'), null, 'nothing narrowed this preview');
  const none = previewResultHtml({ resp: { ...RESPONSE, applied: [{ field: 'lekarz', action: 'show' }] }, subject: { subjectType: 'any', label: 'Wszyscy' }, source: json, nowMs: NOW });
  assert.match(words(none), /Zasady nie ukryły żadnego pola tej wiadomości\./);
  assert.match(words(none), /Tak widzi ją: wszyscy bez własnej zasady/);
});

test('a preview narrowed by the administrator\'s own rule says so, in plain words, and who is affected', () => {
  const html = previewResultHtml({ resp: { ...RESPONSE, limitedByCaller: true }, subject: { subjectType: 'addon', label: 'Asystent lekarza' }, source: json, nowMs: NOW });
  const host = document.createElement('div');
  host.innerHTML = html;
  const limited = host.querySelector('[data-role="limited"]');
  assert.equal(limited.getAttribute('tone'), 'warning');
  assert.equal(limited.getAttribute('title'), 'Ten podgląd jest węższy niż widok wybranego podmiotu');
  assert.equal(limited.getAttribute('message'),
    'Zasada odczytu, która obowiązuje także Ciebie (Twoja własna, Twojej grupy albo dla wszystkich), ukrywa pola, które zobaczy: addon Asystent lekarza. '
    + 'Podgląd nigdy nie pokazuje więcej, niż widzisz sam, więc tych pól w nim brakuje. Pełny widok pokaże się, gdy Ciebie nie obejmuje żadna zasada odczytu.');
  assert.ok(html.indexOf('data-role="limited"') < html.indexOf('tb-preview-record'), 'the explanation comes before the record it qualifies');
});

function open(overrides = {}) {
  const asked = [];
  const previews = [];
  const ctx = {
    instanceId: INSTANCE,
    topic: TOPIC,
    partitionCount: 3,
    source: json,
    directory: async (q) => { asked.push(q); return { entries: q.kind === 'group' ? [{ subjectType: 'group', subjectId: 'g-ksieg', label: 'Księgowość', memberCount: 4 }, { subjectType: 'group', subjectId: 'g-rej', label: 'Rejestracja', memberCount: 5 }] : [] }; },
    loadPartitions: async () => PARTITIONS,
    preview: async (r) => { previews.push(r); return RESPONSE; },
    describeError: (e) => `błąd: ${e.message}`,
    ...overrides,
  };
  return { win: openHidingPreview(ctx), asked, previews };
}

test('the window opens on the newest message of the busiest partition and warns that the preview is audited', async () => {
  closeAll();
  const { win, asked } = open();
  await tick();
  assert.equal(norm(win._titleEl.textContent), 'Podgląd, jak widzi… — wizyty');
  assert.match(norm(win.querySelector('.tb-audit-banner').textContent), /^Ten podgląd zapisuje się w dzienniku audytu\. Zapisujemy, kto oglądał, kiedy, którą wiadomość i jako kto\./);
  assert.match(win.querySelector('.tb-audit-banner').textContent, /Dostępny tylko dla administratora topiku\./);
  assert.deepEqual(asked, [{ kind: 'group', query: '' }]);
  assert.equal(win.querySelector('[data-role="partition"]').value, '1');
  assert.equal(win.querySelector('[data-role="offset"]').value, '899');
  assert.equal(norm(win.querySelector('[data-role="range"]').textContent), 'od 10 do 899');
  assert.equal(win.querySelector('[data-role="result"]').textContent.trim(), '', 'nothing is read until the administrator asks');
});

test('"Pokaż" sends the exact request for the chosen subject and message and shows what the rules did', async () => {
  closeAll();
  const { win, previews } = open();
  await tick();
  const show = win.querySelector('[data-act="show"]');
  assert.equal(show.hasAttribute('disabled'), false, 'a group is chosen by default');
  pick(win.querySelector('[data-role="subject"]'), 'group:g-rej');
  win.querySelector('[data-role="offset"]').value = '41';
  win.querySelector('[data-role="offset"]').dispatchEvent(new CustomEvent('input', { bubbles: true }));
  pick(win.querySelector('[data-role="partition"]'), '1');
  win.querySelector('[data-role="offset"]').value = '41';
  win.querySelector('[data-role="offset"]').dispatchEvent(new CustomEvent('input', { bubbles: true }));
  show.click();
  await tick();
  assert.deepEqual(previews, [{ instanceId: INSTANCE, topic: TOPIC, partition: 1, offset: 41, subjectType: 'group', subjectId: 'g-rej' }]);
  const result = win.querySelector('[data-role="result"]');
  assert.match(norm(result.querySelector('.tb-preview-record-head').textContent), /^Tak widzi ją: grupa Rejestracja/);
  assert.equal(norm(result.querySelector('[data-role="summary"]').textContent), 'Zasady ukryły 1 pole: powod.');
  assert.equal(win.querySelector('[data-role="error"]').hidden, true);
});

test('"Wszyscy" asks no directory and previews the rule for everyone; another subject kind asks its own list', async () => {
  closeAll();
  const { win, asked, previews } = open();
  await tick();
  asked.length = 0;
  pick(win.querySelector('[data-role="kind"]'), 'any');
  assert.equal(win.querySelector('[data-role="pick-box"]').hidden, true);
  assert.deepEqual(asked, []);
  assert.match(win.querySelector('[data-role="pick-note"]').textContent, /ktoś, kto nie ma własnej zasady ani zasady swojej grupy/);
  win.querySelector('[data-act="show"]').click();
  await tick();
  assert.deepEqual([previews[0].subjectType, previews[0].subjectId], ['any', '*']);
  pick(win.querySelector('[data-role="kind"]'), 'addon');
  await tick();
  assert.deepEqual(asked, [{ kind: 'addon', query: '' }]);
  assert.equal(win.querySelector('[data-role="pick-note"]').textContent, 'Nie znaleziono addonu z dostępem do tej instancji.');
  assert.ok(win.querySelector('[data-act="show"]').hasAttribute('disabled'), 'nobody to look as');
});

test('a message that is not there, or a refusal, is said in the window; a preview narrowed by the caller is explained above the record', async () => {
  closeAll();
  const failing = open({ preview: async () => { throw new Error('bus.record_not_found'); } });
  await tick();
  failing.win.querySelector('[data-act="show"]').click();
  await tick();
  assert.equal(norm(failing.win.querySelector('[data-role="error"]').textContent), 'błąd: bus.record_not_found');
  assert.equal(failing.win.querySelector('[data-role="result"]').textContent.trim(), '');
  closeAll();
  const narrowed = open({ preview: async () => ({ ...RESPONSE, limitedByCaller: true }) });
  await tick();
  narrowed.win.querySelector('[data-act="show"]').click();
  await tick();
  assert.ok(narrowed.win.querySelector('[data-role="result"] tf-alert[data-role="limited"]'));
});

test('a topic\'s partition list that cannot be read leaves the window usable on partition 0 without a number', async () => {
  closeAll();
  const { win, previews } = open({ loadPartitions: async () => { throw new Error('nope'); } });
  await tick();
  assert.equal(win.querySelector('[data-role="offset"]').value, '');
  assert.ok(win.querySelector('[data-act="show"]').hasAttribute('disabled'), 'no number, nothing to ask');
  win.querySelector('[data-role="offset"]').value = '7';
  win.querySelector('[data-role="offset"]').dispatchEvent(new CustomEvent('input', { bubbles: true }));
  win.querySelector('[data-act="show"]').click();
  await tick();
  assert.deepEqual([previews[0].partition, previews[0].offset], [0, 7]);
});
