// =============================================================================
// File: modules/tentabus/overview.test.js
// Description: The Przegląd tab rendered from snapshot fixtures in happy-dom:
// the populated tab (KPI tiles, busiest topics, alerts, node rows), the empty
// instance (T11) with its one way forward, the error card (T12) before any
// data, the stale state that keeps the last numbers, and the rule that a poll
// repaints text in place instead of rebuilding a row. Every button leads to a
// move the shell performs (`ctx.go`).
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawOverview, paintOverview, overviewMode } = await import('./overview.js');

const NOW = 1_800_000_000_000;
const MIN = 60_000;
const sp = (s) => String(s).replace(/\u00a0|\u202f/g, ' ').replace(/\s+/g, ' ').trim();
const metaText = (card) => [...card.querySelectorAll('.tb-alert-meta > span')].map((s) => sp(s.textContent)).join(' | ');

const topicList = [
  { name: 'wyniki-badan', partitions: 6, contentType: 'application/hl7-v2', isDlq: false },
  { name: 'faktury', partitions: 3, contentType: 'application/xml', isDlq: false },
  { name: '__dlq.wyniki-badan', partitions: 6, isDlq: true },
];
function stats(over = {}) {
  return {
    topicCount: 3,
    dlqTopicCount: 1,
    totalMsgsInPerSec: 412,
    totalBytesOnDisk: 38.2 * 1024 ** 3,
    totalDlqDepth: 14,
    topics: [
      { topic: 'wyniki-badan', msgsInPerSec: 412, totalLag: 18420, totalBytesOnDisk: 38.2 * 1024 ** 3, dlqDepth: 14, dlqLastHour: 3 },
      { topic: 'faktury', msgsInPerSec: 0, totalLag: 10800, totalBytesOnDisk: 1024 ** 3, dlqDepth: 0, dlqLastHour: 0 },
    ],
    groups: [
      { group: 'aplikacja-lekarza', topic: 'wyniki-badan', lagTotal: 18420, paused: false, lagRisingSinceMs: NOW - 25 * MIN },
      { group: 'system-rozliczen', topic: 'faktury', lagTotal: 10800, paused: true, lagRisingSinceMs: null },
      { group: 'raporty', topic: 'wyniki-badan', lagTotal: 0, paused: false, lagRisingSinceMs: null },
    ],
    ...over,
  };
}
const nodes = [{ nodeId: 'n1', label: 'rig26', reachable: true }];
const replicaTopics = [
  { topic: 'wyniki-badan', partitions: [0, 1, 2, 3, 4, 5].map((p) => ({ partition: p, leaderNodeId: 'n1', replicas: ['n1'], isr: ['n1'] })) },
  { topic: 'faktury', partitions: [0, 1, 2].map((p) => ({ partition: p, leaderNodeId: 'n1', replicas: ['n1'], isr: ['n1'] })) },
  { topic: '__bus.metrics', partitions: [{ partition: 0, leaderNodeId: 'n1', replicas: ['n1'], isr: ['n1'] }] },
];

function mount(view) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  let current = { stats: null, error: null, errorKind: null, topicList, nodes, replicaTopics, replicaLags: [], instanceLabel: 'Produkcja', stale: false, ratePoints: [], nowMs: NOW, ...view };
  const ctx = { view: () => current, go: (a) => moves.push(a) };
  drawOverview(body, ctx);
  return { body, moves, ctx, set(next) { current = { ...current, ...next }; paintOverview(body, ctx); } };
}

test('mode: loading, error, empty and populated', () => {
  assert.equal(overviewMode({ stats: null, error: null }), 'loading');
  assert.equal(overviewMode({ stats: null, error: new Error('x') }), 'error');
  assert.equal(overviewMode({ stats: stats({ topics: [{ topic: '__bus.metrics', msgsInPerSec: 0 }] }) }), 'empty', 'broker-internal topics do not count');
  assert.equal(overviewMode({ stats: stats() }), 'full');
});

