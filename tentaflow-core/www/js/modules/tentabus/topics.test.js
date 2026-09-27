// =============================================================================
// File: modules/tentabus/topics.test.js
// Description: The Topiki list (T02): rows joined with the stats snapshot,
// "Opóźnione" by the same thresholds as the overview alerts, the filter
// counts and search, the footer totals of the rows shown, the second line
// of each row, and the drawn states — list with row actions (the bin only
// for an administrator, with a line saying who can delete), empty (T11) and
// error (T12) — together with the moves the buttons ask the shell for.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { topicRows, filterTopicRows, topicsFooter, topicSubline, drawTopics } = await import('./topics.js');

const NOW = 1_790_000_000_000;
const MIN = 60_000;
const GIB = 1024 ** 3;

const topics = [
  { name: 'wyniki-badan', partitions: 6, replicationFactor: 3, retentionMs: 30 * 86_400_000, contentType: 'application/hl7-v2', schemaId: 'wynik-badania' },
  { name: 'wizyty', partitions: 6, replicationFactor: 3, retentionMs: 14 * 86_400_000, contentType: 'application/json', schemaId: 'wizyta' },
  { name: 'faktury', partitions: 2, replicationFactor: 3, retentionMs: 365 * 86_400_000, contentType: 'application/xml', schemaId: null },
  { name: 'przyjecia', partitions: 3, replicationFactor: 3, retentionMs: 30 * 86_400_000, contentType: 'application/hl7-v2', schemaId: null },
  { name: '__dlq.wizyty', partitions: 1, replicationFactor: 3, retentionMs: 86_400_000, contentType: '', isDlq: true },
];

const stats = {
  topics: [
    { topic: 'wyniki-badan', msgsInPerSec: 412, totalLag: 18420, dlqDepth: 14, totalBytesOnDisk: 38.2 * GIB },
    { topic: 'wizyty', msgsInPerSec: 188.4, totalLag: 2340, dlqDepth: 6, totalBytesOnDisk: 11.4 * GIB },
    { topic: 'faktury', msgsInPerSec: 12, totalLag: 10800, dlqDepth: 0, totalBytesOnDisk: 1.9 * GIB },
    { topic: 'przyjecia', msgsInPerSec: 96, totalLag: 120, dlqDepth: 0, totalBytesOnDisk: 6.1 * GIB },
  ],
  groups: [
    // Growing for 25 min with 18 420 waiting: falls behind.
    { group: 'aplikacja-lekarza', topic: 'wyniki-badan', lagTotal: 18420, lagRisingSinceMs: NOW - 25 * MIN, paused: false },
    // Growing but only for 3 min: not yet.
    { group: 'rejestracja', topic: 'wizyty', lagTotal: 2340, lagRisingSinceMs: NOW - 3 * MIN, paused: false },
    // Paused with work waiting.
    { group: 'system-rozliczen', topic: 'faktury', lagTotal: 10800, paused: true },
    { group: 'archiwum', topic: 'przyjecia', lagTotal: 120, lagRisingSinceMs: null, paused: false },
  ],
};

const rows = () => topicRows({ topics, stats, nowMs: NOW });

test('rows: one per topic of the reader, joined with the stats; the broker\'s own topics never appear', () => {
  const r = rows();
  assert.deepEqual(r.map((x) => x.name), ['wyniki-badan', 'wizyty', 'faktury', 'przyjecia']);
  const wyniki = r[0];
  assert.equal(wyniki.contentLabel, 'HL7 v2');
  assert.equal(wyniki.waiting, 18420);
  assert.equal(wyniki.dlq, 14);
  assert.equal(wyniki.replicas, 3);
  assert.deepEqual(r.map((x) => x.delayed), [true, false, true, false]);
});

test('a topic the snapshot does not list yet shows zeros', () => {
  const [fresh] = topicRows({ topics: [{ name: 'nowy', partitions: 3, replicationFactor: 1, retentionMs: 1 }], stats: { topics: [], groups: [] }, nowMs: NOW });
  assert.deepEqual([fresh.rate, fresh.waiting, fresh.dlq, fresh.bytes, fresh.delayed], [0, 0, 0, 0, false]);
});

test('filters count and select; search matches the name, results in name order', () => {
  const all = filterTopicRows(rows());
  assert.deepEqual(all.counts, { all: 4, delayed: 2, dlq: 2 });
  assert.deepEqual(all.rows.map((r) => r.name), ['faktury', 'przyjecia', 'wizyty', 'wyniki-badan']);
  assert.deepEqual(filterTopicRows(rows(), { filter: 'delayed' }).rows.map((r) => r.name), ['faktury', 'wyniki-badan']);
  assert.deepEqual(filterTopicRows(rows(), { filter: 'dlq' }).rows.map((r) => r.name), ['wizyty', 'wyniki-badan']);
  assert.deepEqual(filterTopicRows(rows(), { query: 'WYN' }).rows.map((r) => r.name), ['wyniki-badan']);
  assert.deepEqual(filterTopicRows(rows(), { filter: 'dlq', query: 'fakt' }).rows, []);
});

