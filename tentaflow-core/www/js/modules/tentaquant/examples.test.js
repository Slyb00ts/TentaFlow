// =============================================================================
// File: modules/tentaquant/examples.test.js
// Description: The Przykłady tab (Q10). The decisions — which text, which
// examples a filter leaves, which width is allowed, how the reference outcome
// reads — are pure and pinned without a DOM; the gallery and the page of one
// example are driven through a fake screen whose `tq` answers what Core's
// `Example*` handlers answer, including the fork that hands the Studio the new
// circuit cell. The wasm front end is absent in Node, so the drawing falls back
// to the text view, which is exactly what this build must do without it.
// =============================================================================

import { window, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  clampWidth, drawExamples, exampleDescription, exampleTitle, examplesState, filterExamples,
  isParametric, levelOptions, outcomeRows, widthLabel,
} = await import('./examples.js');

const BELL = {
  exampleId: 'bell-state',
  position: 1,
  titles: { pl: 'Stan Bella i korelacje', en: 'Bell state and correlations' },
  descriptions: { pl: 'Dwa kubity, H + CNOT, pomiar.', en: 'Two qubits, H + CNOT, measurement.' },
  level: 'intro',
  tags: ['entanglement', 'measurement'],
  qubits: 2, qubitsMin: 2, qubitsMax: 2, qubitsDefault: 2, depth: 3,
};
const GHZ = {
  exampleId: 'ghz',
  position: 2,
  titles: { pl: 'Stan GHZ — n kubitów', en: 'GHZ state — n qubits' },
  descriptions: { pl: 'Splątanie n kubitów jednym łańcuchem CNOT.', en: 'Entangles n qubits.' },
  level: 'advanced',
  tags: ['entanglement', 'scaling'],
  qubits: 5, qubitsMin: 3, qubitsMax: 28, qubitsDefault: 5, depth: 6,
};
const LIST = { examples: [BELL, GHZ] };

const detailOf = (example, qubits) => {
  const width = qubits ?? example.qubits;
  const keys = example.exampleId === 'ghz' ? ['0'.repeat(width), '1'.repeat(width)] : ['00', '11'];
  return {
    example: { ...example, qubits: width },
    readme: { pl: '## Co tu się dzieje\n\nH i CNOT.', en: '## What happens\n\nH and CNOT.' },
    qasm3: `OPENQASM 3.0;\nqubit[${width}] q;\n`,
    expected: { shots: 4096, seed: 7, tolerance: 0.05, outcomes: { [keys[0]]: 0.5, [keys[1]]: 0.5 } },
  };
};

// ---- pure ------------------------------------------------------------------

test('titles and descriptions are read in the dashboard language, then English', () => {
  assert.equal(exampleTitle(BELL), 'Stan Bella i korelacje');
  assert.equal(exampleDescription({ descriptions: { en: 'only English' } }), 'only English');
  assert.equal(exampleTitle(null), '');
});

test('a width label is plural-correct for a fixed circuit and a range', () => {
  assert.equal(widthLabel(BELL), '2 kubity');
  assert.equal(widthLabel(GHZ), '3–28 kubitów');
  assert.equal(widthLabel({ qubitsMin: 1, qubitsMax: 1 }), '1 kubit');
  assert.equal(isParametric(GHZ), true);
  assert.equal(isParametric(BELL), false);
});

test('a typed width is clamped into the example\'s range and garbage keeps the old one', () => {
  assert.equal(clampWidth(GHZ, '12', 5), 12);
  assert.equal(clampWidth(GHZ, '1', 5), 3);
  assert.equal(clampWidth(GHZ, '99', 5), 28);
  assert.equal(clampWidth(GHZ, 'abc', 5), 5);
  assert.equal(clampWidth(GHZ, 'abc', 5), 5);
});

test('levels offered are the ones present, in teaching order', () => {
  assert.deepEqual(levelOptions([GHZ, BELL]), ['all', 'intro', 'advanced']);
  assert.deepEqual(levelOptions([]), ['all']);
});

test('the filter matches title, description and tags, and the level', () => {
  const all = [BELL, GHZ];
  assert.deepEqual(filterExamples(all, {}).map((e) => e.exampleId), ['bell-state', 'ghz']);
  assert.deepEqual(filterExamples(all, { query: 'bella' }).map((e) => e.exampleId), ['bell-state']);
  assert.deepEqual(filterExamples(all, { query: 'ŁAŃCUCHEM' }).map((e) => e.exampleId), ['ghz'], 'case-insensitive');
  assert.deepEqual(filterExamples(all, { query: 'scaling' }).map((e) => e.exampleId), ['ghz']);
  assert.deepEqual(filterExamples(all, { level: 'intro' }).map((e) => e.exampleId), ['bell-state']);
  assert.deepEqual(filterExamples(all, { query: 'scaling', level: 'intro' }), []);
});

