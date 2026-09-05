// =============================================================================
// File: modules/tentaquant/run-detail.test.js
// Description: The run detail under the Q08 table: the parameter rows (only
// what the row can prove), the event line built from the two timestamps a run
// carries, the outputs drawn by tf-mime-output, and the artifacts as downloads
// through the signed URL `Run::Artifact` mints — one URL per click, never
// cached on the row. The comparison block of the mockup is deliberately absent:
// `Run::Compare` is not on the wire.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  artifactFileName, artifactLabel, detailRows, drawRunDetail, streamNote, targetText,
} = await import('./run-detail.js');
const { COUNTS_MIME } = await import('./quantum-view.js');

const NODES = [{ nodeId: 'node-a', nodeName: 'spark-01', isLocal: true, online: true, instanceStatus: 'ready' }];

const countsArtifact = {
  cellId: 'c1', seq: 0, mime: COUNTS_MIME, sizeBytes: 48, sha256: null,
  inlineJson: JSON.stringify({ counts: { '00': 512, '11': 512 }, shots: 1024, numQubits: 2 }),
};
const stateArtifact = {
  cellId: 'c1', seq: 1, mime: 'application/x-tentaquant-state+json', sizeBytes: 9_000_000,
  sha256: 'a'.repeat(64), inlineJson: null,
};

const run = (over = {}) => ({
  runId: '2f9a1c3d-0000-4000-8000-000000000001',
  projectId: 'grover-4q', notebookId: 'nb1', cellId: 'c1abcdef', kind: 'cell',
  target: 'core:node-a', nodeId: 'node-a', status: 'succeeded',
  startedAt: '2026-09-03 14:02:00', endedAt: '2026-09-03 14:02:02', error: null,
  metrics: {
    durationMs: 1400, qubits: 4, clbits: 4, shots: 1024, seed: 3141592653, memoryBytes: 262144,
    gates: 38, keyframes: 38, method: 'statevector', precision: 'double', backend: 'cpu',
  },
  userId: 'u1', userName: 'Anna Kowalska', pinnedAt: null,
  artifacts: [countsArtifact, stateArtifact],
  ...over,
});

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

test('the parameters are what the row carries and nothing it does not', () => {
  const rows = new Map(detailRows(run(), { projectName: 'Grover 4-kubitowy', nodes: NODES }));
  assert.equal(rows.get('Projekt'), 'Grover 4-kubitowy');
  assert.equal(rows.get('Źródło'), 'notatnik · komórka c1abcdef');
  assert.equal(rows.get('Cel'), 'T1 · Core · spark-01');
  assert.equal(rows.get('Osoba'), 'Anna Kowalska');
  assert.equal(rows.get('Rejestr'), '4 kubity (+4 bity klasyczne)');
  assert.equal(rows.get('Shoty'), '1024');
  // The seed is what makes the histogram repeatable, and a run mints a new one
  // per run — so the number the run actually drew with is stated, not implied.
  assert.equal(rows.get('Ziarno losowe'), '3141592653');
  assert.equal(rows.get('Metoda'), 'statevector · double');
  assert.equal(rows.get('Bramki'), '38 · 38 klatek');
  assert.equal(rows.get('Czas'), '1,4 s');
  assert.equal(rows.get('Błąd'), undefined, 'a run that worked has no error row');
});

test('a run with nothing measured shows no rows it would have to invent', () => {
  const rows = new Map(detailRows(run({ status: 'queued', endedAt: null, metrics: {} }), { nodes: NODES }));
  assert.equal(rows.get('Projekt'), 'bez projektu');
  assert.equal(rows.get('Zakończony'), undefined);
  assert.equal(rows.get('Shoty'), undefined);
  assert.equal(rows.get('Ziarno losowe'), undefined, 'a run that drew nothing has no seed to state');
  assert.equal(rows.get('Rejestr'), undefined);
});

test('the two "why not" notes of the metrics reach the reader', () => {
  const rows = new Map(detailRows(run({
    metrics: { stateNote: 'a measured circuit has no single state', evolutionNote: 'over the keyframe budget' },
  }), { nodes: NODES }));
  assert.equal(rows.get('Stan'), 'a measured circuit has no single state');
  assert.equal(rows.get('Ewolucja'), 'over the keyframe budget');
});

test('a failed run puts the reason in the parameters too', () => {
  const rows = new Map(detailRows(run({ status: 'failed', error: 'CircuitError: too wide' }), { nodes: NODES }));
  assert.equal(rows.get('Błąd'), 'CircuitError: too wide');
});