test('populated: KPI tiles carry the snapshot figures', () => {
  const { body } = mount({ stats: stats() });
  const tiles = [...body.querySelectorAll('[data-role="kpi"] tf-stat-card')];
  assert.deepEqual(tiles.map((t) => t.dataset.kpi), ['rate', 'topics', 'groups', 'dlq']);
  assert.equal(tiles[0].getAttribute('value'), '412');
  assert.equal(sp(tiles[0].getAttribute('delta')), 'zapisywane teraz do 1 topiku');
  assert.equal(tiles[1].getAttribute('value'), '2');
  assert.equal(sp(tiles[1].getAttribute('suffix')), '· 9 partycji');
  assert.equal(sp(tiles[1].getAttribute('delta')), '39,2 GB na dysku');
  assert.equal(tiles[2].getAttribute('label'), 'Odbiorcy z opóźnieniem');
  assert.equal(tiles[2].getAttribute('value'), '2');
  assert.equal(sp(tiles[2].getAttribute('suffix')), 'z 3');
  assert.equal(tiles[2].getAttribute('delta'), 'aplikacja-lekarza, system-rozliczen');
  assert.equal(tiles[2].getAttribute('delta-type'), 'warn');
  assert.equal(tiles[3].getAttribute('value'), '14');
  assert.equal(tiles[3].getAttribute('accent'), 'warning');
});

test('populated: busiest topics with content type, waiting chip and a bar relative to the busiest', () => {
  const { body, moves } = mount({ stats: stats() });
  const rows = [...body.querySelectorAll('[data-role="topics"] .topic-mini')];
  assert.deepEqual(rows.map((r) => r.dataset.topic), ['wyniki-badan', 'faktury']);
  assert.equal(sp(rows[0].querySelector('[data-role="sub"]').textContent), 'HL7 v2 · 6 partycji · 38,2 GB');
  assert.equal(sp(rows[0].querySelector('[data-role="waiting"] tf-chip').getAttribute('label')), '18 420 czeka');
  assert.equal(rows[0].querySelector('[data-role="bar"]').getAttribute('value'), '100');
  assert.equal(rows[1].querySelector('[data-role="bar"]').getAttribute('value'), '0');
  rows[1].click();
  assert.deepEqual(moves.at(-1), { kind: 'topic', topic: 'faktury' });
});

test('populated: alerts in order, each with the button that leads to its place', () => {
  const { body, moves } = mount({
    stats: stats(),
    replicaLags: [{ nodeId: 'n3', nodeLabel: 'mac-studio', topic: 'wyniki-badan', partition: 2, lagBytes: 331 * 1024 * 1024, lagMs: 3500 }],
  });
  const cards = [...body.querySelectorAll('.tb-alert')];
  assert.deepEqual(cards.map((c) => sp(c.querySelector('[data-role="title"]').textContent)), [
    'Odbiorca aplikacja-lekarza nie nadąża',
    'Przybywa nieprzetworzonych wiadomości',
    'Odbiorca system-rozliczen jest wstrzymany',
    'Kopia na nodzie mac-studio jest w tyle',
  ]);
  assert.equal(metaText(cards[0]), '18 420 wiadomości czeka | wyniki-badan | czeka od 25 min', 'no history of recent growth: it waits');
  assert.equal(metaText(cards[1]), 'wyniki-badan | 3 w ostatniej godzinie, razem 14');
  assert.equal(metaText(cards[2]), '10 800 wiadomości czeka | faktury');
  assert.equal(metaText(cards[3]), '1 partycja topiku wyniki-badan | razem 331 MB, do 4 s za nodem prowadzącym');
  assert.equal(body.querySelector('[data-role="alerts-count"] tf-chip').getAttribute('label'), '4');
  cards[0].querySelector('tf-button').click();
  cards[1].querySelector('tf-button').click();
  cards[3].querySelector('tf-button').click();
  assert.deepEqual(moves, [
    { kind: 'group', group: 'aplikacja-lekarza', topic: 'wyniki-badan' },
    { kind: 'dlq', topic: 'wyniki-badan' },
    { kind: 'tab', tab: 'replication' },
  ]);
});

test('a lag that grew in the last sample is "rośnie od"', () => {
  const lagSeries = new Map([['aplikacja-lekarza\u0000wyniki-badan', [{ atMs: NOW - 2 * MIN, lagTotal: 18000 }, { atMs: NOW - MIN, lagTotal: 18420 }]]]);
  const { body } = mount({ stats: stats(), lagSeries });
  assert.equal(metaText(body.querySelector('.tb-alert')), '18 420 wiadomości czeka | wyniki-badan | rośnie od 25 min');
});