test('the reference outcome lists the likeliest bitstrings first', () => {
  assert.deepEqual(outcomeRows({ outcomes: { '11': 0.5, '00': 0.5 } }), [
    { bits: '00', probability: 0.5 }, { bits: '11', probability: 0.5 },
  ]);
  assert.deepEqual(outcomeRows({ outcomes: { a: 0.25, b: 0.75 } }).map((r) => r.bits), ['b', 'a']);
  assert.deepEqual(outcomeRows(null), []);
});

// ---- the view --------------------------------------------------------------

function fakeScreen({ permissions = ['quant.read', 'quant.run'], fork } = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  const screen = {
    root,
    tab: 'examples',
    instanceId: 'tentaquant-0a1b2c3d',
    disposed: false,
    lab: { myPermissions: permissions },
    examples: examplesState(),
    requests: [],
    opened: [],
    studio: [],
    locations: 0,
    setLocation() { this.locations += 1; },
    async openProject(projectId) { this.opened.push(projectId); },
    async openStudioWithCell(cell) { this.studio.push(cell); },
    async tq(kind, payload = {}) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantExampleListRequest') return LIST;
      if (kind === 'tentaQuantExampleGetRequest') {
        const found = LIST.examples.find((e) => e.exampleId === payload.exampleId);
        if (!found) throw new Error('example not found');
        return detailOf(found, payload.qubits ?? undefined);
      }
      if (kind === 'tentaQuantExampleForkRequest') return fork(payload);
      throw new Error(`unexpected ${kind}`);
    },
  };
  const host = window.document.createElement('div');
  root.appendChild(host);
  return { screen, host };
}

const cleanup = () => { window.document.body.innerHTML = ''; };
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const click = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
const FORK = (payload) => ({
  project: { projectId: 'p1', name: 'Stan GHZ — n kubitów' },
  notebook: { notebookId: 'n1', name: 'Stan GHZ — n kubitów' },
  circuitCellId: 'c1',
  echo: payload,
});

test('the gallery draws one card per example with width, depth and level', async () => {
  const { screen, host } = fakeScreen();
  await drawExamples(screen, host);
  assert.equal(host.querySelectorAll('.ex-card').length, 2);
  const card = host.querySelector('.ex-card[data-example="ghz"]');
  assert.match(card.textContent, /Stan GHZ/);
  assert.match(card.textContent, /3–28 kubitów/);
  assert.match(card.textContent, /Zaawansowany/);
  assert.match(host.querySelector('.tq-table-footer').textContent, /Pokazuję 2 z 2/);
  // No variant tabs and no run times: nothing ships that could back them.
  assert.equal(host.querySelector('[data-variant]'), null);
  assert.doesNotMatch(host.textContent, /QPU|GPU/);
  cleanup();
});

test('the level filter narrows the gallery and says so when nothing is left', async () => {
  const { screen, host } = fakeScreen();
  await drawExamples(screen, host);
  host.querySelector('#tq-ex-level').dispatchEvent(new window.CustomEvent('change', { detail: { value: 'intro' } }));
  assert.deepEqual([...host.querySelectorAll('.ex-card')].map((c) => c.dataset.example), ['bell-state']);
  host.querySelector('#tq-ex-search').dispatchEvent(new window.CustomEvent('search', { detail: { value: 'zzz' } }));
  assert.equal(host.querySelectorAll('.ex-card').length, 0);
  assert.ok(host.querySelector('tf-empty-state'));
  cleanup();
});

test('opening a card loads the example, follows the route and shows README, source and outcome', async () => {
  const { screen, host } = fakeScreen();
  await drawExamples(screen, host);
  click(host.querySelector('.ex-card[data-example="bell-state"]'));
  await tick();
  await tick();
  assert.equal(screen.examples.exampleId, 'bell-state');
  assert.ok(screen.locations > 0);
  assert.deepEqual(screen.requests.at(-1), ['tentaQuantExampleGetRequest', { exampleId: 'bell-state', qubits: null }]);
  assert.match(host.querySelector('.kata-head h3').textContent, /Stan Bella i korelacje/);
  assert.equal(host.querySelector('#tq-ex-source').value, 'OPENQASM 3.0;\nqubit[2] q;\n');
  assert.deepEqual(
    [...host.querySelectorAll('.ex-outcome')].map((r) => [r.querySelector('.mono').textContent, r.querySelector('.pts').textContent]),
    [['00', '50 %'], ['11', '50 %']],
  );
  assert.equal(host.querySelector('#tq-ex-width'), null, 'a fixed circuit has no width control');
  cleanup();
});

