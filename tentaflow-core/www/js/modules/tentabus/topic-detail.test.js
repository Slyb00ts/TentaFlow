// =============================================================================
// File: modules/tentabus/topic-detail.test.js
// Description: A topic's page (U2): the title with what the topic carries,
// the vertical section menu and the phone's "Sekcja" list, one section at a
// time; rights decide what is shown — no read access closes Stan and
// Partycje i kopie and the preview, with the reason; Stan's tiles, alerts
// and consumers come from the live snapshot of this topic only; the
// partitions table offers "Przenieś prowadzenie" only where a node can take
// over; a deleted topic says so instead of an empty page.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { drawTopicDetail, effectiveSection, sectionOpen, topicDetailLoader } = await import('./topic-detail.js');
const { stateKpis, topicAlerts, topicConsumers } = await import('./topic-state.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 20));
const MIN = 60_000;
const NOW = Date.UTC(2026, 8, 26, 12, 0, 0);

const topic = {
  name: 'wyniki-badan', partitions: 2, retentionMs: 30 * 86_400_000, retentionBytesPerPartition: 8 * 1024 ** 3,
  replicationFactor: 1, acks: 'leader', durabilityClass: 'standard', compression: 'lz4', maxDeliveryAttempts: 5,
  retryBackoffMs: 1000, contentType: 'application/hl7-v2', schemaId: null, validation: 'off',
};
const partitions = [
  { partition: 0, earliestOffset: 0, highWatermark: 1200, sizeBytes: 2048, earliestTimestampMs: Date.UTC(2026, 7, 24) },
  { partition: 1, earliestOffset: 0, highWatermark: 800, sizeBytes: 1024, earliestTimestampMs: Date.UTC(2026, 8, 1) },
];
const stats = {
  topics: [
    { topic: 'wyniki-badan', msgsInPerSec: 30, totalLag: 1800, dlqDepth: 14, dlqLastHour: 3, totalBytesOnDisk: 3072 },
    { topic: 'faktury', msgsInPerSec: 70, totalLag: 0, dlqDepth: 0, dlqLastHour: 0, totalBytesOnDisk: 1 },
  ],
  groups: [
    { group: 'aplikacja-lekarza', topic: 'wyniki-badan', lagTotal: 1800, paused: false, lagRisingSinceMs: NOW - 25 * MIN },
    { group: 'raporty-laboratorium', topic: 'wyniki-badan', lagTotal: 0, paused: false },
    { group: 'system-rozliczen', topic: 'faktury', lagTotal: 400, paused: true },
  ],
};
const nodes = [{ nodeId: 'n-rig', label: 'rig26', reachable: true, isLocal: true }];
const replicaTopics = [{
  topic: 'wyniki-badan',
  partitions: [
    { partition: 0, leaderNodeId: 'n-rig', replicas: ['n-rig'], isr: ['n-rig'], lagging: [] },
    { partition: 1, leaderNodeId: 'n-rig', replicas: ['n-rig'], isr: ['n-rig'], lagging: [] },
  ],
}];

function mount({ access = { canRead: true, canWrite: true, canAdmin: true }, section = 'state', detail, error = null, adminLabels = [] } = {}) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const view = {
    name: 'wyniki-badan',
    detail: detail === undefined ? { topic, partitions, groups: [], access, adminLabels } : detail,
    error,
    errorKind: error ? 'lost' : null,
    section,
    stats,
    subjects: [],
    capabilities: { schemaTypes: ['json_schema'] },
    nodes,
    replicaTopics,
    replicaLags: [],
    lagSeries: new Map(),
    notice: null,
    justMoved: new Set(),
    instanceLabel: 'Produkcja',
    nowMs: NOW,
  };
  const ctx = { view: () => view, go: (a) => moves.push(a) };
  drawTopicDetail(body, ctx);
  return { body, ctx, view, moves };
}

