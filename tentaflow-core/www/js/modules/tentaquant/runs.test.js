// =============================================================================
// File: modules/tentaquant/runs.test.js
// Description: Q08 — the run row as a model (tier, node, source, duration, the
// event line and who may act on it) and the table that draws it. The two halves
// are separate on purpose: everything a row SAYS is a pure function of the
// `RunInfo` the wire sent, so it is asserted without a DOM, and the view is
// then checked for the mockup's contract — one toolbar, a scrollable table, a
// footer summary, an empty state and the actions a person is actually allowed.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  canControlRun, filterRuns, runDurationMs, runIsLive, runNodeName, runSourceLabel, runStatusLabel,
  runStatusTone, runTier, runTimeline, runUsers,
} = await import('./run-model.js');
const { shortId } = await import('./format.js');
const { drawRuns, runFilterState, runFooter, runTableRow } = await import('./runs.js');

const NODES = [
  { nodeId: 'node-a', nodeName: 'spark-01', isLocal: true, online: true, instanceStatus: 'ready' },
];

const run = (over = {}) => ({
  runId: '2f9a1c3d-0000-4000-8000-000000000001',
  projectId: 'grover-4q',
  notebookId: 'nb1',
  cellId: 'c1abcdef',
  kind: 'cell',
  target: 'core:node-a',
  nodeId: 'node-a',
  status: 'succeeded',
  startedAt: '2026-09-03 14:02:00',
  endedAt: '2026-09-03 14:02:02',
  error: null,
  metrics: { durationMs: 1400, qubits: 4, clbits: 4, shots: 1024, gates: 38, keyframes: 38, method: 'statevector', precision: 'double', backend: 'cpu' },
  userId: 'u1',
  userName: 'Anna Kowalska',
  pinnedAt: null,
  artifacts: [],
  ...over,
});

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

test('the tier comes off `runs.target`, and an unknown prefix is not guessed', () => {
  assert.equal(runTier(run()), 'T1');
  assert.equal(runTier(run({ target: 'browser' })), 'T0');
  assert.equal(runTier(run({ target: 'qpu:ibm_torino' })), '');
});

test('the node is named as the laboratory knows it, and by its id when it left the fleet', () => {
  assert.equal(runNodeName(run(), NODES), 'spark-01');
  assert.equal(runNodeName(run({ nodeId: 'node-z' }), NODES), 'node-z');
  assert.equal(runNodeName(run({ target: 'browser', nodeId: null }), NODES), 'przeglądarka');
});

test('the source line names the cell a run came out of', () => {
  assert.equal(runSourceLabel(run()), 'notatnik · komórka c1abcdef');
  assert.equal(runSourceLabel(run({ cellId: null })), 'notatnik');
  assert.equal(runSourceLabel(run({ notebookId: null, kind: 'circuit' })), 'studio obwodów');
  // A kind this build does not know is never rendered as a raw wire word.
  assert.equal(runSourceLabel(run({ notebookId: null, kind: 'quantum-annealing' })), 'run');
});

test('a failed run says WHAT failed, which is the point of the column', () => {
  assert.equal(runStatusLabel(run()), 'OK');
  assert.equal(runStatusLabel(run({ status: 'failed', error: 'CircuitError' })), 'błąd · CircuitError');
  assert.equal(runStatusLabel(run({ status: 'running' })), 'w toku');
  assert.equal(runStatusTone('succeeded'), 'ok');
  assert.equal(runStatusTone('nonsense'), 'neutral');
});

test('every status wears a colour tf-chip actually paints — a failed run is RED', async () => {
  await import('/js/components/tf-chip.js');
  const host = window.document.createElement('div');
  window.document.body.appendChild(host);
  // The tone is only a word until a chip turns it into a class: a status the
  // component does not know falls back to `info` (blue) with no error, so
  // asserting the string we passed would prove nothing about the colour.
  const classOf = (status) => {
    host.innerHTML = runTableRow(run({ status, error: 'CircuitError' }), { nodes: NODES }).status;
    return host.querySelector('tf-chip').querySelector('span').className;
  };
  assert.equal(classOf('failed'), 'tf-chip err', 'a failed run is red, never blue');
  for (const status of ['created', 'queued', 'running', 'succeeded', 'failed', 'cancelled']) {
    assert.equal(classOf(status), `tf-chip ${runStatusTone(status)}`, status);
  }
  cleanup();
});

