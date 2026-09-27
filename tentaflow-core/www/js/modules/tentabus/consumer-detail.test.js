// =============================================================================
// File: modules/tentabus/consumer-detail.test.js
// Description: A consumer's page (U3): the title with the one sentence the
// server can vouch for, the vertical section menu, "Wstrzymaj"/"Wznów" only
// for the topic's administrator (else who can), Stan's tiles (what waits and
// whether it grows, the reading rate from the history, the state, its own
// unprocessed messages) and what needs attention; Miejsce czytania with the
// last message read, the newest one and what waits per partition, "Przesuń"
// in the row and a just-moved partition held back; Ustawienia as values with
// the way of confirming locked; a consumer gone meanwhile says so.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { drawConsumerDetail, consumerDlq, positionRows, consumerKpis, rateText, consumerAlerts } = await import('./consumer-detail.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 20));
const MIN = 60_000;
const NOW = new Date(2026, 8, 23, 15, 0, 0).getTime();
const enc = (s) => Array.from(new TextEncoder().encode(String(s)));
const dlqRecord = (group, atMs) => ({
  partition: 0, offset: 1, timestampMs: atMs,
  headers: [{ key: 'dlq.group_id', value: enc(group) }, { key: 'dlq.last_failed_at_ms', value: enc(atMs) }],
});

const detail = {
  group: 'aplikacja-lekarza', topic: 'wyniki-badan', commitMode: 'auto_after_success', paused: false,
  partitions: [
    { partition: 0, committedOffset: 1000, lag: 3000 },
    { partition: 1, committedOffset: 0, lag: 0 },
  ],
};
const topicDetail = {
  topic: { name: 'wyniki-badan', partitions: 2, contentType: 'application/hl7-v2', maxDeliveryAttempts: 5, retryBackoffMs: 2000, replicationFactor: 1 },
  partitions: [{ partition: 0, earliestOffset: 200, highWatermark: 4000 }, { partition: 1, earliestOffset: 0, highWatermark: 0 }],
  access: { canRead: true, canWrite: true, canAdmin: true },
  adminLabels: [],
};
const stats = {
  topics: [{ topic: 'wyniki-badan', msgsInPerSec: 412 }],
  groups: [{ group: 'aplikacja-lekarza', topic: 'wyniki-badan', lagTotal: 3000, paused: false, lagRisingSinceMs: NOW - 25 * MIN, consumeRatePerMin: 24_000 }],
};
const samples = [{ atMs: NOW - 2 * MIN, lagTotal: 2500, committedTotal: 10 }, { atMs: NOW - MIN, lagTotal: 3000, committedTotal: 20 }];

function mount({ section = 'state', access = topicDetail.access, adminLabels = [], data, error = null, statsOverride, justMoved = new Set(), notice = null } = {}) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const view = {
    group: 'aplikacja-lekarza',
    topic: 'wyniki-badan',
    section,
    data: data === undefined ? {
      detail,
      topicDetail: { ...topicDetail, access, adminLabels },
      dlq: consumerDlq({ records: [dlqRecord('aplikacja-lekarza', NOW - 10 * MIN), dlqRecord('aplikacja-lekarza', NOW - 3 * 3_600_000), dlqRecord('inny', NOW)], hasMore: false, group: 'aplikacja-lekarza', nowMs: NOW }),
      samples,
    } : data,
    error,
    errorKind: error ? 'lost' : null,
    stats: statsOverride || stats,
    notice,
    justMoved,
    busy: false,
    instanceLabel: 'Produkcja',
    nowMs: NOW,
  };
  const ctx = { view: () => view, go: (a) => moves.push(a) };
  drawConsumerDetail(body, ctx);
  return { body, moves, view, redraw: () => drawConsumerDetail(body, ctx) };
}

test('the consumer\'s own unprocessed messages among the page, with the last hour and the newest', () => {
  const d = consumerDlq({ records: [dlqRecord('g', NOW - 10 * MIN), dlqRecord('g', NOW - 2 * 3_600_000), dlqRecord('other', NOW)], hasMore: true, group: 'g', nowMs: NOW });
  assert.deepEqual(d, { count: 2, lastHour: 1, lastAtMs: NOW - 10 * MIN, partial: true });
});

