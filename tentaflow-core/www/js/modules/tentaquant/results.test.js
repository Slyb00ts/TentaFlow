// =============================================================================
// File: modules/tentaquant/results.test.js
// Description: The project gallery (Q16) draws every thumbnail in the browser
// from `runs.tile_json`, so the tests pin the reading of that column, the three
// tile shapes, the filters, and the selection that travels to the comparison.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  COMPARE_MAX, TILE_KINDS, drawResults, filterResults, parseTile, resultFilterState,
  resultTitle, seriesPoints, tileSvg,
} = await import('./results.js');

const NODES = [{ nodeId: 'node-a', nodeName: 'spark-01', online: true, instanceStatus: 'ready' }];

const HIST_TILE = JSON.stringify({
  kind: 'histogram', counts_top: [['00', 0.5], ['11', 0.5]], series: [], bloch: [],
});
const CONV_TILE = JSON.stringify({ kind: 'convergence', counts_top: [], series: [3, 2, 1.5], bloch: [] });
// One entangled qubit (a vector at the origin) beside a pure |0>.
const STATE_TILE = JSON.stringify({ kind: 'state', counts_top: [], series: [], bloch: [[0, 0, 0], [0, 0, 1]] });

const run = (over = {}) => ({
  runId: 'aaaa1111-0000-4000-8000-000000000001',
  projectId: 'p1', notebookId: 'nb1', cellId: 'c1', kind: 'cell',
  target: 'core:node-a', nodeId: 'node-a', status: 'succeeded',
  startedAt: '2026-09-03 14:02:00', endedAt: '2026-09-03 14:02:02',
  metrics: { qubits: 2, shots: 1024, keyframes: 3 },
  userId: 'u1', userName: 'Anna Kowalska', pinnedAt: null, tileJson: HIST_TILE,
  ...over,
});

// ---- the tile --------------------------------------------------------------

test('the tile is read out of the stored column, in either field spelling', () => {
  const tile = parseTile(run());
  assert.equal(tile.kind, 'histogram');
  assert.deepEqual(tile.countsTop, [
    { bitstring: '00', probability: 0.5 },
    { bitstring: '11', probability: 0.5 },
  ]);
  assert.equal(parseTile({ tile_json: CONV_TILE }).kind, 'convergence');
});

test('a run with no tile, or a broken one, is reported as having none', () => {
  assert.equal(parseTile(run({ tileJson: null })), null);
  assert.equal(parseTile(run({ tileJson: '{oops' })), null);
});

test('a kind this build cannot draw is reported as unknown, never mis-drawn', () => {
  const tile = parseTile(run({ tileJson: JSON.stringify({ kind: 'tomography' }) }));
  assert.equal(tile.kind, '');
  assert.equal(tileSvg(tile), '');
  assert.deepEqual(TILE_KINDS, ['histogram', 'convergence', 'state']);
});

test('each kind draws its own shape and nothing else', () => {
  assert.match(tileSvg(parseTile(run())), /<rect class="rt-bar"/);
  assert.match(tileSvg(parseTile({ tileJson: CONV_TILE })), /<polyline class="rt-line"/);
  const state = tileSvg(parseTile({ tileJson: STATE_TILE }));
  assert.match(state, /<circle class="rt-sph"/);
  assert.equal((state.match(/rt-vec is-mixed/g) || []).length, 1, 'only the mixed qubit is dashed');
  assert.equal((state.match(/class="rt-vec"/g) || []).length, 1, 'the pure one keeps a solid arrow');
});

test('a series is fitted to the box, and a flat one sits on the middle line', () => {
  const points = seriesPoints([1, 0], 100, 80).split(' ');
  assert.equal(points[0], '0.0,0.0');
  assert.equal(points[1], '100.0,80.0');
  assert.equal(seriesPoints([5, 5, 5], 100, 80), '0.0,40.0 50.0,40.0 100.0,40.0');
  assert.equal(seriesPoints([], 100, 80), '');
});