test('a finished run reports what it measured; a live one is measured against the clock', () => {
  assert.equal(runDurationMs(run()), 1400);
  const started = Date.parse('2026-09-03T14:02:00Z');
  const live = run({ status: 'running', endedAt: null, metrics: { qubits: 4 } });
  assert.equal(runDurationMs(live, started + 2600), 2600);
  assert.equal(runDurationMs(run({ startedAt: null, metrics: {} })), null);
});

test('the event line prints only the two moments a run row can prove', () => {
  const done = runTimeline(run());
  assert.deepEqual(done.map((i) => i.id), ['created', 'queued', 'running', 'done']);
  assert.deepEqual(done.map((i) => i.state), ['done', 'done', 'done', 'done']);
  assert.equal(done[0].at, '2026-09-03 14:02:00');
  assert.equal(done[1].at, null, 'the queue moment is not a timestamp the row carries');
  assert.equal(done[3].at, '2026-09-03 14:02:02');
  assert.equal(done[3].outcome, 'succeeded');

  const queued = runTimeline(run({ status: 'queued', endedAt: null }));
  assert.deepEqual(queued.map((i) => i.state), ['done', 'current', 'pending', 'pending']);
  assert.equal(queued[3].outcome, '');
  assert.equal(runTimeline(run({ status: 'cancelled' }))[3].outcome, 'cancelled');
  assert.equal(runTimeline(run({ status: 'failed' }))[3].outcome, 'failed');
});

test('only the person who started a run may reach into it', () => {
  assert.equal(canControlRun(run(), 'u1'), true);
  assert.equal(canControlRun(run(), 'u2'), false, 'a supervisor reads, it does not stop');
  assert.equal(canControlRun(run(), ''), false);
  assert.equal(runIsLive(run({ status: 'queued' })), true);
  assert.equal(runIsLive(run()), false);
});

test('the filters narrow by tier, status, person and free text', () => {
  const rows = [
    run(),
    run({ runId: 'b', target: 'browser', nodeId: null, status: 'failed', userId: 'u2', userName: 'Marek Nowak' }),
    run({ runId: 'c', status: 'running', projectId: 'vqe-h2' }),
  ];
  const names = new Map([['grover-4q', 'Grover 4-kubitowy'], ['vqe-h2', 'VQE H₂']]);
  assert.equal(filterRuns(rows, runFilterState(), names).length, 3);
  assert.deepEqual(filterRuns(rows, runFilterState({ tier: 'T0' }), names).map((r) => r.runId), ['b']);
  assert.deepEqual(filterRuns(rows, runFilterState({ status: 'running' }), names).map((r) => r.runId), ['c']);
  assert.deepEqual(filterRuns(rows, runFilterState({ user: 'u2' }), names).map((r) => r.runId), ['b']);
  assert.deepEqual(filterRuns(rows, runFilterState({ query: 'VQE' }), names).map((r) => r.runId), ['c']);
  assert.deepEqual(runUsers(rows).map((u) => u.name), ['Anna Kowalska', 'Marek Nowak']);
});

