// =============================================================================
// File: modules/tentaquant/views-quantum.test.js
// Description: The two quantum screens painting under happy-dom: the circuit
// Studio skeleton (Q07) and the notebook cell column (Q06). The browser
// simulator is not available in Node, which is exactly one of the states these
// screens must render honestly — so the assertions cover the markup contract
// (one toolbar, the mockup's two-column layout, the components instead of raw
// controls) and the absence of every tier that has no backend.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawStudio, studioState } = await import('./studio.js');
const { drawNotebook, openNotebookPicker } = await import('./notebook.js');
const { serializeCells, createCell } = await import('./cells.js');
const {
  COUNTS_MIME, MAX_LIVE_STATE_QUBITS, MAX_RUN_BATCHES, STATE_MIME, countsBundle, gridOf,
} = await import('./quantum-view.js');
const { updateCell } = await import('./cells.js');
const { makeGate } = await import('../../components/tf-quantum-circuit.js');

// h q[0]; cx q[0], q[1]; c = measure q; — the same fixture the component tests
// use, so a grid assertion here means the same thing there.
const gate = (id, qubits) => ({ kind: { Gate: { gate: makeGate(id, []), qubits } }, conditions: [] });
const BELL = {
  qubitRegisters: [{ name: 'q', start: 0, size: 2 }],
  clbitRegisters: [{ name: 'c', start: 0, size: 2 }],
  numQubits: 2,
  numClbits: 2,
  ops: [
    gate('H', [0]),
    gate('Cx', [0, 1]),
    { kind: { Measure: { qubit: 0, clbit: 0 } }, conditions: [] },
    { kind: { Measure: { qubit: 1, clbit: 1 } }, conditions: [] },
  ],
};

const project = (over = {}) => ({
  projectId: 'grover-4q', name: 'Grover 4-kubitowy', description: '', ownerUserId: 'u1',
  ownerName: 'Anna Kowalska', visibility: 'private', myRole: 'owner', shareCount: 0,
  fileCount: 0, notebookCount: 1, runCount: 0, linkedProjectId: null,
  createdAt: '2026-09-02 10:00:00', updatedAt: '2026-09-03 14:02:00', archivedAt: null,
  ...over,
});

const CELLS = [
  createCell('markdown', { id: 'm1', source: '# Grover\n' }),
  createCell('circuit', { id: 'c1' }),
];

// What `Target::List` answers on a two-node fleet: the browser, Core here, and
// Core on the other node — which is listed and refused, with the reason on the
// row, exactly as `tentaquant/targets.rs` writes it.
const TARGETS = [
  {
    target: 'browser', tier: 'T0', nodeId: null, nodeName: 'browser', isLocal: true,
    online: true, available: true, maxQubits: 24, precision: 'single', reason: null,
  },
  {
    target: 'core:node-a', tier: 'T1', nodeId: 'node-a', nodeName: 'node-a', isLocal: true,
    online: true, available: true, maxQubits: 28, precision: 'double', reason: null,
  },
  {
    target: 'core:node-b', tier: 'T1', nodeId: 'node-b', nodeName: 'node-b', isLocal: false,
    online: true, available: false, maxQubits: 28, precision: 'double',
    reason: 'runs on another node cannot stream their evolution here yet',
  },
];

// The row `Circuit::Simulate` answers with, and the two frames its stream then
// carries: one output and one recorded keyframe.
const T1_RUN = {
  runId: '7b04e2aa-0000-4000-8000-000000000001',
  projectId: 'grover-4q', notebookId: null, cellId: null, kind: 'circuit',
  target: 'core:node-a', nodeId: 'node-a', status: 'succeeded',
  startedAt: '2026-09-03 14:02:00', endedAt: '2026-09-03 14:02:01', error: null,
  metrics: { durationMs: 1400, qubits: 2, clbits: 2, shots: 1024, gates: 2, keyframes: 2, method: 'statevector', precision: 'double', backend: 'cpu' },
  userId: 'u1', userName: 'Anna Kowalska', pinnedAt: null, artifacts: [],
};

const COUNTS_FRAME = {
  seq: 1,
  kind: 'output',
  output: {
    cellId: T1_RUN.runId,
    seq: 0,
    mime: 'application/x-tentaquant-counts+json',
    sizeBytes: 48,
    sha256: null,
    inlineJson: JSON.stringify({ counts: { '00': 512, '11': 512 }, shots: 1024, numQubits: 2, numClbits: 2 }),
  },
};

const KEYFRAME_FRAME = {
  seq: 2,
  kind: 'state_keyframe',
  keyframe: {
    step: 1,
    gate: { name: 'H', qubits: [0], matrix: [] },
    bloch: [[1, 0, 0], [0, 0, 1]],
    purity: [1, 1],
    pairs: [],
    top: [{ index: 0, amplitude: [0.7071, 0], partners: [] }],
    probsTop: [{ bitstring: '00', probability: 0.5 }],
  },
};

const RESOLUTION_T0 = {
  target: 'browser', tier: 'T0', nodeId: null, unavailable: [],
  reason: '2 qubits fit the browser (up to 20), so nothing leaves this machine',
};

