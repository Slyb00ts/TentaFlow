// =============================================================================
// File: modules/tentaquant/run-view.test.js
// Description: The full-screen run result (Q15): the five tabs, where the frames
// come from and how honestly that is labelled, the replay of the measured shots
// (a replay, never a fresh browser sample), the exact-vs-measured numbers of the
// histogram, the comparison request, and the two files of the scientific package
// that this screen previews — which must be byte-identical to what the archive
// writes.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  EXPORT_PARTS, MAX_DENSITY_QUBITS, RESULT_TABS, citationBib, compareTone, drawRunView,
  methodNote, partialCounts, probabilityMap, shotProgress, stepsFromFrames, storedCounts,
} = await import('./run-view.js');
const { COUNTS_MIME, STATE_MIME } = await import('./quantum-view.js');
const { PROBS_MIME } = await import('../../components/tf-mime-output.js');

const R2 = Math.SQRT1_2;
const NODES = [{ nodeId: 'node-a', nodeName: 'spark-01', online: true, instanceStatus: 'ready' }];
const RUN_ID = 'aaaa1111-0000-4000-8000-000000000001';

const H_MATRIX = [[R2, 0], [R2, 0], [R2, 0], [-R2, 0]];

const FRAMES = [
  {
    step: 1,
    gate: { name: 'h', qubits: [0], matrix: H_MATRIX },
    bloch: [[1, 0, 0], [0, 0, 1]],
    purity: [1, 1],
    pairs: [],
    top: [{ index: 0, amplitude: [R2, 0], partners: [{ index: 1, amplitude: [R2, 0] }] }],
    probsTop: [{ bitstring: '00', probability: 0.5 }, { bitstring: '01', probability: 0.5 }],
  },
  {
    step: 2,
    gate: { name: 'measure', qubits: [0], matrix: [] },
    bloch: [[0, 0, 1], [0, 0, 1]],
    purity: [1, 1],
    pairs: [],
    top: [{ index: 0, amplitude: [1, 0], partners: [] }],
    probsTop: [{ bitstring: '00', probability: 1 }],
  },
];

const countsArtifact = {
  cellId: 'c1', seq: 0, mime: COUNTS_MIME, sizeBytes: 40, sha256: null,
  inlineJson: JSON.stringify({ counts: { '00': 512, '11': 512 }, shots: 1024, numQubits: 2 }),
};
const probsArtifact = {
  cellId: 'c1', seq: 1, mime: PROBS_MIME, sizeBytes: 40, sha256: null,
  inlineJson: JSON.stringify({ probabilities: [0.5, 0, 0, 0.5], numQubits: 2 }),
};
const stateArtifact = {
  cellId: 'c1', seq: 2, mime: STATE_MIME, sizeBytes: 80, sha256: null,
  inlineJson: JSON.stringify({ amplitudes: [R2, 0, 0, 0, 0, 0, R2, 0], numQubits: 2 }),
};

const run = (over = {}) => ({
  runId: RUN_ID,
  projectId: 'p1', notebookId: 'nb1', cellId: 'c1', kind: 'cell',
  target: 'core:node-a', nodeId: 'node-a', status: 'succeeded',
  startedAt: '2026-09-03 14:02:00', endedAt: '2026-09-03 14:02:02', error: null,
  metrics: {
    durationMs: 1400, qubits: 2, clbits: 2, shots: 1024, seed: 7, memoryBytes: 64,
    gates: 2, keyframes: 2, method: 'statevector', precision: 'double', backend: 'cpu',
    coreVersion: '1.2.3',
  },
  userId: 'u1', userName: 'Anna Kowalska', pinnedAt: null,
  artifacts: [countsArtifact, probsArtifact, stateArtifact],
  ...over,
});

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

test('the circuit strip is built from the recorded frames and nothing else', () => {
  assert.deepEqual(stepsFromFrames(FRAMES), [
    { step: 1, name: 'h', qubits: [0], collapsing: false },
    { step: 2, name: 'measure', qubits: [0], collapsing: true },
  ]);
});