test('KPI tiles and node rows are links to their tabs, by click and by keyboard', () => {
  const { body, moves } = mount({ stats: stats() });
  const tiles = [...body.querySelectorAll('tf-stat-card')];
  assert.deepEqual(tiles.map((t) => [t.getAttribute('role'), t.getAttribute('tabindex'), t.dataset.tab]),
    [['link', '0', 'topics'], ['link', '0', 'topics'], ['link', '0', 'groups'], ['link', '0', 'dlq']]);
  tiles[3].click();
  tiles[2].dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  const node = body.querySelector('[data-role="nodes"] .job-row');
  node.dispatchEvent(new KeyboardEvent('keydown', { key: ' ', bubbles: true }));
  assert.deepEqual(moves, [{ kind: 'tab', tab: 'dlq' }, { kind: 'tab', tab: 'groups' }, { kind: 'tab', tab: 'replication' }]);
});

test('a poll repaints numbers in place: rows and alert cards keep their elements', () => {
  const m = mount({ stats: stats() });
  const row = m.body.querySelector('.topic-mini');
  const card = m.body.querySelector('.tb-alert');
  const next = stats();
  next.topics[0].totalLag = 19000;
  next.groups[0].lagTotal = 19000;
  m.set({ stats: next });
  assert.equal(m.body.querySelector('.topic-mini'), row);
  assert.equal(m.body.querySelector('.tb-alert'), card);
  assert.equal(sp(row.querySelector('[data-role="waiting"] tf-chip').getAttribute('label')), '19 000 czeka');
});

test('node rows: state, leads/holds and the in-sync chip; one node gets its own sentence', () => {
  const { body } = mount({ stats: stats() });
  const r = body.querySelector('[data-role="nodes"] .job-row');
  assert.equal(r.querySelector('[data-role="state"]').getAttribute('label'), 'działa');
  assert.equal(sp(r.querySelector('[data-role="leads"]').textContent), 'prowadzi 9 partycji');
  assert.equal(r.querySelector('[data-role="holds"]').textContent, '', 'one node holds no copies of other leaders');
  assert.equal(sp(r.querySelector('[data-role="sync"]').getAttribute('label')), '9 z 9 zgodnych');
  assert.match(body.querySelector('[data-role="nodes-sub"]').textContent, /jeden node/);
});

test('empty instance (T11): zero tiles and the way to the topics', () => {
  const { body, moves } = mount({ stats: stats({ topicCount: 0, dlqTopicCount: 0, totalMsgsInPerSec: 0, totalDlqDepth: 0, topics: [], groups: [] }), topicList: [] });
  const tiles = [...body.querySelectorAll('tf-stat-card')];
  assert.deepEqual(tiles.map((t) => t.getAttribute('delta')), ['nie ma jeszcze topików', 'utwórz pierwszy w zakładce Topiki', 'pojawią się, gdy programy zaczną czytać', 'nic nie czeka']);
  assert.equal(body.querySelector('tf-empty-state').getAttribute('title'), 'Instancja Produkcja jest pusta');
  body.querySelector('tf-empty-state tf-button').click();
  assert.deepEqual(moves, [{ kind: 'tab', tab: 'topics' }]);
  assert.equal(body.querySelector('.tb-alert'), null);
});

test('error before any data (T12): a card with the reason and a retry; denied offers none', () => {
  const lost = mount({ stats: null, error: new Error('x'), errorKind: 'lost' });
  const es = lost.body.querySelector('tf-empty-state');
  assert.equal(es.getAttribute('title'), 'Nie udało się wczytać przeglądu');
  assert.match(es.getAttribute('message'), /Połączenie z instancją Produkcja zostało przerwane/);
  lost.body.querySelector('[data-go="retry"]').click();
  assert.deepEqual(lost.moves, [{ kind: 'retry' }]);
  const denied = mount({ stats: null, error: new Error('x'), errorKind: 'denied' });
  assert.equal(denied.body.querySelector('[data-go="retry"]'), null);
});

test('stale data (connection lost after a load) keeps the numbers and greys them', () => {
  const m = mount({ stats: stats() });
  m.set({ error: new Error('x'), errorKind: 'lost', stale: true });
  assert.ok(m.body.classList.contains('is-stale'));
  assert.equal(m.body.querySelector('tf-stat-card').getAttribute('value'), '412');
  m.set({ error: null, errorKind: null, stale: false });
  assert.equal(m.body.classList.contains('is-stale'), false);
});