function fakeScreen(over = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  const screen = {
    root,
    instanceId: 'tentaquant-0a1b2c3d',
    projectId: 'grover-4q',
    project: project(),
    projectTab: 'studio',
    notebooks: [{ notebookId: 'nb1', name: 'Grover', currentVersion: 3, updatedAt: '2026-09-03 14:02:00' }],
    files: [],
    notebookId: 'nb1',
    studio: studioState(),
    projectViewDispose: null,
    requests: [],
    reloadFiles() {},
    reloadNotebooks() {},
    openStudioWithCell(payload) { this.requests.push(['studio', payload]); },
    subscriptions: [],
    tqSubscribe(kind, payload, handlers) {
      this.subscriptions.push({ kind, payload, handlers });
      return Promise.resolve(() => {});
    },
    onTransport() { return () => {}; },
    userId: 'u1',
    lab: { instanceId: 'tentaquant-0a1b2c3d', displayName: 'Kwanty R&D', nodes: [], myPermissions: ['quant.read', 'quant.run'] },
    openRun(runId) { this.requests.push(['open-run', runId]); },
    tq(kind, payload) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantTargetListRequest') return Promise.resolve({ targets: TARGETS });
      if (kind === 'tentaQuantTargetResolveRequest') return Promise.resolve(RESOLUTION_T0);
      if (kind === 'tentaQuantNotebookGetRequest') {
        return Promise.resolve({
          notebook: { notebookId: 'nb1', name: 'Grover', currentVersion: 3, updatedAt: '2026-09-03 14:02:00' },
          version: 3,
          cellsJson: serializeCells(CELLS),
        });
      }
      if (kind === 'tentaQuantCircuitSimulateRequest') {
        return Promise.resolve({ run: { ...T1_RUN, status: 'running', endedAt: null, metrics: null } });
      }
      if (kind === 'tentaQuantNotebookSaveRequest') {
        // What the handler answers: the notebook row at its NEW version.
        return Promise.resolve({
          notebook: { notebookId: 'nb1', name: 'Grover', currentVersion: 4, updatedAt: '2026-09-04 09:00:00' },
        });
      }
      return Promise.resolve({});
    },
    ...over,
  };
  return screen;
}

const cleanup = () => { window.document.body.innerHTML = ''; };

/// Waits for something a dynamic import inside a view produces. Polling beats a
/// fixed delay: the first load of a module graph takes as long as it takes.
async function until(predicate, what) {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    const value = predicate();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(`timed out waiting for ${what}`);
}

// ---------------------------------------------------------------------------
// Q07 — the Studio
// ---------------------------------------------------------------------------

test('the Studio draws one toolbar, the circuit and the state panel of the mockup', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawStudio(screen, host);
  assert.equal(host.querySelectorAll('.tf-toolbar').length, 1, 'one toolbar per view');
  assert.ok(host.querySelector('.studio-layout .studio-main tf-quantum-circuit'));
  assert.equal(host.querySelectorAll('.studio-layout .state-panel .section-card').length, 4);
  // Primitives are components, never raw controls (CLAUDE.md rule 7): every
  // native control on the screen was built by a tf-* element, none by the view.
  const raw = [...host.querySelectorAll('button, input, select, textarea')]
    .filter((el) => !el.closest('tf-button, tf-input, tf-select, tf-segmented, tf-slider, tf-code-editor, tf-quantum-circuit, tf-mime-output, tf-chip'));
  assert.deepEqual(raw, []);
  cleanup();
});

test('the Studio offers the browser tier before the wire answers at all', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawStudio(screen, host);
  const options = [...host.querySelectorAll('#tq-studio-target option')].map((o) => o.value);
  // This page IS the browser tier, so it needs no confirmation; `auto` is the
  // default because the placement rule belongs to the server.
  assert.deepEqual(options, ['auto', 'browser']);
  assert.match(host.querySelector('#tq-studio-target').textContent, /T0 · przeglądarka \(≤ 24 kubity\)/);
  // No promises of tiers this build does not have.
  assert.doesNotMatch(host.querySelector('#tq-studio-target').textContent, /QPU|GPU/);
  cleanup();
});

test('the Studio lists every target the wire answers and disables the refused one with its reason', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawStudio(screen, host);
  const select = host.querySelector('#tq-studio-target');
  await until(() => select.querySelector('option[value="core:node-a"]'), 'the targets from the wire');
  const options = [...select.querySelectorAll('option')].map((o) => o.value);
  assert.deepEqual(options, ['auto', 'browser', 'core:node-a', 'core:node-b']);
  const refused = select.querySelector('option[value="core:node-b"]');
  assert.equal(refused.disabled, true, 'a target that cannot take a run is not selectable');
  // The server's own words, not a sentence of ours: the user has to be able to
  // read WHY the other node is not on offer.
  assert.match(refused.textContent, /runs on another node cannot stream their evolution here yet/);
  assert.equal(select.querySelector('option[value="core:node-a"]').disabled, false);
  cleanup();
});

test('the `auto` rule is resolved before the run and shown next to the select', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawStudio(screen, host);
  const hint = host.querySelector('#tq-studio-target-hint');
  await until(() => /T0/.test(hint.textContent), 'the resolution of the auto rule');
  assert.equal(hint.textContent, 'auto → T0 · przeglądarka');
  const asked = screen.requests.find(([kind]) => kind === 'tentaQuantTargetResolveRequest');
  assert.ok(asked, 'the rule is the server\'s, so the screen asks for it');
  assert.equal(asked[1].fromBrowser, true);
  assert.equal(asked[1].needsKernel, false);
  cleanup();
});

test('a T1 run streams its histogram and its keyframes into the panel', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  // No wasm engine in this process, so the grid is set directly: what is under
  // test is the stream, not the parser.
  view.grid = gridOf(BELL);
  await view.runOnCore();

  const asked = screen.requests.find(([kind]) => kind === 'tentaQuantCircuitSimulateRequest');
  assert.ok(asked, 'a T1 run is started on the wire, never computed here');
  assert.equal(asked[1].projectId, 'grover-4q');
  assert.equal(screen.subscriptions[0].kind, 'tentaQuantRunSubscribeRequest');
  assert.equal(screen.subscriptions[0].payload.runId, T1_RUN.runId);

  const { handlers } = screen.subscriptions[0];
  handlers.onChunk({ event: COUNTS_FRAME });
  assert.equal(host.querySelector('#tq-studio-counts').bundle[COUNTS_MIME].shots, 1024);

  handlers.onChunk({ event: KEYFRAME_FRAME });
  // The panel is now showing frames a NODE recorded, and says so.
  assert.equal(host.querySelector('#tq-studio-state-tier').textContent, 'T1 · Core');
  assert.equal(host.querySelector('#tq-studio-amps').bundle[STATE_MIME].top.length, 1);
  // The distribution the frame carried is drawn next to the state it belongs
  // to — the measured counts of the run stay in their own card below.
  assert.equal(host.querySelector('#tq-studio-kf-probs').hidden, false);
  assert.deepEqual(host.querySelector('#tq-studio-kf-hist').bundle[COUNTS_MIME].counts, { '00': 0.5 });
  assert.equal(host.querySelectorAll('#tq-studio-bloch tf-bloch-sphere').length, 2);
  // Discrete frames: the slider steps between them and the play button, which
  // would be a slideshow pretending to be an animation, is down.
  assert.equal(view.state.mode, 'step', 'the timeline is opened rather than hidden behind a mode');
  assert.equal(host.querySelector('#tq-studio-step').getAttribute('max'), '0');
  assert.match(host.querySelector('#tq-studio-step-value').textContent, /klatka 1 \/ 1 · H q0/);
  assert.equal(host.querySelector('[data-act="play"]').hasAttribute('disabled'), true);

  handlers.onEnd({ reason: 'completed' });
  const status = host.querySelector('#tq-studio-run-status');
  assert.equal(status.hidden, false);
  assert.match(status.textContent, /7b04e2aa/);
  assert.match(status.textContent, /Otwórz run/);
  view.dispose();
  cleanup();
});