// ---- filters ---------------------------------------------------------------

const NOW = Date.parse('2026-09-03T15:00:00Z');

test('the default filters are the mockup: every tier, every type, thirty days', () => {
  assert.deepEqual(resultFilterState(), { query: '', tier: 'all', kind: 'all', period: 'month' });
});

test('the tier filter reads the run target', () => {
  const runs = [run(), run({ runId: 'b', target: 'browser', nodeId: null })];
  assert.equal(filterResults(runs, { tier: 'T1' }, { now: NOW }).length, 1);
  assert.equal(filterResults(runs, { tier: 'T0' }, { now: NOW })[0].runId, 'b');
});

test('asking for one tile kind excludes a run with no tile at all', () => {
  const runs = [run(), run({ runId: 'b', tileJson: null })];
  assert.equal(filterResults(runs, { kind: 'histogram' }, { now: NOW }).length, 1);
  assert.equal(filterResults(runs, { kind: 'all' }, { now: NOW }).length, 2);
});

test('the period is measured against the run start', () => {
  const old = run({ runId: 'old', startedAt: '2026-07-01 10:00:00' });
  assert.equal(filterResults([run(), old], { period: 'month' }, { now: NOW }).length, 1);
  assert.equal(filterResults([run(), old], { period: 'all' }, { now: NOW }).length, 2);
});

// `parseServerTs` is the one place the app reads a server timestamp; doing it
// again inline here would silently pass a run whose stamp carries a zone.
test('a timestamp that already carries a zone is still filtered by period', () => {
  const zoned = run({ runId: 'zoned', startedAt: '2026-07-01T10:00:00Z' });
  assert.equal(filterResults([zoned], { period: 'month' }, { now: NOW }).length, 0);
  assert.equal(filterResults([zoned], { period: 'all' }, { now: NOW }).length, 1);
  // A run with no start at all is not silently dropped by the period filter.
  assert.equal(filterResults([run({ runId: 'none', startedAt: null })], { period: 'day' }, { now: NOW }).length, 1);
});

test('the search covers the id, the target and the person', () => {
  const runs = [run()];
  assert.equal(filterResults(runs, { query: 'anna' }, { now: NOW }).length, 1);
  assert.equal(filterResults(runs, { query: 'node-a' }, { now: NOW }).length, 1);
  assert.equal(filterResults(runs, { query: 'nothing' }, { now: NOW }).length, 0);
});

test('a run is named by what it computed, since it carries no title', () => {
  assert.equal(resultTitle(run()), '2 kubity · 1024 shoty');
  assert.equal(resultTitle(run({ metrics: { qubits: 3 } })), 'Stan 3 kubitów');
  assert.equal(resultTitle(run({ metrics: {}, notebookId: null, kind: 'circuit' })), 'studio obwodów');
});

// ---- the gallery -----------------------------------------------------------

function fakeScreen(runs) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  return {
    root,
    lab: { nodes: NODES },
    projects: [{ projectId: 'p1', name: 'VQE H2' }],
    runs,
    resultFilters: resultFilterState({ period: 'all' }),
    selectedResults: new Set(),
    opened: [],
    pinned: [],
    tabs: [],
    openRunResult(runId, options) { this.opened.push([runId, options]); },
    toggleResultPin(runId) { this.pinned.push(runId); },
    selectProjectTab(tab) { this.tabs.push(tab); },
  };
}

function draw(runs) {
  const screen = fakeScreen(runs);
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawResults(screen, host);
  return { screen, host };
}

const cleanup = () => { window.document.body.innerHTML = ''; };