test('a parametric example gets a width control that reloads the circuit at that width', async () => {
  const { screen, host } = fakeScreen();
  screen.examples = examplesState({ exampleId: 'ghz' });
  await drawExamples(screen, host);
  const width = host.querySelector('#tq-ex-width');
  assert.equal(width.getAttribute('min'), '3');
  assert.equal(width.getAttribute('max'), '28');
  width.value = '9';
  width.dispatchEvent(new window.Event('change', { bubbles: true }));
  await tick();
  await tick();
  assert.deepEqual(screen.requests.at(-1), ['tentaQuantExampleGetRequest', { exampleId: 'ghz', qubits: 9 }]);
  assert.equal(host.querySelector('#tq-ex-source').value, 'OPENQASM 3.0;\nqubit[9] q;\n');
  assert.deepEqual([...host.querySelectorAll('.ex-outcome .mono')].map((r) => r.textContent), ['0'.repeat(9), '1'.repeat(9)]);
  cleanup();
});

test('Fork sends the example, the width and the language, then opens the new project', async () => {
  const { screen, host } = fakeScreen({ fork: FORK });
  screen.examples = examplesState({ exampleId: 'ghz', qubits: 9 });
  await drawExamples(screen, host);
  click(host.querySelector('[data-act="fork"]'));
  await tick();
  await tick();
  assert.deepEqual(screen.requests.at(-1), ['tentaQuantExampleForkRequest', { exampleId: 'ghz', qubits: 9, language: 'pl' }]);
  assert.deepEqual(screen.opened, ['p1']);
  assert.deepEqual(screen.studio, [], 'a plain fork does not jump to the Studio');
  cleanup();
});

test('Open in studio forks, then hands the Studio the circuit cell that was just made', async () => {
  const { screen, host } = fakeScreen({ fork: FORK });
  screen.examples = examplesState({ exampleId: 'bell-state' });
  await drawExamples(screen, host);
  click(host.querySelector('[data-act="studio"]'));
  await tick();
  await tick();
  assert.deepEqual(screen.opened, ['p1']);
  assert.deepEqual(screen.studio, [{
    notebookId: 'n1', cellId: 'c1', source: 'OPENQASM 3.0;\nqubit[2] q;\n', name: 'Stan GHZ — n kubitów',
  }]);
  cleanup();
});

test('a card\'s own Fork button copies without opening the page', async () => {
  const { screen, host } = fakeScreen({ fork: FORK });
  await drawExamples(screen, host);
  click(host.querySelector('.ex-card[data-example="bell-state"] [data-act="fork"]'));
  await tick();
  await tick();
  assert.equal(screen.examples.exampleId, null);
  assert.deepEqual(screen.requests.at(-1), ['tentaQuantExampleForkRequest', { exampleId: 'bell-state', qubits: undefined, language: 'pl' }]);
  assert.deepEqual(screen.opened, ['p1']);
  cleanup();
});

test('a person without quant.run reads the gallery but cannot fork', async () => {
  const { screen, host } = fakeScreen({ permissions: ['quant.read'] });
  await drawExamples(screen, host);
  assert.ok([...host.querySelectorAll('[data-act="fork"]')].every((b) => b.hasAttribute('disabled')));
  assert.match(host.querySelector('tf-alert').getAttribute('message'), /quant\.run/);
  cleanup();
});

test('a failed fork is an error toast and leaves the page where it was', async () => {
  const { screen, host } = fakeScreen({ fork: () => { throw new Error('denied'); } });
  screen.examples = examplesState({ exampleId: 'bell-state' });
  await drawExamples(screen, host);
  click(host.querySelector('[data-act="fork"]'));
  await tick();
  await tick();
  assert.deepEqual(screen.opened, []);
  assert.equal(host.querySelector('[data-act="fork"]').hasAttribute('disabled'), false, 'the button is usable again');
  cleanup();
});

test('a route naming an example this build does not ship falls back to the gallery', async () => {
  const { screen, host } = fakeScreen();
  screen.examples = examplesState({ exampleId: 'no-such-example' });
  await drawExamples(screen, host);
  assert.equal(screen.examples.exampleId, null);
  assert.equal(host.querySelectorAll('.ex-card').length, 2);
  cleanup();
});

test('a failed list is an alert, and an answer that arrives after the tab changed paints nothing', async () => {
  const failing = fakeScreen();
  failing.screen.tq = async () => { throw new Error('boom'); };
  await drawExamples(failing.screen, failing.host);
  assert.equal(failing.host.querySelector('tf-alert').getAttribute('message'), 'boom');

  const late = fakeScreen();
  const pending = drawExamples(late.screen, late.host);
  late.screen.tab = 'course';
  await pending;
  assert.equal(late.host.querySelector('.ex-card'), null);
  cleanup();
});

test('the page speaks the dashboard language', async () => {
  await I18n.setLanguage('en');
  const { screen, host } = fakeScreen();
  await drawExamples(screen, host);
  assert.match(host.querySelector('.ex-card').textContent, /Bell state and correlations/);
  assert.match(host.querySelector('.ex-card').textContent, /Beginner/);
  await I18n.setLanguage('pl');
  cleanup();
});