test('the footer sums the rows it is under', () => {
  const f = topicsFooter(filterTopicRows(rows(), { filter: 'delayed' }).rows);
  assert.equal(f.topics, 2);
  assert.equal(f.partitions, 8);
  assert.equal(f.rate, 424);
  assert.equal(Math.round(f.bytes / GIB * 10) / 10, 40.1);
});

test('the second line names the content and the pattern, or says there is none', () => {
  const [wyniki, , faktury] = rows();
  assert.equal(topicSubline(wyniki), 'HL7 v2 · wzór wynik-badania');
  assert.equal(topicSubline(faktury), 'XML · bez wzoru');
  assert.equal(topicSubline({ contentLabel: '', schemaId: '' }), 'bez wzoru');
});

function mount(view) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const ctx = { view: () => ({ stats, instanceLabel: 'Produkcja', error: null, errorKind: null, canAdmin: true, notice: null, nowMs: NOW, ...view }), go: (a) => moves.push(a) };
  drawTopics(body, ctx);
  return { body, moves, ctx };
}

const norm = (s) => String(s).replace(/[  ]/g, ' ');

test('list: rows, filter labels with counts, footer, create button for an administrator', () => {
  const { body, moves } = mount({ topics });
  const table = body.querySelector('[data-role="table"]');
  assert.equal(table.rows.length, 4);
  const wyniki = table.rows.find((r) => r._topic === 'wyniki-badan');
  assert.match(wyniki.name, /HL7 v2 · wzór wynik-badania/);
  assert.match(norm(wyniki.waiting), /18 420 czeka/, 'a delayed topic gets the waiting chip');
  const przyjecia = table.rows.find((r) => r._topic === 'przyjecia');
  assert.equal(przyjecia.waiting, '120', 'a topic that keeps up shows a plain number');
  assert.equal(wyniki.retention, '30 dni');
  const labels = [...body.querySelectorAll('[data-role="filter"] .tf-seg-opt')].map((b) => b.textContent);
  assert.deepEqual(labels, ['Wszystkie 4', 'Opóźnione 2', 'Nieprzetworzone 2']);
  assert.match(norm(body.querySelector('[data-role="footer"]').textContent), /4 topiki.*17 partycji.*708 wiadomości\/s.*na dysku/);
  assert.equal(body.querySelector('.tb-admin-note'), null);
  body.querySelector('[data-go="create"]').click();
  assert.deepEqual(moves, [{ kind: 'create' }]);
});

test('row actions: preview, delete and open, each without opening the row itself', () => {
  const { body, moves } = mount({ topics });
  const table = body.querySelector('[data-role="table"]');
  const cell = table.rowActions(table.rows[0], 0, () => table.rows[0]);
  const acts = [...cell.querySelectorAll('tf-button')].map((b) => b.dataset.act);
  assert.deepEqual(acts, ['preview', 'delete', 'open']);
  for (const b of cell.querySelectorAll('tf-button')) b.click();
  assert.deepEqual(moves, [
    { kind: 'preview', topic: table.rows[0]._topic },
    { kind: 'delete', topic: table.rows[0]._topic },
    { kind: 'open', topic: table.rows[0]._topic },
  ]);
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[1], index: 1 } }));
  assert.deepEqual(moves.at(-1), { kind: 'open', topic: table.rows[1]._topic });
});

test('without administration: no create, no bin, and a line saying who can', () => {
  const { body } = mount({ topics, canAdmin: false });
  assert.equal(body.querySelector('[data-go="create"]'), null);
  assert.match(body.querySelector('.tb-admin-note').textContent, /administrator instancji/);
  const table = body.querySelector('[data-role="table"]');
  const acts = [...table.rowActions(table.rows[0], 0, () => table.rows[0]).querySelectorAll('tf-button')].map((b) => b.dataset.act);
  assert.deepEqual(acts, ['preview', 'open']);
});

test('the notice of a create or delete is drawn above the list', () => {
  const { body } = mount({ topics, notice: { tone: 'success', title: 'Utworzono topik nowy.', text: 'Programy mogą już do niego wysyłać wiadomości.' } });
  const alert = body.querySelector('[data-role="notice"] tf-alert');
  assert.equal(alert.getAttribute('title'), 'Utworzono topik nowy.');
  assert.equal(alert.getAttribute('tone'), 'success');
});

test('one topic: nothing to narrow, so no search and no filters', () => {
  const { body } = mount({ topics: [topics[0]] });
  assert.equal(body.querySelector('[data-role="search"]').hidden, true);
  assert.equal(body.querySelector('[data-role="filter"]').hidden, true);
});

test('empty (T11) and error (T12) states', () => {
  const empty = mount({ topics: [] });
  assert.equal(empty.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie ma jeszcze żadnego topiku');
  empty.body.querySelector('[data-go="create"]').click();
  assert.deepEqual(empty.moves, [{ kind: 'create' }]);
  const failed = mount({ topics: null, error: new Error('socket closed'), errorKind: 'lost' });
  assert.equal(failed.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie udało się wczytać topików');
  failed.body.querySelector('[data-go="retry"]').click();
  assert.deepEqual(failed.moves, [{ kind: 'retry' }]);
});