test('a T1 run that ended gives the Run button back for the next one', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  const select = host.querySelector('#tq-studio-target');
  await until(() => select.querySelector('option[value="core:node-a"]'), 'the targets from the wire');
  view.grid = gridOf(BELL);
  view.circuit = BELL;
  view.state.target = 'core:node-a';
  const button = host.querySelector('#tq-studio-run');

  await view.run();
  const started = () => screen.requests.filter(([kind]) => kind === 'tentaQuantCircuitSimulateRequest').length;
  assert.equal(started(), 1);
  // A run in flight owns the panel, and the button SAYS so rather than
  // swallowing the click.
  assert.equal(button.hasAttribute('disabled'), true);

  screen.subscriptions[0].handlers.onEnd({ reason: 'completed' });
  assert.equal(button.hasAttribute('disabled'), false, 'the stream ended, so the button is free');
  await view.run();
  assert.equal(started(), 2, 'the second run is started, not dropped by a stale stream handle');
  // The Run button means the same thing on both tiers: "sample again". A T1 run
  // that sent no seed would take `SimulateOptions::default().seed` (0) and hand
  // back the previous run's histogram bit for bit, while the same button on T0
  // mints a seed per run.
  const seeds = screen.requests
    .filter(([kind]) => kind === 'tentaQuantCircuitSimulateRequest')
    .map(([, payload]) => payload.options.seed);
  assert.equal(seeds.filter((seed) => Number.isInteger(seed) && seed > 0).length, 2, 'every run carries its own seed');
  assert.notEqual(seeds[0], seeds[1]);
  view.dispose();
  cleanup();
});

test('a refused run takes the previous run\'s line with it', async () => {
  const screen = fakeScreen();
  const inner = screen.tq.bind(screen);
  let refuse = false;
  screen.tq = (kind, payload) => {
    if (kind === 'tentaQuantCircuitSimulateRequest' && refuse) {
      screen.requests.push([kind, payload]);
      return Promise.reject(new Error('brak uprawnień'));
    }
    return inner(kind, payload);
  };
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  const select = host.querySelector('#tq-studio-target');
  await until(() => select.querySelector('option[value="core:node-a"]'), 'the targets from the wire');
  view.grid = gridOf(BELL);
  view.circuit = BELL;
  view.state.target = 'core:node-a';

  await view.run();
  screen.subscriptions[0].handlers.onEnd({ reason: 'completed' });
  const status = host.querySelector('#tq-studio-run-status');
  assert.equal(status.hidden, false);

  // The node refused THIS circuit a run. Keeping the finished run's id and its
  // status chip above it would attribute that run to a circuit that never got
  // one.
  refuse = true;
  await view.run();
  assert.equal(status.hidden, true);
  assert.equal(view.runState, null);
  assert.equal(view.runInfo, null);
  assert.equal(host.querySelector('#tq-studio-run').hasAttribute('disabled'), false, 'and the button is free again');
  view.dispose();
  cleanup();
});

test('a click while the run is being placed does not start a second one', async () => {
  const screen = fakeScreen();
  const inner = screen.tq.bind(screen);
  let release = null;
  // The round trip that places the run is part of the run: while it is open the
  // button is down, and a click that slips through anyway is refused.
  screen.tq = (kind, payload) => {
    if (kind !== 'tentaQuantCircuitSimulateRequest') return inner(kind, payload);
    screen.requests.push([kind, payload]);
    return new Promise((resolve) => {
      release = () => resolve({ run: { ...T1_RUN, status: 'running', endedAt: null, metrics: null } });
    });
  };
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  view.grid = gridOf(BELL);
  view.circuit = BELL;
  view.state.target = 'core:node-a';

  const first = view.run();
  await until(() => release, 'the request the run is waiting on');
  assert.equal(host.querySelector('#tq-studio-run').hasAttribute('disabled'), true);
  await view.run();
  assert.equal(screen.requests.filter(([kind]) => kind === 'tentaQuantCircuitSimulateRequest').length, 1);
  release();
  await first;
  assert.equal(screen.subscriptions.length, 1, 'one run, one stream');
  view.dispose();
  cleanup();
});

test('editing the circuit gives the browser tier its state back', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  view.grid = gridOf(BELL);
  await view.runOnCore();
  screen.subscriptions[0].handlers.onChunk({ event: KEYFRAME_FRAME });
  assert.equal(view.keyframeMode(), true);
  // Those frames describe the program that WAS on the grid.
  await view.rebuildSimulator();
  assert.equal(view.keyframeMode(), false);
  assert.equal(host.querySelector('#tq-studio-run-status').hidden, true);
  assert.equal(host.querySelector('[data-act="play"]').hasAttribute('disabled'), false);
  view.dispose();
  cleanup();
});

test('the step slider and the transport belong to the step mode only', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  assert.equal(host.querySelector('#tq-studio-steps').hidden, true);
  assert.equal(host.querySelector('#tq-studio-text').hidden, true);
  screen.studio.mode = 'step';
  view.applyMode();
  assert.equal(host.querySelector('#tq-studio-steps').hidden, false);
  assert.equal(host.querySelector('#tq-studio-transport').hidden, false);
  // The slider is the tf-* primitive and carries its own label.
  assert.equal(host.querySelector('#tq-studio-step').getAttribute('aria-label'), 'Krok');
  cleanup();
});