test('reading places per partition from the consumer and its topic', () => {
  const rows = positionRows({ detail, topicPartitions: topicDetail.partitions });
  assert.deepEqual(rows.map((r) => [r.partition, r.committed, r.highWatermark, r.earliest, r.waiting, r.share]), [[0, 1000, 4000, 200, 3000, 100], [1, 0, 0, 0, 0, 0]]);
});

test('the reading rate: approx. per second, per minute below one a second, unknown before two samples', () => {
  assert.deepEqual(rateText(24_000), { value: 'ok. 400', suffix: '/s' });
  assert.deepEqual(rateText(30), { value: '30', suffix: '/min' });
  assert.deepEqual(rateText(0), { value: '0', suffix: '/s' });
  assert.deepEqual(rateText(null), { value: '—', suffix: '' });
});

test('what needs attention: falling behind, paused with a backlog, its unprocessed messages', () => {
  const k = consumerKpis({ live: stats.groups[0], detail, topicStats: stats.topics[0], dlq: { count: 2 }, samples, nowMs: NOW });
  assert.deepEqual(consumerAlerts({ live: stats.groups[0], kpis: k, nowMs: NOW }).map((a) => a.kind), ['lagging', 'dlq']);
  const paused = { ...stats.groups[0], paused: true };
  const kp = consumerKpis({ live: paused, detail, topicStats: null, dlq: null, samples: [], nowMs: NOW });
  assert.deepEqual(consumerAlerts({ live: paused, kpis: kp, nowMs: NOW }).map((a) => a.kind), ['paused']);
});

test('the page: back, title with "czyta topik", the menu, Wstrzymaj for an administrator', () => {
  const { body, moves } = mount();
  assert.equal(body.querySelector('.tb-title').textContent, 'aplikacja-lekarza');
  assert.equal(body.querySelector('[data-role="desc"]').textContent, 'czyta topik wyniki-badan');
  const menu = body.querySelector('[data-role="menu"]');
  assert.equal(menu.getAttribute('orientation'), 'vertical');
  assert.deepEqual([...menu.querySelectorAll('tf-tab')].map((t) => t.id), ['state', 'position', 'settings']);
  assert.equal(menu.querySelector('tf-tab#position').getAttribute('count'), '2');
  const toggle = body.querySelector('[data-role="toggle"]');
  assert.equal(toggle.textContent, 'Wstrzymaj');
  toggle.click();
  body.querySelector('[data-go="back"]').click();
  assert.deepEqual(moves, [{ kind: 'pause' }, { kind: 'back' }]);
});

test('Stan: four tiles, what needs attention and the topic it reads', () => {
  const { body, moves, redraw } = mount();
  redraw();
  const tiles = [...body.querySelectorAll('[data-section="state"] tf-stat-card')];
  assert.deepEqual(tiles.map((t) => t.getAttribute('label')), ['Czeka łącznie', 'Tempo czytania', 'Stan', 'Nieprzetworzone']);
  assert.equal(norm(tiles[0].getAttribute('value')), '3 000');
  assert.equal(tiles[0].getAttribute('delta'), 'rośnie od 25 min');
  assert.equal(tiles[1].getAttribute('value'), 'ok. 400');
  assert.equal(tiles[1].getAttribute('delta'), 'do topiku przybywa 412/s');
  assert.equal(tiles[2].getAttribute('value'), 'działa');
  assert.equal(tiles[2].getAttribute('delta'), 'potwierdza po udanym przetworzeniu');
  assert.equal(tiles[3].getAttribute('value'), '2');
  assert.equal(tiles[3].getAttribute('delta'), '1 w ostatniej godzinie');
  const alerts = [...body.querySelectorAll('[data-section="state"] .tb-alert')];
  assert.deepEqual(alerts.map((a) => a.querySelector('[data-role="title"]').textContent), ['Odbiorca nie nadąża', '2 nieprzetworzone wiadomości tego odbiorcy']);
  assert.match(norm(alerts[0].textContent), /3 000 wiadomości czeka, rośnie od 25 min/);
  // A button that opens another section is not that section's container.
  assert.equal(alerts[0].querySelector('tf-button').hidden, false);
  alerts[0].querySelector('tf-button').click();
  alerts[1].querySelector('tf-button').click();
  body.querySelector('[data-role="topic"]').click();
  assert.deepEqual(moves, [{ kind: 'section', section: 'position' }, { kind: 'dlq' }, { kind: 'topic' }]);
  assert.match(body.querySelector('[data-role="topic-sub"]').textContent, /HL7 v2 · 2 partycje/);
});