test('the shots fill at the measurement step, not before it', () => {
  const measures = [2];
  assert.equal(shotProgress(0, measures, 2), 0);
  assert.equal(shotProgress(1, measures, 2), 0, 'nothing is measured before the measurement');
  assert.equal(shotProgress(1.5, measures, 2), 0.5);
  assert.equal(shotProgress(2, measures, 2), 1);
});

test('a recording with no measurement fills only at the very end', () => {
  assert.equal(shotProgress(1.5, [], 2), 0);
  assert.equal(shotProgress(2, [], 2), 1);
});

test('the replay scales the runs OWN counts — it never samples again', () => {
  assert.deepEqual(partialCounts({ '00': 512, '11': 512 }, 0.5), { '00': 256, '11': 256 });
  assert.deepEqual(partialCounts({ '00': 512, '11': 512 }, 1), { '00': 512, '11': 512 });
  assert.deepEqual(partialCounts({ '00': 512 }, 0), { '00': 0 });
});

test('the counts and the exact distribution are read out of their own outputs', () => {
  assert.deepEqual(storedCounts({ [COUNTS_MIME]: { counts: { '00': 1, '11': 3 }, shots: 4 } }),
    { counts: { '00': 1, '11': 3 }, shots: 4 });
  // `stored_counts` falls back to the sum when the artifact recorded no total.
  assert.deepEqual(storedCounts({ [COUNTS_MIME]: { counts: { '00': 1, '11': 3 } } }),
    { counts: { '00': 1, '11': 3 }, shots: 4 });
  assert.equal(storedCounts({}), null);
  assert.deepEqual(probabilityMap({ [PROBS_MIME]: { probabilities: [0.5, 0, 0, 0.5], numQubits: 2 } }),
    { '00': 0.5, '11': 0.5 });
  assert.equal(probabilityMap({ [PROBS_MIME]: { probabilities: [] } }), null);
});

test('the BibTeX entry is the one the archive writes, braces stripped from the name', () => {
  assert.equal(
    citationBib(run({ userName: 'Anna {Kowalska}' })),
    `@misc{tentaquant-${RUN_ID},\n  title  = {TentaQuant run ${RUN_ID}},\n  author = {Anna Kowalska},\n`
    + `  year   = {2026},\n  note   = {Run ${RUN_ID}, started 2026-09-03 14:02:00, target core:node-a},\n}\n`,
  );
});