test('a viewer gets a read-only grid and no save actions', () => {
  const screen = fakeScreen({ project: project({ myRole: 'viewer' }) });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  drawStudio(screen, host);
  assert.ok(host.querySelector('tf-quantum-circuit').hasAttribute('readonly'));
  assert.ok(host.querySelector('[data-act="save-qasm"]').hasAttribute('disabled'));
  assert.ok(host.querySelector('[data-act="save-cell"]').hasAttribute('disabled'));
  // Exporting what you can already see is not a write and stays available.
  assert.equal(host.querySelector('[data-act="export-qasm"]').hasAttribute('disabled'), false);
  cleanup();
});

test('the Studio says so when this build has no browser simulator', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  await view.init();
  const alert = host.querySelector('#tq-studio-status tf-alert');
  assert.ok(alert, 'the missing wasm module is reported, not hidden');
  assert.equal(alert.getAttribute('tone'), 'warning');
  cleanup();
});

test('a status banner outlives a mode switch — the state panel does not own it', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  await view.init();
  // Node has no browser simulator, so init raises the banner; painting the
  // state on every mode switch must not wipe a condition that is still true.
  screen.studio.mode = 'step';
  view.applyMode();
  screen.studio.mode = 'edit';
  view.applyMode();
  assert.ok(host.querySelector('#tq-studio-status tf-alert'), 'the missing simulator is still reported');

  // The other persistent condition: a rejected program, whose detailed list is
  // in the OpenQASM tab and therefore invisible in the other two modes.
  view.engineMissing = false;
  view.parseErrors = [{ line: 3, column: 1, message: 'oczekiwano ;' }];
  view.paintErrors();
  screen.studio.mode = 'step';
  view.applyMode();
  const alert = host.querySelector('#tq-studio-status tf-alert');
  assert.ok(alert, 'a parse error must never fail silently');
  assert.equal(alert.getAttribute('message'), 'oczekiwano ;');

  // And it clears the moment the program parses again.
  view.parseErrors = [];
  view.paintErrors();
  assert.equal(host.querySelector('#tq-studio-status tf-alert'), null);
  cleanup();
});

test('the run button counts its shots through the plural forms', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  // 1024 ends in 24, so Polish takes the "few" form — the reason plurals go
  // through i18n and never through concatenation (CLAUDE.md rule 8).
  assert.equal(host.querySelector('#tq-studio-run').getAttribute('label'), 'Uruchom 1024 shoty');
  screen.studio.shots = 1;
  view.paintRunButton();
  assert.equal(host.querySelector('#tq-studio-run').getAttribute('label'), 'Uruchom 1 shot');
  screen.studio.shots = 5;
  view.paintRunButton();
  assert.equal(host.querySelector('#tq-studio-run').getAttribute('label'), 'Uruchom 5 shotów');
  cleanup();
});

// ---------------------------------------------------------------------------
// Q06 — the notebook
// ---------------------------------------------------------------------------

test('the notebook draws its cells beside the state panel, with add bars between them', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.equal(host.querySelectorAll('.nb-layout .cells .cell').length, 2);
  // One bar above every cell and one at the end.
  assert.equal(host.querySelectorAll('.add-cell').length, 3);
  assert.ok(host.querySelector('.nb-layout .state-panel'));
  // Exactly the two cell kinds that have a backend today.
  const adds = [...host.querySelectorAll('.add-cell [data-add]')].map((b) => b.dataset.add);
  assert.deepEqual([...new Set(adds)], ['markdown', 'circuit']);
  assert.doesNotMatch(host.textContent, /Python/);
  cleanup();
});

test('the notebook toolbar offers the tiers and reports the auto rule', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  const select = host.querySelector('#tq-nb-target');
  await until(() => select.querySelector('option[value="core:node-a"]'), 'the targets from the wire');
  assert.deepEqual(
    [...select.querySelectorAll('option')].map((o) => o.value),
    ['auto', 'browser', 'core:node-a', 'core:node-b'],
  );
  assert.equal(select.querySelector('option[value="core:node-b"]').disabled, true);
  await until(() => /T0/.test(host.querySelector('#tq-nb-target-hint').textContent), 'the auto resolution');
  view.dispose();
  cleanup();
});

test('a circuit cell run on T1 shows its run and moves the panel onto the recorded frames', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  view.target = 'core:node-a';
  const id = view.cells.find((c) => c.kind === 'circuit').id;
  const running = view.run(id);
  await until(() => screen.subscriptions.length, 'the run stream');
  const { handlers } = screen.subscriptions[0];
  handlers.onChunk({ event: COUNTS_FRAME });
  handlers.onChunk({ event: KEYFRAME_FRAME });

  const out = view.cellEl(id).querySelector('[data-out]');
  assert.match(out.textContent, /T1 · Core/);
  assert.match(out.textContent, /7b04e2aa/);
  assert.equal(out.querySelector('[data-run-counts]').bundle[COUNTS_MIME].shots, 1024);
  assert.match(out.textContent, /Otwórz run/);

  // The panel follows the last circuit cell, so it is now showing the frames a
  // node recorded — with the slider that steps between them.
  assert.equal(host.querySelector('#tq-nb-steps').hidden, false);
  assert.match(host.querySelector('#tq-nb-step-value').textContent, /klatka 1 \/ 1 · H q0/);
  assert.equal(host.querySelectorAll('#tq-nb-bloch tf-bloch-sphere').length, 2);
  assert.equal(host.querySelector('#tq-nb-amps').bundle[STATE_MIME].top.length, 1);
  assert.deepEqual(host.querySelector('#tq-nb-kf-hist').bundle[COUNTS_MIME].counts, { '00': 0.5 });
  assert.match(host.querySelector('.state-panel .tier').textContent, /T1 · Core/);

  // A cell run twice is two samples, here as in the Studio.
  const asked = screen.requests.find(([kind]) => kind === 'tentaQuantCircuitSimulateRequest');
  assert.ok(Number.isInteger(asked[1].options.seed) && asked[1].options.seed > 0, 'the run carries its own seed');

  handlers.onEnd({ reason: 'completed' });
  await running;
  view.dispose();
  cleanup();
});