test('a paused consumer: Wznów, the state tile and the paused alert', () => {
  const { body } = mount({ statsOverride: { ...stats, groups: [{ ...stats.groups[0], paused: true, lagRisingSinceMs: null }] } });
  assert.equal(body.querySelector('[data-role="toggle"]').textContent, 'Wznów');
  const tiles = [...body.querySelectorAll('[data-section="state"] tf-stat-card')];
  assert.equal(tiles[2].getAttribute('value'), 'wstrzymany');
  assert.equal(tiles[2].getAttribute('delta'), 'nie dostaje nowych wiadomości');
  const titles = [...body.querySelectorAll('[data-section="state"] .tb-alert [data-role="title"]')].map((t) => t.textContent);
  assert.ok(titles.includes('Odbiorca jest wstrzymany'));
});

test('Miejsce czytania: last read, newest and waiting per partition; Przesuń in the row', async () => {
  const { body, moves } = mount({ section: 'position' });
  await tick();
  const table = body.querySelector('[data-section="position"] tf-table');
  assert.equal(table.rows.length, 2);
  assert.equal(norm(table.rows[0].read), '999');
  assert.equal(norm(table.rows[0].last), '3 999');
  assert.match(norm(table.rows[0].waiting), /3 000/);
  assert.equal(table.rows[1].read, 'nic');
  assert.equal(table.rows[1].last, 'brak wiadomości');
  const button = table.rowActions(table.rows[0], 0);
  assert.equal(button.textContent, 'Przesuń');
  button.click();
  assert.deepEqual(moves, [{ kind: 'move', partition: 0 }]);
  assert.match(norm(body.querySelector('[data-section="position"] [data-role="footer"]').textContent), /Razem czeka 3 000 wiadomości/);
});

test('a partition just moved is marked and not offered again until refresh', async () => {
  const { body } = mount({ section: 'position', justMoved: new Set([0]), notice: { section: 'position', title: 'Zapisano nowe miejsce czytania.', text: 'Partycja 0: …' } });
  await tick();
  const table = body.querySelector('[data-section="position"] tf-table');
  assert.match(table.rows[0].partition, /zmieniono przed chwilą/);
  assert.ok(table.rowActions(table.rows[0], 0).hasAttribute('disabled'));
  assert.equal(body.querySelector('[data-section="position"] tf-alert').getAttribute('title'), 'Zapisano nowe miejsce czytania.');
});

test('without administration: no buttons, and who can pause and move', async () => {
  const { body } = mount({ section: 'position', access: { canRead: true, canWrite: false, canAdmin: false }, adminLabels: ['Anna Kowalska'] });
  await tick();
  assert.equal(body.querySelector('[data-role="toggle"]'), null);
  assert.match(body.querySelector('.tb-title-note').textContent, /administrator topiku wyniki-badan \(Anna Kowalska\)/);
  assert.equal(body.querySelector('[data-section="position"] tf-table').rowActions, null);
  assert.equal(body.querySelector('[data-section="position"] .tb-who-can'), null, 'said once, under the title — not again in the section');
});

test('Ustawienia: values only; the way of confirming is locked, retries lead to the topic', () => {
  const { body, moves } = mount({ section: 'settings' });
  const s = body.querySelector('[data-section="settings"]');
  assert.equal(s.querySelectorAll('[data-go="change"]').length, 0, 'no "Zmień" on a consumer');
  assert.match(s.textContent, /Kiedy potwierdza wiadomość/);
  assert.match(s.textContent, /po udanym przetworzeniu/);
  assert.match(s.textContent, /Ustawia go program odbiorcy przy każdym połączeniu/);
  assert.match(norm(s.textContent), /5 prób, pierwsza przerwa 2 s/);
  assert.doesNotMatch(s.textContent, /czas na odpowiedź/i);
  s.querySelector('[data-go="topic-settings"]').click();
  assert.deepEqual(moves, [{ kind: 'topic-settings' }]);
});

test('a consumer gone meanwhile says so and leads back to the list', () => {
  const { body, moves } = mount({ data: null, error: new Error('protocol error NotFound: bus.group_not_found: \'aplikacja-lekarza\' on \'wyniki-badan\'') });
  assert.match(body.querySelector('tf-empty-state').getAttribute('title'), /aplikacja-lekarza/);
  body.querySelector('tf-empty-state [data-go="back"]').click();
  assert.deepEqual(moves, [{ kind: 'back' }]);
});