test('the method note states only what the run stored', () => {
  const note = methodNote(run(), { projectName: 'VQE H2' });
  assert.match(note, /\| Project \| VQE H2 \|/);
  assert.match(note, /\| Seed \| 7 \|/);
  assert.match(note, /\| Recorded evolution \| yes, 2 keyframes \|/);
  assert.match(note, /\| Stored state vector \| yes \|/);
  assert.match(note, /\| Engine \| TentaFlow Core 1\.2\.3 \|/);
  assert.doesNotMatch(note, /Evolution note/, 'a note the run does not carry gets no row');
  const bare = methodNote(run({ metrics: null, artifacts: [] }));
  assert.doesNotMatch(bare, /## Simulation/);
  assert.doesNotMatch(bare, /Project/);
  assert.doesNotMatch(note, /## Measurement/, 'no counts handed in, no measurement section');
  assert.doesNotMatch(note, /## Outcome/, 'a run that did not fail has no outcome section');
});

// The preview is the file, not a prettier cousin of it: `export.rs::method_md`
// closes with a Measurement table whenever the run stored counts, and with an
// Outcome section whenever the run carries an error.
test('the method note carries the measurement and the failure the archive writes', () => {
  const note = methodNote(run(), { counts: { counts: { '00': 512, '11': 512 }, shots: 1024 } });
  assert.match(note, /\n## Measurement\n\n\| \| \|\n\|---\|---\|\n\| Shots \| 1024 \|\n\| Distinct outcomes \| 2 \|\n/);
  assert.match(note, /\nThe full histogram is in `counts\.json` and `counts\.csv`\.\n$/);
  const failed = methodNote(run({ status: 'failed', error: 'qubit index out of range' }));
  assert.match(failed, /\n## Outcome\n\nThe run ended with: qubit index out of range\n$/);
});

test('the eight comparison series have eight distinct tones', () => {
  const tones = new Set(Array.from({ length: 8 }, (_, i) => compareTone(i)));
  assert.equal(tones.size, 8);
  assert.equal(compareTone(99), compareTone(7), 'the scale is clamped');
});

test('the five views and the six package parts are the ones the plan names', () => {
  assert.deepEqual(RESULT_TABS, ['evolution', 'state', 'histogram', 'compare', 'data']);
  assert.deepEqual(EXPORT_PARTS, [
    'counts_json', 'counts_csv', 'statevector_npz', 'circuit_qasm', 'method_md', 'citation_bib',
  ]);
  assert.equal(MAX_DENSITY_QUBITS, 6);
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
    projectId: 'p1',
    project: { projectId: 'p1', name: 'VQE H2' },
    lab: { displayName: 'Kwanty R&D', nodes: NODES },
    projects: [{ projectId: 'p1', name: 'VQE H2' }],
    runs: [run(), run({ runId: 'bbbb2222', metrics: { qubits: 2, shots: 512 } })],
    row: run(),
    frames: FRAMES,
    requests: [],
    tabs: [],
    closed: 0,
    setRunViewDispose() {},
    setResultTab(tab) { this.tabs.push(tab); },
    closeRunResult() { this.closed += 1; },
    closeProject() {},
    backToLabs() {},
    openNotebookForRun() { this.notebook = true; },
    tqSubscribe() { return Promise.resolve(() => {}); },
    onTransport() { return () => {}; },
    tq(kind, payload) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantRunGetRequest') return Promise.resolve({ run: this.row });
      if (kind === 'tentaQuantRunKeyframesRequest') return Promise.resolve({ keyframes: this.frames });
      if (kind === 'tentaQuantRunStateQueryRequest') {
        return Promise.resolve({
          source: 'state', step: 0, numQubits: 2,
          bloch: [[0, 0, 0], [0, 0, 0]], purity: [0.5, 0.5],
          pairs: [{ qubits: [0, 1], rho: [[0.5, 0], [0, 0], [0, 0], [0.5, 0],
            [0, 0], [0, 0], [0, 0], [0, 0], [0, 0], [0, 0], [0, 0], [0, 0],
            [0.5, 0], [0, 0], [0, 0], [0.5, 0]], mutualInformation: 2, concurrence: 1 }],
          probsTop: [{ bitstring: '00', probability: 0.5 }, { bitstring: '11', probability: 0.5 }],
        });
      }
      if (kind === 'tentaQuantRunCompareRequest') {
        return Promise.resolve({
          bitstrings: ['00', '11'],
          runs: [
            { runId: RUN_ID, label: 'VQE H2', target: 'core:node-a', backend: 'cpu', startedAt: '', durationMs: 1400, shots: 1024, counts: [512, 512], probabilities: [0.5, 0.5], totalVariationDistance: null, hellingerFidelity: null },
            { runId: 'bbbb2222', label: 'VQE H2', target: 'browser', backend: 'wasm', startedAt: '', durationMs: 18, shots: 512, counts: [250, 262], probabilities: [0.488, 0.512], totalVariationDistance: 0.012, hellingerFidelity: 0.999 },
          ],
          diff: [0.012, 0.012],
        });
      }
      if (kind === 'tentaQuantRunExportRequest') {
        return Promise.resolve({ sha256: 'a'.repeat(64), url: '/x?token=t', expiresAtMs: 0, sizeBytes: 4096, entries: ['counts.json', 'method.md'] });
      }
      return Promise.resolve({});
    },
    ...over,
  };
}

const cleanup = () => { window.document.body.innerHTML = ''; };

async function draw(screen, options = {}) {
  const host = window.document.createElement('div');
  screen.root.appendChild(host);
  const view = drawRunView(screen, host, RUN_ID, options);
  await view.ready;
  return { host, view };
}

test('the screen is a breadcrumb, a header and five tabs', async () => {
  const screen = fakeScreen();
  const { host } = await draw(screen);
  const crumbs = [...host.querySelectorAll('tf-breadcrumb-item')];
  assert.equal(crumbs.length, 4);
  assert.match(crumbs[1].textContent, /Kwanty R&D/);
  assert.match(crumbs[2].textContent, /VQE H2/);
  assert.match(crumbs[3].textContent, /aaaa1111/);
  assert.equal(host.querySelectorAll('#tq-result-tabs tf-tab').length, 5);
  assert.ok(host.querySelector('.res-rail'), 'the run metadata rail');
  assert.match(host.querySelector('.res-rail').textContent, /Ziarno losowe/);
  cleanup();
});

test('the evolution view names where its frames came from and drives the components', async () => {
  const { host } = await draw(fakeScreen());
  const section = host.querySelector('#tq-evolution');
  assert.ok([...section.querySelectorAll('tf-chip')]
    .some((c) => /z klatek kluczowych Core/.test(c.getAttribute('label'))));
  assert.equal(section.querySelectorAll('#tq-evo-bloch tf-bloch-sphere').length, 2);
  const strip = section.querySelector('#tq-strip');
  assert.equal(strip.steps.length, 2);
  assert.equal(strip.numQubits, 2);
  // Position 0 is the register before the run: |0> on both spheres.
  const spheres = section.querySelectorAll('tf-bloch-sphere');
  assert.deepEqual(Array.from(spheres[0].vector, Math.round), [0, 0, 1]);
  cleanup();
});

test('seeking the strip moves the whole panel, exactly', async () => {
  const { host, view } = await draw(fakeScreen());
  const strip = host.querySelector('#tq-strip');
  strip.dispatchEvent(new window.CustomEvent('seek', { detail: { position: 1 } }));
  assert.equal(view.position, 1);
  const sphere = host.querySelector('#tq-evo-bloch tf-bloch-sphere');
  // After the H the qubit is on the equator — the frame the node recorded.
  assert.deepEqual(Array.from(sphere.vector, (v) => Math.round(v * 1000) / 1000), [1, 0, 0]);
  cleanup();
});

test('halfway through the H the state is the exact interpolation, not a blend', async () => {
  const { host, view } = await draw(fakeScreen());
  host.querySelector('#tq-strip').dispatchEvent(new window.CustomEvent('seek', { detail: { position: 0.5 } }));
  assert.equal(view.position, 0.5);
  const sphere = host.querySelector('#tq-evo-bloch tf-bloch-sphere');
  const drawn = Array.from(sphere.vector, (v) => Math.round(v * 1e6) / 1e6);
  // Half an H is half a turn about the H axis: (1/2, -sqrt(2)/2, 1/2).
  assert.deepEqual(drawn, [0.5, -0.707107, 0.5]);
  cleanup();
});

test('the live histogram fills at the measurement with the runs own counts', async () => {
  const { host } = await draw(fakeScreen());
  const strip = host.querySelector('#tq-strip');
  const histogram = host.querySelector('#tq-evo-hist');
  strip.dispatchEvent(new window.CustomEvent('seek', { detail: { position: 1 } }));
  assert.deepEqual(histogram.series[0].counts, { '00': 0, '11': 0 }, 'nothing before the measurement');
  strip.dispatchEvent(new window.CustomEvent('seek', { detail: { position: 2 } }));
  assert.deepEqual(histogram.series[0].counts, { '00': 512, '11': 512 });
  assert.match(host.querySelector('#tq-evo-shots').getAttribute('label'), /1024 \/ 1024/);
  cleanup();
});

test('a run that recorded no evolution says so with its own note', async () => {
  const screen = fakeScreen({
    row: run({ metrics: { qubits: 2, shots: 1024, keyframes: 0, evolutionNote: 'over the keyframe budget' } }),
  });
  const { host } = await draw(screen);
  assert.equal(host.querySelector('#tq-evolution'), null);
  assert.match(host.querySelector('tf-alert').getAttribute('message'), /over the keyframe budget/);
  assert.ok(!screen.requests.some(([kind]) => kind === 'tentaQuantRunKeyframesRequest'));
  cleanup();
});

test('the state view asks for the reduced quantities and draws all five pictures', async () => {
  const screen = fakeScreen();
  const { host, view } = await draw(screen);
  view.selectTab('state');
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  const query = screen.requests.find(([kind]) => kind === 'tentaQuantRunStateQueryRequest');
  assert.ok(query, 'the reduced quantities come from the node');
  assert.deepEqual(query[1].pairs, [], 'a small register asks for every pair');
  assert.equal(host.querySelectorAll('#tq-state-bloch tf-bloch-sphere').length, 2);
  assert.ok(host.querySelector('tf-qsphere'));
  assert.ok(host.querySelector('tf-state-bars'));
  assert.ok(host.querySelector('tf-density-plot'));
  assert.ok(host.querySelector('tf-entanglement-graph'));
  // Two qubits and a stored state vector: the FULL rho, not a pair.
  assert.equal(host.querySelector('tf-density-plot').matrix.dim, 4);
  assert.match(host.textContent, /Pełna ρ/);
  assert.equal(host.querySelector('tf-entanglement-graph').pairs.length, 1);
  cleanup();
});

test('the state view explains itself in Polish, deterministically', async () => {
  const { host, view } = await draw(fakeScreen());
  view.selectTab('state');
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  const box = host.querySelector('#tq-explain-state');
  assert.match(box.textContent, /splątane/);
  // The amplitudes the run stored are part of the same state, so the sentence
  // names the heaviest basis states as well as the entanglement.
  assert.match(box.textContent, /\|00⟩/);
  assert.doesNotMatch(box.textContent, /explain\./);
  cleanup();
});

test('the histogram compares the measurement with the exact distribution of the same run', async () => {
  const { host, view } = await draw(fakeScreen());
  view.selectTab('histogram');
  const histogram = host.querySelector('#tq-hist');
  assert.equal(histogram.series.length, 2);
  assert.equal(histogram.series[1].id, 'ideal');
  const metrics = [...host.querySelectorAll('.cmp-metric .v')].map((v) => v.textContent);
  assert.equal(metrics[0], '0.000', 'the measured half-half matches the exact half-half');
  assert.equal(metrics[1], '1.000');
  cleanup();
});

test('without an exact distribution the histogram says there is nothing to compare against', async () => {
  const screen = fakeScreen({ row: run({ artifacts: [countsArtifact] }) });
  const { host, view } = await draw(screen);
  view.selectTab('histogram');
  assert.equal(host.querySelector('#tq-hist').series.length, 1);
  assert.match(host.textContent, /nie zapisał dokładnego rozkładu/);
  cleanup();
});

test('a preselected comparison runs by itself and draws the series, the table and the diff row', async () => {
  const screen = fakeScreen();
  const { host, view } = await draw(screen, { tab: 'compare', compare: ['bbbb2222'] });
  assert.equal(view.compareIds.length, 2);
  // "Porównaj zaznaczone" is a request, not a suggestion: nothing was clicked
  // here and the comparison already ran.
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  const request = screen.requests.find(([kind]) => kind === 'tentaQuantRunCompareRequest');
  assert.deepEqual(request[1].runIds, [RUN_ID, 'bbbb2222']);
  assert.equal(host.querySelector('#tq-compare-hist').series.length, 2);
  assert.equal(host.querySelector('#tq-compare-table').rows.length, 2);
  assert.match(host.querySelector('.cmp-diff').textContent, /0\.012/);
  cleanup();
});

test('the export sends the checklist and lists what the archive actually holds', async () => {
  const screen = fakeScreen();
  const { host, view } = await draw(screen);
  view.selectTab('data');
  assert.equal(host.querySelectorAll('tf-checkbox[data-part]').length, 6);
  await view.exportPackage();
  const request = screen.requests.find(([kind]) => kind === 'tentaQuantRunExportRequest');
  assert.deepEqual(request[1].parts, [], 'every part ticked means "every part the run has"');
  assert.match(host.querySelector('#tq-export-result').textContent, /counts\.json/);
  cleanup();
});

test('unticking a part narrows the export to the parts that are left', async () => {
  const screen = fakeScreen();
  const { host, view } = await draw(screen);
  view.selectTab('data');
  const box = host.querySelector('tf-checkbox[data-part="counts_csv"]');
  box.dispatchEvent(new window.CustomEvent('change', { detail: { checked: false } }));
  await view.exportPackage();
  const request = screen.requests.find(([kind]) => kind === 'tentaQuantRunExportRequest');
  assert.equal(request[1].parts.includes('counts_csv'), false);
  assert.equal(request[1].parts.length, 5);
  cleanup();
});

test('the data view previews the two generated files of the package', async () => {
  const { host, view } = await draw(fakeScreen());
  view.selectTab('data');
  assert.match(host.querySelector('[data-method]').textContent, /# Method note/);
  assert.match(host.querySelector('[data-bib]').textContent, /@misc\{tentaquant-/);
  cleanup();
});

test('a tab switch is recorded on the route so a reload lands where the reader was', async () => {
  const screen = fakeScreen();
  const { view } = await draw(screen);
  view.selectTab('histogram');
  assert.deepEqual(screen.tabs, ['histogram']);
  cleanup();
});

test('the header leads to the notebook the run came from, and back out', async () => {
  const screen = fakeScreen();
  const { host } = await draw(screen);
  host.querySelector('[data-act="notebook"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(screen.notebook, true);
  host.querySelector('[data-act="close"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(screen.closed, 1);
  cleanup();
});

test('a run from no notebook offers no way into one', async () => {
  const { host } = await draw(fakeScreen({ row: run({ notebookId: null }) }));
  assert.equal(host.querySelector('[data-act="notebook"]'), null);
  cleanup();
});

test('the Wyjaśnij switch turns its sentence off and on again', async () => {
  const { host, view } = await draw(fakeScreen());
  assert.equal(view.explain.has('evolution'), true);
  const toggle = host.querySelector('[data-explain="evolution"]');
  toggle.dispatchEvent(new window.CustomEvent('change', { detail: { checked: false }, bubbles: true }));
  assert.equal(view.explain.has('evolution'), false);
  assert.ok(host.querySelector('#tq-explain-evolution').hasAttribute('hidden'));
  host.querySelector('[data-explain="evolution"]')
    .dispatchEvent(new window.CustomEvent('change', { detail: { checked: true }, bubbles: true }));
  assert.equal(view.explain.has('evolution'), true);
  assert.match(host.querySelector('#tq-explain-evolution').textContent, /q0/);
  cleanup();
});

// The first step has a "before": the register the circuit started from. Without
// it the opening gate is reported as having changed nothing, which is the one
// thing the Wyjaśnij box must never say about a gate that moved the vector.
test('the first gate is explained against the register it started from', async () => {
  const { host, view } = await draw(fakeScreen());
  const box = host.querySelector('#tq-explain-evolution');
  const strip = host.querySelector('#tq-strip');
  // Position 0: the H is still ahead of the playhead and says so.
  assert.match(box.textContent, /jeszcze się nie wykonała/);
  strip.dispatchEvent(new window.CustomEvent('seek', { detail: { position: 1 } }));
  assert.match(box.textContent, /Bramka H na q0 zmieniła/);
  assert.doesNotMatch(box.textContent, /nie zmieniła nic widocznego/);
  assert.match(box.textContent, /q0 zszedł z bieguna na równik/);
  strip.dispatchEvent(new window.CustomEvent('seek', { detail: { position: 0.5 } }));
  assert.match(box.textContent, /Bramka H na q0 zmieniła/, 'and halfway through it too');
  assert.equal(view.position, 0.5);
  cleanup();
});

test('the view keeps exactly one delegated listener across every repaint', async () => {
  const screen = fakeScreen();
  const { host, view } = await draw(screen);
  view.selectTab('histogram');
  view.selectTab('evolution');
  view.render();
  host.querySelector('[data-act="close"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(screen.closed, 1, 'one click, one close');
  cleanup();
});

test('the comparison tab with only this run asks nothing until a second one is added', async () => {
  const screen = fakeScreen();
  const { host, view } = await draw(screen);
  view.selectTab('compare');
  await Promise.resolve();
  assert.equal(screen.requests.some(([kind]) => kind === 'tentaQuantRunCompareRequest'), false);
  assert.match(host.querySelector('#tq-compare-body').textContent, /Dodaj co najmniej jeden run/);
  cleanup();
});