test('a target this build does not know keeps its wire name instead of a borrowed tier', () => {
  assert.equal(targetText(run(), NODES), 'T1 · Core · spark-01');
  assert.equal(targetText(run({ target: 'browser', nodeId: null }), NODES), 'T0 · przeglądarka · przeglądarka');
  // `runTier` answers '' rather than guessing, and this row says the same.
  assert.equal(targetText(run({ target: 'qpu:ibm-torino' }), NODES), 'qpu:ibm-torino');
  const rows = new Map(detailRows(run({ target: 'qpu:ibm-torino' }), { nodes: NODES }));
  assert.equal(rows.get('Cel'), 'qpu:ibm-torino');
});

test('what the stream says about itself is a sentence, not a silence', () => {
  assert.equal(streamNote({ gap: false, error: '' }, 'completed'), '');
  assert.match(streamNote({ gap: true, error: '' }, 'gap'), /klatek przepadła/);
  assert.match(streamNote({ gap: false, error: '' }, 'not_found'), /nie prowadzi już strumienia/);
  // A transport failure is the most specific thing we know, so it wins.
  assert.match(streamNote({ gap: true, error: 'socket closed' }, 'not_found'), /Strumień runu przerwany: socket closed/);
});

test('artifacts are named and saved under a name that says which run they are', () => {
  assert.equal(artifactLabel(countsArtifact), 'Histogram pomiarów');
  assert.equal(artifactLabel(stateArtifact), 'Wektor stanu');
  // A mime this build does not know keeps its wire name rather than vanishing.
  assert.equal(artifactLabel({ mime: 'application/x-tentaquant-tile+svg' }), 'application/x-tentaquant-tile+svg');
  assert.equal(artifactFileName(run(), countsArtifact), 'run-2f9a1c3d-counts.json');
  assert.equal(artifactFileName(run(), stateArtifact), 'run-2f9a1c3d-state.json');
  assert.equal(artifactFileName(run(), { mime: 'x/y' }), 'run-2f9a1c3d-artifact.bin');
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
    lab: { nodes: NODES, myPermissions: ['quant.read', 'quant.run'] },
    projects: [{ projectId: 'grover-4q', name: 'Grover 4-kubitowy' }],
    row: run(),
    requests: [],
    subscriptions: [],
    disposed: [],
    setRunViewDispose(dispose) { this.disposed.push(dispose); },
    selectRun() {},
    reloadRuns() {},
    tqSubscribe(kind, payload, handlers) {
      this.subscriptions.push({ kind, payload, handlers });
      return Promise.resolve(() => {});
    },
    onTransport() { return () => {}; },
    tq(kind, payload) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantRunGetRequest') return Promise.resolve({ run: this.row });
      if (kind === 'tentaQuantRunArtifactRequest') {
        return Promise.resolve({
          url: '/tentaquant/artifact/org/inst/aaa?token=t', mime: 'application/json',
          sizeBytes: 9_000_000, expiresAtMs: 0,
        });
      }
      return Promise.resolve({});
    },
    ...over,
  };
}

const cleanup = () => { window.document.body.innerHTML = ''; };

async function draw(screen) {
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawRunDetail(screen, host, run().runId, {});
  await view.ready;
  return { host, view };
}

test('the detail names the run, its target and its status, and drops the comparison block', async () => {
  const { host } = await draw(fakeScreen());
  const head = host.querySelector('.run-detail-head');
  assert.match(head.textContent, /2f9a1c3d/);
  assert.match(head.textContent, /T1 · Core/);
  assert.match(head.textContent, /spark-01/);
  assert.equal(head.querySelector('tf-chip').getAttribute('label'), 'OK');
  // `Run::Compare` does not exist, so neither does the mockup's comparison.
  assert.doesNotMatch(host.textContent, /Porównaj|symulator vs/);
  cleanup();
});

test('the event line is drawn from the timestamps the run has', async () => {
  const { host } = await draw(fakeScreen());
  const items = [...host.querySelectorAll('.run-timeline .tl-item')];
  assert.equal(items.length, 4);
  assert.match(items[0].textContent, /Zlecony/);
  assert.match(items[1].textContent, /przeszedł/, 'the queue moment has no timestamp of its own');
  assert.match(items[3].textContent, /Zakończony powodzeniem/);
  assert.ok(items.every((i) => i.classList.contains('is-done')));
  cleanup();
});

test('an inline output is drawn and a stored one is a download', async () => {
  const { host } = await draw(fakeScreen());
  const outputs = host.querySelectorAll('#tq-run-outputs tf-mime-output');
  assert.equal(outputs.length, 1, 'only what travelled inline can be drawn');
  assert.equal(outputs[0].bundle[COUNTS_MIME].shots, 1024);
  const artifacts = [...host.querySelectorAll('.run-artifact')];
  assert.equal(artifacts.length, 2);
  assert.match(artifacts[1].textContent, /Wektor stanu/);
  assert.ok(artifacts[1].querySelector('[data-artifact]'), 'the stored one is fetched by hash');
  assert.equal(artifacts[0].querySelector('[data-artifact]'), null, 'the inline one is already here');
  cleanup();
});