test('leaving the notebook mid-run releases the cell that was waiting for it', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  view.target = 'core:node-a';
  const id = view.cells.find((c) => c.kind === 'circuit').id;
  const running = view.run(id);
  await until(() => screen.subscriptions.length, 'the run stream');
  // Leaving the notebook stops the session without it ever reporting an end, so
  // nothing but the view itself can release the waiter — and a waiter left
  // pending stalls "Uruchom wszystko" halfway and holds the closed view, its
  // DOM and the screen behind it.
  view.dispose();
  const settled = await Promise.race([
    running.then(() => 'released'),
    new Promise((resolve) => setTimeout(() => resolve('stuck'), 200)),
  ]);
  assert.equal(settled, 'released');
  cleanup();
});

test('a markdown cell previews and a circuit cell carries the grid and the text tab', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  const [markdown, circuit] = host.querySelectorAll('.cell');
  assert.ok(markdown.querySelector('[data-markdown]'), 'markdown renders a preview');
  assert.equal(markdown.querySelector('tf-code-editor'), null, 'the editor opens on demand');
  assert.ok(circuit.querySelector('tf-quantum-circuit[data-circuit]'));
  assert.ok(circuit.querySelector('[data-view="text"] tf-code-editor'));
  assert.ok(circuit.querySelector('[data-act="run"]'), 'a circuit cell runs in the browser');
  assert.ok(circuit.querySelector('[data-act="studio"]'), 'and opens in the Studio');
  cleanup();
});

test('editing a markdown cell swaps the preview for the editor', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  host.querySelector('.cell [data-act="toggle-edit"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  const cell = host.querySelector('.cell');
  assert.ok(cell.querySelector('tf-code-editor'));
  assert.equal(cell.querySelector('[data-markdown]'), null);
  cleanup();
});

test('adding a cell marks the notebook dirty and enables the save button', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.ok(host.querySelector('[data-act="save"]').hasAttribute('disabled'), 'a freshly loaded notebook is clean');
  host.querySelector('.add-cell [data-add="circuit"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(view.cells.length, 3);
  assert.equal(host.querySelector('[data-act="save"]').hasAttribute('disabled'), false);
  cleanup();
});

test('a viewer reads the notebook and cannot change it', async () => {
  const screen = fakeScreen({ projectTab: 'notebook', project: project({ myRole: 'viewer' }) });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.equal(host.querySelectorAll('.add-cell').length, 0, 'no cell can be added');
  assert.ok(host.querySelector('[data-act="save"]').hasAttribute('disabled'));
  assert.ok(host.querySelector('.cell [data-act="delete"]').hasAttribute('disabled'));
  // A viewer still runs a circuit in their own browser (SPEC decision 8).
  assert.equal(host.querySelector('.cell [data-act="run"]')?.hasAttribute('disabled'), false);
  cleanup();
});

test('a project with no notebook offers to create one instead of an empty column', async () => {
  const screen = fakeScreen({ projectTab: 'notebook', notebooks: [], notebookId: null });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.ok(host.querySelector('tf-empty-state'));
  assert.ok(host.querySelector('[data-act="create"]'));
  assert.equal(host.querySelector('.cells'), null);
  cleanup();
});

test('a cell kind this build cannot draw is shown as such and survives a save', async () => {
  const screen = fakeScreen({
    projectTab: 'notebook',
    tq() {
      return Promise.resolve({
        notebook: { notebookId: 'nb1', name: 'Grover', currentVersion: 4, updatedAt: '2026-09-03 14:02:00' },
        version: 4,
        cellsJson: '[{"id":"k1","kind":"code","source":"print(1)"}]',
      });
    },
  });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.match(host.querySelector('.tq-cell-unknown').textContent, /„code”/);
  assert.equal(view.cells[0].source, 'print(1)');
  assert.equal(serializeCells(view.cells), '[{"id":"k1","kind":"code","source":"print(1)"}]');
  cleanup();
});

// ---------------------------------------------------------------------------
// Q07 — the panel follows the circuit
// ---------------------------------------------------------------------------

test('a new circuit takes the previous circuit’s histogram off the screen', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  // What a finished run leaves behind: the bundle the screen itself produces,
  // for the circuit that was run.
  const counts = host.querySelector('#tq-studio-counts');
  view.counts = { '00': 512, '11': 512 };
  counts.bundle = countsBundle(view.counts, 1024);
  assert.ok(counts.bundle[COUNTS_MIME], 'the fixture is the bundle the run writes');
  await view.rebuildSimulator();
  assert.equal(view.counts, null, 'the model drops the shots');
  assert.equal(counts.bundle, null, 'and so does the card — nothing claims to be this circuit’s result');
  cleanup();
});

test('a batch that was already in flight cannot repaint the histogram it lost', async () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  const counts = host.querySelector('#tq-studio-counts');
  // A long run is drawing in batches when the user edits the grid: the batch
  // that lands afterwards carries the OLD circuit's draws.
  const generation = view.runGeneration;
  view.counts = {};
  assert.equal(view.applyBatch(generation, { '00': 256 }), true);
  assert.ok(counts.bundle[COUNTS_MIME], 'a batch of the current run does draw');

  await view.rebuildSimulator();
  assert.equal(view.applyBatch(generation, { '00': 256 }), false, 'the loop is told to stop');
  assert.equal(view.counts, null, 'and nothing of the old circuit is merged back');
  assert.equal(counts.bundle, null);
  cleanup();
});

test('the Studio’s own run plan pays for a bounded number of evolutions', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  // The shots input offers 100 000; one batch is one full evolution of the
  // register, so the run may not turn that maximum into 391 of them.
  const plan = view.runPlan(100000, 0);
  assert.ok(plan.length <= MAX_RUN_BATCHES, `${plan.length} batches is ${plan.length} evolutions`);
  assert.equal(plan.reduce((sum, b) => sum + b.shots, 0), 100000);
  assert.equal(view.runPlan(1024, 0).length, 4, 'a default run still fills in 256-shot steps');
  cleanup();
});