test('the gallery splits pinned from the rest and counts both in the footer', () => {
  const { host } = draw([run(), run({ runId: 'b', pinnedAt: '2026-09-03 14:10:00' })]);
  const heads = [...host.querySelectorAll('.tq-section-head')];
  assert.equal(heads.length, 2);
  assert.match(heads[0].textContent, /Przypięte/);
  assert.match(heads[1].textContent, /Wszystkie/);
  assert.equal(host.querySelectorAll('.res-tile').length, 2);
  assert.match(host.querySelector('.tq-table-footer').textContent, /2 wyniki/);
  assert.match(host.querySelector('.tq-table-footer').textContent, /przypięte: 1/);
  cleanup();
});

test('a run that never succeeded is not a result', () => {
  const { host } = draw([run({ status: 'failed' })]);
  assert.equal(host.querySelectorAll('.res-tile').length, 0);
  assert.ok(host.querySelector('tf-empty-state'));
  cleanup();
});

test('a run with no tile keeps its card and says why the picture is missing', () => {
  const { host } = draw([run({ tileJson: null })]);
  assert.equal(host.querySelectorAll('.res-tile').length, 1);
  assert.match(host.querySelector('.rt-chart').textContent, /miniatury/);
  cleanup();
});

test('a tile click opens the full result view', () => {
  const { screen, host } = draw([run()]);
  host.querySelector('.res-tile').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(screen.opened[0][0], run().runId);
  cleanup();
});

test('the star pins without opening the run', () => {
  const { screen, host } = draw([run()]);
  host.querySelector('[data-pin]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.pinned, [run().runId]);
  assert.deepEqual(screen.opened, []);
  cleanup();
});

test('two selected runs enable the comparison and carry the selection into it', () => {
  const screen = fakeScreen([run(), run({ runId: 'b' })]);
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  screen.selectedResults.add(run().runId);
  screen.selectedResults.add('b');
  drawResults(screen, host);
  const button = host.querySelector('[data-act="compare"]');
  assert.equal(button.hasAttribute('disabled'), false);
  assert.match(button.textContent, /2/);
  button.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(screen.opened[0][1].tab, 'compare');
  assert.equal(screen.opened[0][1].compare.length, 2);
  cleanup();
});

test('a single selected run cannot be compared with itself', () => {
  const screen = fakeScreen([run()]);
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  screen.selectedResults.add(run().runId);
  drawResults(screen, host);
  assert.ok(host.querySelector('[data-act="compare"]').hasAttribute('disabled'));
  cleanup();
});

test('the comparison ceiling is the wire ceiling', () => {
  assert.equal(COMPARE_MAX, 8);
});

test('a redraw does not double the delegated listener', () => {
  const screen = fakeScreen([run()]);
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawResults(screen, host);
  drawResults(screen, host);
  drawResults(screen, host);
  host.querySelector('.res-tile').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(screen.opened.length, 1, 'one click, one open');
  cleanup();
});

test('a tile answers the keyboard the way its role promises', () => {
  const { screen, host } = draw([run()]);
  host.querySelector('.res-tile').dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  assert.equal(screen.opened.length, 1);
  cleanup();
});

// The mockup's empty state is a start, not a shrug: two of its three actions
// have a destination in this build, and both must actually go there.
test('a project with no results offers the two starts the mockup names', () => {
  const { screen, host } = draw([]);
  const empty = host.querySelector('tf-empty-state');
  assert.ok(empty);
  const buttons = [...host.querySelectorAll('tf-button[data-act^="start-"]')];
  assert.deepEqual(buttons.map((b) => b.textContent.trim()), ['Uruchom z notatnika', 'Zbuduj obwód']);
  buttons[0].dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  buttons[1].dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.tabs, ['notebook', 'studio']);
  cleanup();
});

test('a filtered-out listing offers no start — the runs exist, the filter hides them', () => {
  const screen = fakeScreen([run()]);
  screen.resultFilters = resultFilterState({ period: 'all', query: 'nothing matches' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawResults(screen, host);
  assert.ok(host.querySelector('tf-empty-state'));
  assert.equal(host.querySelector('[data-act="start-notebook"]'), null);
  cleanup();
});