test('a download mints one signed URL per click', async () => {
  const screen = fakeScreen();
  const { host } = await draw(screen);
  const saved = [];
  const anchors = window.HTMLAnchorElement.prototype;
  const realClick = anchors.click;
  anchors.click = function click() { saved.push({ name: this.download, href: this.href }); };
  try {
    host.querySelector('[data-artifact]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
    for (let i = 0; i < 50 && !saved.length; i += 1) await new Promise((r) => setTimeout(r, 5));
  } finally {
    anchors.click = realClick;
  }
  const asked = screen.requests.filter(([kind]) => kind === 'tentaQuantRunArtifactRequest');
  assert.equal(asked.length, 1);
  assert.equal(asked[0][1].sha256, stateArtifact.sha256);
  assert.equal(saved[0].name, 'run-2f9a1c3d-state.json');
  assert.match(saved[0].href, /token=t/);
  cleanup();
});

test('a live run is followed, a finished one is not', async () => {
  const finished = fakeScreen();
  await draw(finished);
  assert.deepEqual(finished.subscriptions, [], 'nothing to follow');
  cleanup();

  const live = fakeScreen({ row: run({ status: 'running', endedAt: null, artifacts: [] }) });
  const { host, view } = await draw(live);
  for (let i = 0; i < 50 && !live.subscriptions.length; i += 1) await new Promise((r) => setTimeout(r, 5));
  assert.equal(live.subscriptions[0].payload.runId, run().runId);
  live.subscriptions[0].handlers.onChunk({ event: { seq: 1, kind: 'output', output: countsArtifact } });
  assert.equal(host.querySelector('#tq-run-outputs tf-mime-output').bundle[COUNTS_MIME].shots, 1024);
  // A run in flight offers the stop; a finished one does not.
  assert.ok(host.querySelector('[data-act="cancel"]'));
  view.dispose();
  cleanup();
});

test('a stream the node refuses says so instead of leaving the run reading "w toku"', async () => {
  const live = fakeScreen({ row: run({ status: 'running', endedAt: null, artifacts: [] }) });
  const { host, view } = await draw(live);
  for (let i = 0; i < 50 && !live.subscriptions.length; i += 1) await new Promise((r) => setTimeout(r, 5));
  live.subscriptions[0].handlers.onError(new Error('this run is not on this node'));
  const alert = host.querySelector('tf-alert');
  assert.ok(alert, 'the refusal reaches the panel');
  assert.match(alert.getAttribute('message'), /Strumień runu przerwany: this run is not on this node/);
  // The row keeps the status it had — the stream ending is not an outcome.
  assert.match(host.querySelector('.run-detail-head').textContent, /w toku/);
  assert.equal(view.stream, null, 'the finished session is released, not held');
  view.dispose();
  cleanup();
});

test('the stop and the pin belong to the person who started the run', async () => {
  const mine = await draw(fakeScreen({ row: run({ status: 'running', endedAt: null }) }));
  assert.ok(mine.host.querySelector('[data-act="pin"]'));
  assert.ok(mine.host.querySelector('[data-act="cancel"]'));
  mine.view.dispose();
  cleanup();

  const theirs = await draw(fakeScreen({ row: run({ status: 'running', endedAt: null, userId: 'u2' }) }));
  assert.equal(theirs.host.querySelector('[data-act="pin"]'), null);
  assert.equal(theirs.host.querySelector('[data-act="cancel"]'), null, 'a supervisor reads, it does not stop');
  theirs.view.dispose();
  cleanup();
});

test('a view that closes lets go of the panel every run is opened into', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  // The table opens run after run into this ONE element without redrawing it,
  // so a view that leaves its listener behind leaves itself, the screen and the
  // DOM it closed over attached once per row a person opened.
  const added = [];
  const removed = [];
  const hostAdd = host.addEventListener.bind(host);
  const hostRemove = host.removeEventListener.bind(host);
  host.addEventListener = (type, fn, opts) => { if (type === 'click') added.push(fn); hostAdd(type, fn, opts); };
  host.removeEventListener = (type, fn, opts) => { if (type === 'click') removed.push(fn); hostRemove(type, fn, opts); };

  const first = drawRunDetail(screen, host, run().runId, {});
  await first.ready;
  first.dispose();
  assert.deepEqual(removed, added, 'the closed view took its listener with it');

  const second = drawRunDetail(screen, host, run().runId, {});
  await second.ready;
  assert.equal(added.length, 2);
  second.dispose();
  assert.deepEqual(removed, added);
  cleanup();
});

test('a run that cannot be read is reported instead of drawn empty', async () => {
  const screen = fakeScreen({ tq: () => Promise.reject(new Error('NotFound')) });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  await drawRunDetail(screen, host, 'nope', {}).ready;
  assert.equal(host.querySelector('tf-alert').getAttribute('message'), 'NotFound');
  cleanup();
});