test('a circuit that never parsed says so in the resources card', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  view.circuit = null;
  view.paintResources();
  const card = host.querySelector('#tq-studio-resources');
  assert.match(card.textContent, /nie parsuje/, 'an empty box would explain nothing');
  cleanup();
});

test('the gate card names the selected operation and offers its two edits', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  assert.match(host.querySelector('#tq-studio-gate').textContent, /Kliknij bramkę/);

  view.grid = gridOf(BELL);
  view.selection = [1];
  view.paintGate();
  assert.match(host.querySelector('#tq-studio-gate-chip').innerHTML, /X q0, q1/);
  assert.match(host.querySelector('#tq-studio-gate').textContent, /Kolumna/);
  assert.ok(host.querySelector('[data-act="gate-duplicate"]'));
  assert.ok(host.querySelector('[data-act="gate-delete"]'));

  // Several gates at once: one delete, no duplicate — the copy of a selection
  // is not an operation the grid defines.
  view.selection = [0, 1];
  view.paintGate();
  assert.equal(host.querySelector('[data-act="gate-duplicate"]'), null);
  assert.match(host.querySelector('[data-act="gate-delete"]').textContent, /Usuń 2 bramki/);
  cleanup();
});

test('the gate card sends its edits through the component, so undo still works', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  const element = host.querySelector('#tq-studio-circuit');
  element.circuit = BELL;
  view.grid = gridOf(BELL);
  view.selection = [0];
  view.paintGate();
  host.querySelector('[data-act="gate-delete"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(element.circuit.ops.length, 3);
  element.undo();
  assert.equal(element.circuit.ops.length, 4, 'the edit is on the undo stack, not a circuit reassignment');
  cleanup();
});

// ---------------------------------------------------------------------------
// The width the state panel can actually draw
// ---------------------------------------------------------------------------

/// A circuit `n` qubits wide with one gate on the first wire.
const wide = (n) => ({
  qubitRegisters: [{ name: 'q', start: 0, size: n }],
  clbitRegisters: [],
  numQubits: n,
  numClbits: 0,
  ops: [gate('H', [0])],
});

/// A simulator handle that counts what the panel pulls out of the wasm heap.
function fakeSim(numQubits) {
  return {
    copies: 0,
    precision: 'single',
    backendName: 'statevector',
    amplitudes() {
      this.copies += 1;
      return new Float64Array(2 ** (numQubits + 1));
    },
    blochVectors() { return new Float64Array(numQubits * 3); },
  };
}

test('a state too wide to list says so instead of copying it out of wasm', () => {
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  const amps = host.querySelector('#tq-studio-amps');

  // Narrow enough: the table gets the real vector.
  view.grid = gridOf(wide(2));
  view.sim = fakeSim(2);
  view.paintState();
  assert.ok(amps.bundle[STATE_MIME], 'the amplitudes are the panel’s own bundle');
  assert.equal(view.sim.copies, 1);

  // One qubit over the ceiling: at 24 — the width the target select promises —
  // a single repaint would copy 268 MB and build ~16.7M row objects, 60 times a
  // second while the evolution plays.
  const numQubits = MAX_LIVE_STATE_QUBITS + 1;
  view.grid = gridOf(wide(numQubits));
  view.sim = fakeSim(numQubits);
  view.paintState();
  assert.equal(view.sim.copies, 0, 'the vector never leaves the wasm heap');
  assert.equal(amps.bundle[STATE_MIME], undefined);
  assert.match(amps.bundle['text/plain'], /amplitud/, 'the card says why it is empty');
  assert.match(amps.bundle['text/plain'], new RegExp(String(MAX_LIVE_STATE_QUBITS)));
  // The spheres still come from the simulator's own pass.
  assert.equal(host.querySelectorAll('#tq-studio-bloch tf-bloch-sphere').length, numQubits);
  cleanup();
});

test('the notebook panel does not ask for a state vector it cannot draw', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  const cell = view.cells.find((c) => c.kind === 'circuit');
  view.parsed.set(cell.id, wide(MAX_LIVE_STATE_QUBITS + 2));
  await view.refreshPanel();
  const hint = host.querySelector('#tq-nb-panel-hint');
  assert.match(hint.textContent, /panel stanu/, 'the panel explains its ceiling');
  assert.equal(host.querySelectorAll('#tq-nb-bloch tf-bloch-sphere').length, 0);
  cleanup();
});

// ---------------------------------------------------------------------------
// Markdown cells
// ---------------------------------------------------------------------------

test('a markdown cell renders headings and lists, not their syntax', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  const id = view.cells[0].id;
  view.cells = updateCell(view.cells, id, { source: '# Grover 4q\n\n## Plan\n1. superpozycja\n2. wyrocznia\n' });
  view.render();
  const md = view.cellEl(id).querySelector('[data-markdown]');
  await until(() => md.querySelector('h1'), 'the rendered markdown');
  assert.equal(md.querySelector('h1').textContent, 'Grover 4q');
  assert.equal(md.querySelector('h2').textContent, 'Plan');
  assert.equal(md.querySelectorAll('ol li').length, 2);
  assert.doesNotMatch(md.textContent, /#/, 'no markup is left as text');
  cleanup();
});

test('the saved-at label comes back when an edit is undone', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  const label = () => host.querySelector('.tq-save-state').textContent;
  const clean = label();
  assert.match(clean, /zapisano/);

  const id = view.cells[0].id;
  const original = view.cells[0].source;
  view.cells = updateCell(view.cells, id, { source: `${original}x` });
  view.markDirty();
  assert.equal(label(), 'niezapisane zmiany');

  // Typing the character away again leaves the notebook clean — and the label
  // has to say so, not keep the warning until the next full redraw.
  view.cells = updateCell(view.cells, id, { source: original });
  view.markDirty();
  assert.equal(label(), clean);
  assert.ok(host.querySelector('[data-act="save"]').hasAttribute('disabled'));
  cleanup();
});