test('a row is two-line cells and never a raw uuid as a title', () => {
  const row = runTableRow(run(), { projectName: 'Grover 4-kubitowy', nodes: NODES });
  assert.equal(shortId(run().runId), '2f9a1c3d');
  assert.match(row.run, /tf-table__cell-title[^>]*>2f9a1c3d</);
  assert.match(row.run, /tf-table__cell-sub[^>]*>Anna Kowalska</);
  assert.match(row.project, /Grover 4-kubitowy/);
  assert.match(row.project, /notatnik · komórka c1abcdef/);
  // The target cell lands INSIDE the tf-table shadow root, which adopts
  // controls.css and nothing else, so the tier is a component and every class
  // in the markup comes from that one sheet — never from tentaquant.css.
  assert.match(row.target, /<tf-chip [^>]*label="T1 · Core"/);
  assert.match(row.target, /status="accent"/, 'the tier keeps the colour the mockup fixes for T1');
  assert.match(row.target, /tf-table__cell-row/);
  assert.match(row.target, /class="tf-table__cell-sub">spark-01</);
  assert.doesNotMatch(row.target, /class="tier|run-node/, 'a feature stylesheet cannot reach into the shadow root');
  assert.equal(row.size, '4 / 38');
  assert.equal(row.shots, '1024');
  assert.equal(row.time, '1,4 s');
  assert.match(row.status, /label="OK"/);
});

test('a row with nothing measured yet draws dashes, not zeros', () => {
  const row = runTableRow(run({ status: 'queued', endedAt: null, metrics: {} }), { nodes: NODES });
  assert.equal(row.size, '—');
  assert.equal(row.shots, '—');
  assert.match(row.project, /bez projektu/, 'a run outside a project says so');
});

test('every tier keeps its colour, and a target this build cannot name keeps its word', () => {
  const browser = runTableRow(run({ target: 'browser', nodeId: null }), { nodes: NODES });
  assert.match(browser.target, /status="info"/, 'T0 is the browser colour of the mockup');
  assert.match(browser.target, /label="T0 · przeglądarka"/);
  const alien = runTableRow(run({ target: 'qpu:ibm_torino' }), { nodes: NODES });
  assert.match(alien.target, /status="neutral"/, 'an unknown tier has no colour to claim');
  assert.match(alien.target, /label="qpu:ibm_torino"/, 'and the raw target is not translated away');
});

test('the footer counts the listing, the page and the pins', () => {
  const all = [run(), run({ runId: 'b', pinnedAt: '2026-09-03 15:00:00' })];
  assert.deepEqual(runFooter(all, all), ['2 runy', 'pokazuję 2', 'przypięte: 1']);
  assert.deepEqual(runFooter(all, [all[0]]), ['2 runy', 'pokazuję 1', 'przypięte: 0']);
});

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

function fakeScreen(over = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  return {
    root,
    userId: 'u1',
    instanceId: 'tentaquant-0a1b2c3d',
    lab: { instanceId: 'tentaquant-0a1b2c3d', nodes: NODES, myPermissions: ['quant.read', 'quant.run'] },
    projects: [{ projectId: 'grover-4q', name: 'Grover 4-kubitowy' }],
    runs: [run()],
    runsError: '',
    runFilters: runFilterState(),
    runId: null,
    calls: [],
    selectRun(id) { this.runId = id; this.calls.push('select:' + id); },
    // The two halves of the real screen's run-view handle (`tentaquant.js`):
    // whatever draws a detail leaves its stop here, and every path that
    // replaces the panel pulls it first.
    runViewDispose: null,
    disposals: 0,
    setRunViewDispose(dispose) { this.disposeRunView(); this.runViewDispose = dispose; },
    disposeRunView() {
      const dispose = this.runViewDispose;
      this.runViewDispose = null;
      if (dispose) { this.disposals += 1; dispose(); }
    },
    reloadRuns(opts) { this.calls.push('reload:' + (opts?.projectId ?? '')); },
    tq(kind, payload) { this.calls.push([kind, payload]); return Promise.resolve({ run: run() }); },
    ...over,
  };
}

const cleanup = () => { window.document.body.innerHTML = ''; };

function draw(screen, options = {}) {
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawRuns(screen, host, options);
  return host;
}

test('the Runy tab draws one toolbar, a scrollable table and the footer summary', () => {
  const host = draw(fakeScreen());
  assert.equal(host.querySelectorAll('.tf-toolbar').length, 1, 'one toolbar per view');
  // Seven columns do not fit a phone, so the table scrolls inside its own box
  // and the page never scrolls sideways.
  assert.ok(host.querySelector('.table-scroll > #tq-run-table'));
  assert.deepEqual(
    [...host.querySelectorAll('#tq-run-table tf-column')].map((c) => c.getAttribute('key')),
    ['run', 'project', 'target', 'size', 'shots', 'time', 'status'],
  );
  assert.match(host.querySelector('.tq-table-footer').textContent, /1 run/);
  // Primitives are components, never raw controls (CLAUDE.md rule 7).
  const raw = [...host.querySelectorAll('button, input, select')]
    .filter((el) => !el.closest('tf-button, tf-input, tf-select, tf-searchbox, tf-table, tf-chip'));
  assert.deepEqual(raw, []);
  cleanup();
});

test('the person filter belongs to a supervisor and to nobody else', () => {
  assert.equal(draw(fakeScreen()).querySelector('#tq-run-user'), null);
  cleanup();
  const supervisor = fakeScreen({
    lab: { nodes: NODES, myPermissions: ['quant.read', 'quant.run', 'quant.instruct'] },
    runs: [run(), run({ runId: 'b', userId: 'u2', userName: 'Marek Nowak' })],
  });
  const options = [...draw(supervisor).querySelectorAll('#tq-run-user option')].map((o) => o.value);
  assert.deepEqual(options, ['all', 'u1', 'u2']);
  cleanup();
});

test('an empty laboratory and an over-filtered one say different things', () => {
  const empty = draw(fakeScreen({ runs: [] }));
  assert.match(empty.querySelector('tf-empty-state').getAttribute('title'), /Brak runów/);
  assert.equal(empty.querySelector('#tq-run-table'), null, 'no headers over nothing');
  cleanup();
  const filtered = draw(fakeScreen({ runFilters: runFilterState({ status: 'failed' }) }));
  assert.match(filtered.querySelector('tf-empty-state').getAttribute('title'), /Żaden run nie pasuje/);
  cleanup();
});

test('a failed listing is reported instead of being drawn as an empty laboratory', () => {
  const host = draw(fakeScreen({ runs: [], runsError: 'PolicyDenied' }));
  assert.equal(host.querySelector('tf-alert').getAttribute('message'), 'PolicyDenied');
  cleanup();
});

test('the row actions are the two acts a person owns, and only on their own run', () => {
  const screen = fakeScreen({ runs: [run({ status: 'running', endedAt: null })] });
  const host = draw(screen);
  const table = host.querySelector('#tq-run-table');
  const mine = table.rowActions({ _run: run().runId });
  assert.deepEqual([...mine.children].map((b) => b.getAttribute('icon')), ['star', 'x']);
  // The wrapper is appended into the tf-table shadow root: only controls.css
  // reaches it, so the class has to be one of that sheet's.
  assert.equal(mine.className, 'tf-table__row-actions');
  cleanup();

  const others = fakeScreen({ runs: [run({ userId: 'u2' })], userId: 'u1' });
  assert.equal(draw(others).querySelector('#tq-run-table').rowActions({ _run: run().runId }), null);
  cleanup();
});

test('a finished run offers the pin and no stop', () => {
  const screen = fakeScreen();
  const table = draw(screen).querySelector('#tq-run-table');
  const actions = table.rowActions({ _run: run().runId });
  assert.deepEqual([...actions.children].map((b) => b.getAttribute('icon')), ['star']);
  cleanup();
});

test('changing a filter redraws without asking the server again', () => {
  const screen = fakeScreen();
  const host = draw(screen);
  host.querySelector('#tq-run-status').dispatchEvent(new window.CustomEvent('change', {
    bubbles: true, detail: { value: 'failed' },
  }));
  assert.equal(screen.runFilters.status, 'failed');
  assert.deepEqual(screen.calls, [], 'the rows are already here; a filter is not a request');
  assert.match(host.querySelector('tf-empty-state').getAttribute('title'), /Żaden run nie pasuje/);
  cleanup();
});

test('a filter that hides the open run disposes its detail instead of leaking its stream', async () => {
  const screen = fakeScreen({ runs: [run({ status: 'running', endedAt: null })], runId: run().runId });
  const host = draw(screen);
  // The detail of the open run mounts and takes the handle.
  for (let i = 0; i < 50 && !screen.runViewDispose; i += 1) await new Promise((r) => setTimeout(r, 5));
  assert.ok(screen.runViewDispose, 'the open run drew its detail');
  host.querySelector('#tq-run-status').dispatchEvent(new window.CustomEvent('change', {
    bubbles: true, detail: { value: 'failed' },
  }));
  // The row is gone from the table, so the panel it lived in is gone too — and
  // with it the subscription it was holding.
  assert.equal(host.querySelector('#tq-run-detail').innerHTML, '');
  assert.equal(screen.disposals, 1, 'the view that is no longer on screen was stopped');
  assert.equal(screen.runViewDispose, null);
  cleanup();
});

test('the reload button re-reads the listing the tab is showing', () => {
  const screen = fakeScreen();
  const host = draw(screen, { projectId: 'grover-4q' });
  host.querySelector('[data-act="reload"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(screen.calls, ['reload:grover-4q'], 'a project tab reloads that project');
  cleanup();
});