test('sections open with read access, Ustawienia always; a closed one falls back', () => {
  assert.equal(sectionOpen('state', { canRead: false }), false);
  assert.equal(sectionOpen('settings', { canRead: false }), true);
  assert.equal(effectiveSection('partitions', { canRead: true }), 'partitions');
  assert.equal(effectiveSection('state', { canRead: false }), 'settings');
  assert.equal(effectiveSection('nonsense', { canRead: true }), 'state');
});

test('the page: back link, title with what the topic carries, the vertical menu and one section', () => {
  const { body, moves } = mount();
  assert.equal(body.querySelector('.tb-title').textContent, 'wyniki-badan');
  assert.equal(body.querySelector('[data-role="desc"]').textContent, 'HL7 v2 · bez wzoru');
  const menu = body.querySelector('[data-role="menu"]');
  assert.equal(menu.getAttribute('orientation'), 'vertical');
  assert.deepEqual([...menu.querySelectorAll('tf-tab')].map((t) => t.id), ['state', 'settings', 'partitions']);
  assert.equal(menu.querySelector('tf-tab#partitions').getAttribute('count'), '2');
  assert.deepEqual([...body.querySelectorAll('[data-section]')].map((s) => [s.dataset.section, s.hidden]), [['state', false], ['settings', true], ['partitions', true]]);
  body.querySelector('[data-go="back"]').click();
  assert.deepEqual(moves, [{ kind: 'back' }]);
});

test('Stan: four tiles of this topic, its alerts with where to act, its consumers', () => {
  const { body } = mount();
  const tiles = [...body.querySelectorAll('[data-section="state"] tf-stat-card')];
  assert.deepEqual(tiles.map((t) => t.getAttribute('label')), ['Wiadomości na sekundę', 'Czeka na odbiorców', 'Nieprzetworzone', 'Na dysku']);
  assert.equal(tiles[0].getAttribute('delta'), '30% ruchu całej instancji');
  assert.equal(norm(tiles[1].getAttribute('value')), '1 800');
  assert.equal(tiles[1].getAttribute('delta'), 'aplikacja-lekarza nie nadąża');
  assert.equal(tiles[2].getAttribute('delta'), '3 w ostatniej godzinie');
  assert.match(tiles[3].getAttribute('delta'), /najstarsza wiadomość z 24\.08\.2026/);
  const alerts = [...body.querySelectorAll('[data-section="state"] .tb-alert')];
  assert.deepEqual(alerts.map((a) => a.querySelector('[data-role="title"]').textContent), ['Odbiorca aplikacja-lekarza nie nadąża', '14 nieprzetworzonych wiadomości']);
  assert.deepEqual(alerts.map((a) => a.querySelector('tf-button').textContent), ['Zobacz odbiorcę', 'Zobacz i ponów']);
  const consumers = [...body.querySelectorAll('.tb-consumer-row')].map((r) => r.dataset.group);
  assert.deepEqual(consumers, ['aplikacja-lekarza', 'raporty-laboratorium'], 'another topic\'s consumer is not listed');
});

test('moving between sections goes through the shell; the phone list offers the same sections', async () => {
  const { body, moves } = mount();
  body.querySelector('[data-role="menu"] tf-tab#settings > button').click();
  assert.deepEqual(moves.at(-1), { kind: 'section', section: 'settings' });
  const pick = body.querySelector('[data-role="pick"]');
  assert.deepEqual([...pick.querySelectorAll('select option')].map((o) => o.textContent), ['Stan', 'Ustawienia', 'Partycje i kopie']);
});

test('without read access: only Ustawienia, the preview closed with its reason, who can change the topic', () => {
  const { body } = mount({ access: { canRead: false, canWrite: false, canAdmin: false }, adminLabels: ['Anna Kowalska'] });
  const menu = body.querySelector('[data-role="menu"]');
  assert.ok(menu.querySelector('tf-tab#state').hasAttribute('disabled'));
  assert.ok(menu.querySelector('tf-tab#partitions').hasAttribute('disabled'));
  assert.equal(menu.getAttribute('value'), 'settings');
  assert.ok(body.querySelector('[data-role="preview"]').hasAttribute('disabled'));
  assert.match(body.querySelector('[data-role="preview-note"]').textContent, /prawa czytania topiku wyniki-badan/);
  const settings = body.querySelector('[data-section="settings"]');
  assert.equal(settings.hidden, false);
  assert.equal(settings.querySelectorAll('tf-button').length, 0);
  assert.match(settings.textContent, /administrator topiku \(Anna Kowalska\)/);
});