// ---------------------------------------------------------------------------
// Unsaved cells — this view object is the only copy until a save lands
// ---------------------------------------------------------------------------

/// Answers the leave dialog. The button has to exist — that is asserted — but
/// the answer is delivered as tf-window's `action` event rather than a click on
/// it: happy-dom does not retarget a light-DOM click into the shadow ancestors
/// of the slot the button is assigned to, so the component's own footer
/// listener never fires under Node. The event IS the contract the screen reads.
async function answerLeave(action) {
  const win = await until(
    () => [...window.document.querySelectorAll('tf-window')].find((w) => w.querySelector('[data-action="discard"]')),
    'the leave dialog',
  );
  assert.ok(win.querySelector(`[data-action="${action}"]`), `the dialog offers "${action}"`);
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action }, bubbles: true }));
  return win;
}

/// A notebook with one edited markdown cell — the state a tab click may not
/// throw away.
async function dirtyNotebook(over = {}) {
  const screen = fakeScreen({ projectTab: 'notebook', ...over });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  view.cells = updateCell(view.cells, view.cells[0].id, { source: '# Grover, wersja robocza\n' });
  view.markDirty();
  return { screen, host, view };
}

const saves = (screen) => screen.requests.filter(([kind]) => kind === 'tentaQuantNotebookSaveRequest');

test('leaving a dirty notebook asks first, and cancelling keeps every edit', async () => {
  const { screen, view } = await dirtyNotebook();
  // The screen asks through this handle before it disposes the view (a project
  // tab, the breadcrumb, closing the project — all end in the same dispose).
  assert.equal(typeof screen.projectViewGuard, 'function');

  const leaving = screen.projectViewGuard();
  await answerLeave('cancel');
  assert.equal(await leaving, false, 'the screen stays on the notebook');
  assert.match(view.cells[0].source, /wersja robocza/, 'and the edit is still there');
  assert.deepEqual(saves(screen), [], 'nothing was written behind the user’s back');
  cleanup();
});

test('“save and leave” writes the whole column before the screen moves on', async () => {
  const { screen, view } = await dirtyNotebook();
  const leaving = screen.projectViewGuard();
  await answerLeave('save');
  assert.equal(await leaving, true);
  const [[, payload]] = saves(screen);
  assert.equal(payload.expectedVersion, 3, 'the save still carries the optimistic lock');
  assert.match(payload.cellsJson, /wersja robocza/);
  assert.equal(JSON.parse(payload.cellsJson).length, view.cells.length, 'every cell, not just the edited one');
  cleanup();
});

test('“discard” reverts the model, so nothing downstream carries the thrown-away edits', async () => {
  const { screen, view } = await dirtyNotebook();
  const leaving = screen.projectViewGuard();
  await answerLeave('discard');
  assert.equal(await leaving, true);
  assert.equal(view.cells[0].source, CELLS[0].source, 'the column is what the server holds');
  assert.deepEqual(saves(screen), []);
  cleanup();
});

test('a clean notebook leaves without a question', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.equal(await view.confirmLeave(), true);
  assert.equal(window.document.querySelector('tf-window'), null, 'no dialog for nothing to lose');
  cleanup();
});

test('the trip to the Studio never hands on a half-saved notebook', async () => {
  const { screen, host, view } = await dirtyNotebook();
  // The Studio writes a circuit back by RE-READING the notebook from the
  // server, so an unsaved column would come back missing every other edit.
  const circuit = [...host.querySelectorAll('.cell')].find((el) => el.querySelector('[data-act="studio"]'));
  circuit.querySelector('[data-act="studio"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  await answerLeave('save');
  await until(() => screen.requests.find(([kind]) => kind === 'studio'), 'the hand-over to the Studio');
  const [[, payload]] = saves(screen);
  assert.match(payload.cellsJson, /wersja robocza/, 'the markdown edit went to the server first');
  const [, handed] = screen.requests.find(([kind]) => kind === 'studio');
  assert.equal(handed.cellId, view.cells[1].id);
  assert.equal(handed.source, view.cells[1].source);
  cleanup();
});

/// Presses Escape on the top-most window, the way `tf-window` listens for it.
function pressEscape() {
  window.document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
}

test('Escape on the leave dialog answers “stay” instead of leaving the screen hanging', async () => {
  const { screen, view } = await dirtyNotebook();
  const leaving = screen.projectViewGuard();
  await until(
    () => [...window.document.querySelectorAll('tf-window')].find((w) => w.querySelector('[data-action="discard"]')),
    'the leave dialog',
  );
  // Escape closes a tf-window without ever emitting an action, so a promise
  // that only the footer can settle would never resolve and every caller of
  // this guard — a project tab, the breadcrumb — would hang on it.
  pressEscape();
  assert.equal(await leaving, false, 'the screen stays on the notebook');
  assert.match(view.cells[0].source, /wersja robocza/, 'and the edit survives');
  assert.deepEqual(saves(screen), []);
  cleanup();
});

test('Escape on the notebook picker answers null instead of leaving it hanging', async () => {
  const screen = fakeScreen();
  const picked = openNotebookPicker(screen);
  await until(
    () => [...window.document.querySelectorAll('tf-window')].find((w) => w.querySelector('#tq-pick-notebook')),
    'the notebook picker',
  );
  pressEscape();
  assert.equal(await picked, null, 'nothing was picked, and the caller is released');
  cleanup();
});

test('Escape on the new-notebook name dialog releases the caller with nothing created', async () => {
  const screen = fakeScreen();
  const view = drawNotebook(screen, screen.root);
  await view.mount();
  // `create()` awaits the name dialog; a promise only the footer could settle
  // would strand it here forever and leave the view holding a dead modal.
  const creating = view.create();
  await until(
    () => [...window.document.querySelectorAll('tf-window')].find((w) => w.querySelector('#tq-name-input')),
    'the name dialog',
  );
  pressEscape();
  await creating;
  assert.deepEqual(
    screen.requests.filter(([kind]) => kind === 'tentaQuantNotebookCreateRequest'),
    [],
    'no notebook was created',
  );
  cleanup();
});

test('the picker answers with the notebook that was chosen, read after it closed', async () => {
  const screen = fakeScreen({
    notebooks: [
      { notebookId: 'nb1', name: 'Grover' },
      { notebookId: 'nb2', name: 'Splątanie' },
    ],
  });
  const picked = openNotebookPicker(screen);
  const win = await until(
    () => [...window.document.querySelectorAll('tf-window')].find((w) => w.querySelector('#tq-pick-notebook')),
    'the notebook picker',
  );
  win.querySelector('#tq-pick-notebook').value = 'nb2';
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, bubbles: true }));
  assert.deepEqual(await picked, { notebookId: 'nb2' });
  cleanup();
});

test('the project tab carries the unsaved dot — the toolbar label leaves with the panel', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  // The shell the notebook is drawn into: the tab bar outlives the panel.
  screen.root.innerHTML = `
    <tf-tabs id="tq-project-tabs"><tf-tab id="notebook">Notatnik</tf-tab></tf-tabs>
    <div id="tq-project-panel"></div>`;
  const view = drawNotebook(screen, screen.root.querySelector('#tq-project-panel'));
  await view.mount();
  const tab = screen.root.querySelector('#tq-project-tabs tf-tab#notebook');
  assert.equal(tab.hasAttribute('dirty'), false);

  view.cells = updateCell(view.cells, view.cells[0].id, { source: 'x' });
  view.markDirty();
  assert.equal(tab.hasAttribute('dirty'), true, 'the warning survives the click that needs it');

  view.dispose();
  assert.equal(tab.hasAttribute('dirty'), false, 'and goes away with the view');
  cleanup();
});

test('the screen refuses a tab switch the open view did not release', async () => {
  const screen = (await import('../tentaquant.js')).default;
  const view = Object.create(screen);
  view.root = window.document.createElement('div');
  window.document.body.appendChild(view.root);
  view.root.innerHTML = '<tf-tabs id="tq-project-tabs" value="notebook"><tf-tab id="notebook">N</tf-tab><tf-tab id="studio">S</tf-tab></tf-tabs>';
  view.projectTab = 'notebook';
  view.projectViewGuard = () => Promise.resolve(false);

  await view.selectProjectTab('studio');
  assert.equal(view.projectTab, 'notebook', 'the screen stays where the work is');
  // tf-tabs moved its own highlight on the click; a refused switch puts it back.
  assert.equal(view.root.querySelector('#tq-project-tabs').getAttribute('value'), 'notebook');
  cleanup();
});

test('the router asks the open project view before it drops this screen', async () => {
  const screen = (await import('../tentaquant.js')).default;
  const view = Object.create(screen);
  // The router's own guard hook: without it a sidebar click unmounts this
  // screen, and the notebook's cells live nowhere but in the view it drops.
  view.projectViewGuard = () => Promise.resolve(false);
  assert.equal(await view.canUnmount(), false);
  view.projectViewGuard = () => Promise.resolve(true);
  assert.equal(await view.canUnmount(), true);
  view.projectViewGuard = null;
  assert.equal(await view.canUnmount(), true, 'no open view, nothing to ask');
  cleanup();
});

test('a new markdown cell opens in its editor instead of an empty preview', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  host.querySelector('.add-cell [data-add="markdown"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  const id = view.cells[0].id;
  assert.ok(view.editing.has(id), 'the cell the click created is the one being edited');
  assert.ok(view.cellEl(id).querySelector('tf-code-editor'), 'and it is showing its editor');
  cleanup();
});

// ---------------------------------------------------------------------------
// Plan §13.5 — a phone reads, it does not edit
// ---------------------------------------------------------------------------

test('below the editing breakpoint the Studio is a preview, whatever the role says', () => {
  window.happyDOM.setViewport({ width: 390 });
  const screen = fakeScreen();
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawStudio(screen, host);
  assert.equal(view.writable, true, 'the owner may still write the project');
  assert.equal(view.editable, false, 'but not through a 390 px drag-and-drop grid');
  assert.ok(host.querySelector('tf-quantum-circuit').hasAttribute('readonly'));
  assert.ok(host.querySelector('[data-act="save-qasm"]').hasAttribute('disabled'));
  assert.match(host.querySelector('#tq-studio-preview').innerHTML, /tylko podgląd/);

  // Turning the phone sideways gives the editor back without a remount.
  view.setEditable(true);
  assert.equal(host.querySelector('tf-quantum-circuit').hasAttribute('readonly'), false);
  assert.equal(host.querySelector('[data-act="save-qasm"]').hasAttribute('disabled'), false);
  assert.equal(host.querySelector('#tq-studio-preview').innerHTML, '');
  window.happyDOM.setViewport({ width: 1024 });
  cleanup();
});

test('below the editing breakpoint the notebook drops its editing bars', async () => {
  window.happyDOM.setViewport({ width: 390 });
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawNotebook(screen, host);
  await view.mount();
  assert.equal(host.querySelectorAll('.add-cell').length, 0, 'no cell is added with a thumb');
  assert.ok(host.querySelector('.cell [data-act="delete"]').hasAttribute('disabled'));
  assert.ok(host.querySelector('[data-act="save"]').hasAttribute('disabled'));
  // Reading and running the browser tier stay available — that is the point.
  assert.equal(host.querySelector('.cell [data-act="run"]').hasAttribute('disabled'), false);

  view.setEditable(true);
  assert.equal(host.querySelectorAll('.add-cell').length, 3);
  window.happyDOM.setViewport({ width: 1024 });
  cleanup();
});

test('the code editors of both screens speak the dashboard’s language', async () => {
  const screen = fakeScreen({ projectTab: 'notebook' });
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const studioHost = window.document.createElement('div');
  screen.root.appendChild(studioHost);
  drawStudio(screen, studioHost);
  assert.equal(studioHost.querySelector('#tq-studio-source').labels.find, 'Znajdź');

  const view = drawNotebook(screen, host);
  await view.mount();
  host.querySelector('.cell [data-act="toggle-edit"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(host.querySelector('.cell tf-code-editor').labels.replace_all, 'Wszystkie');
  cleanup();
});