test('Partycje i kopie: one row per partition, and a single copy cannot hand over its leadership', async () => {
  const { body } = mount({ section: 'partitions' });
  await tick();
  const table = body.querySelector('[data-section="partitions"] tf-table');
  assert.equal(table.rows.length, 2);
  assert.match(norm(table.rows[0].range), /od 0 do 1 199/);
  assert.match(table.rows[0]._blocker, /jedną kopię/);
  // A disabled button shows no tooltip: the shared reason is text, said once.
  const section = body.querySelector('[data-section="partitions"]');
  assert.match(section.querySelector('[data-role="blocked"]').textContent, /jedną kopię/);
  assert.doesNotMatch(table.rows[0].partition, /jedną kopię/);
  const button = table.shadowRoot?.querySelector('tf-button[data-act="transfer"]');
  if (button) {
    assert.ok(button.hasAttribute('disabled'));
    assert.match(button.title, /jedną kopię/);
  }
});

test('a reader sees the partitions without the button and with who can move leadership', async () => {
  const { body } = mount({ section: 'partitions', access: { canRead: true, canWrite: false, canAdmin: false } });
  await tick();
  const table = body.querySelector('[data-section="partitions"] tf-table');
  assert.equal(table.rowActions, null);
  assert.equal(body.querySelector('[data-section="partitions"] [data-role="blocked"]').textContent, '');
  assert.match(body.querySelector('[data-section="partitions"] .tb-who-can').textContent, /administrator instancji/);
});

test('a topic deleted meanwhile says so and leads back to the list', () => {
  const { body, moves } = mount({ detail: null, error: new Error('protocol error NotFound: bus.topic_not_found: \'wyniki-badan\'') });
  assert.match(body.textContent, /Wszystkie topiki/);
  assert.ok(body.querySelector('tf-empty-state').getAttribute('title').includes('wyniki-badan'));
  body.querySelector('tf-empty-state [data-go="back"]').click();
  assert.deepEqual(moves, [{ kind: 'back' }]);
});

test('Stan\'s figures come from this topic only', () => {
  const groups = stats.groups.filter((g) => g.topic === 'wyniki-badan');
  const k = stateKpis({ topicStats: stats.topics[0], stats, partitions, groups, nowMs: NOW });
  assert.equal(k.share, 30);
  assert.deepEqual(k.lagging, ['aplikacja-lekarza']);
  assert.equal(k.oldestMs, Date.UTC(2026, 7, 24));
  assert.deepEqual(topicAlerts({ topic: 'faktury', stats, replicaLags: [], nowMs: NOW }).map((a) => a.kind), ['paused']);
  assert.deepEqual(topicConsumers(groups).map((c) => [c.group, c.share]), [['aplikacja-lekarza', 100], ['raporty-laboratorium', 0]]);
});

test('only the newest answer about a topic lands: a poll that left before a save cannot paint over it', async () => {
  const pending = [];
  const applied = [];
  let page = { instanceId: 'inst-a', name: 'wizyty' };
  const load = topicDetailLoader({
    fetch: (instanceId, name) => new Promise((resolve) => pending.push({ resolve, name })),
    context: () => page,
    apply: (outcome) => applied.push(outcome.detail?.v ?? 'error'),
  });
  const poll = load('wizyty');
  const afterSave = load('wizyty');
  pending[1].resolve({ v: 'saved' });
  await afterSave;
  pending[0].resolve({ v: 'before-save' });
  await poll;
  assert.deepEqual(applied, ['saved']);

  // An answer for a page no longer open is dropped too.
  const late = load('wizyty');
  page = { instanceId: 'inst-a', name: 'faktury' };
  pending[2].resolve({ v: 'late' });
  await late;
  assert.deepEqual(applied, ['saved']);
});
