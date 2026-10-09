// =============================================================================
// File: modules/tentaquant/devices.test.js
// Description: The Urządzenia tab (Q09). The cards are derived from exactly what
// `Target::List` answers — a refused target keeps the server's sentence, no tier
// the server did not list becomes a device — and the `auto` probe puts its
// question to `Target::Resolve` and prints the answer, debounced and race-safe.
// The view is driven through a fake screen whose `tq` answers what Core's
// handlers answer, so a wrong field name between wire and markup fails here.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  PROBE_MAX_QUBITS, clampProbeQubits, deviceCards, deviceTotals, drawDevices, probeRequest,
  probeState, resolutionView,
} = await import('./devices.js');

const browser = {
  target: 'browser', tier: 'T0', nodeId: null, nodeName: 'browser', isLocal: true,
  online: true, available: true, maxQubits: 24, precision: 'single', reason: null,
};
const local = {
  target: 'core:node-a', tier: 'T1', nodeId: 'node-a', nodeName: 'node-a', isLocal: true,
  online: true, available: true, maxQubits: 28, precision: 'double', reason: null,
};
const remote = {
  target: 'core:node-b', tier: 'T1', nodeId: 'node-b', nodeName: 'node-b', isLocal: false,
  online: true, available: false, maxQubits: 28, precision: 'double',
  reason: 'runs on another node cannot stream their evolution here yet',
};
const LIST = {
  localNodeId: 'node-a',
  // Deliberately out of order: the view orders them, the wire does not promise to.
  targets: [remote, local, browser],
  unavailable: [
    { tier: 'T2', reason: 'the `quantum-python` kernel service is not part of this build' },
    { tier: 'T3', reason: 'no GPU tier in this build: the simulator runs on the CPU' },
  ],
};

// ---- pure ------------------------------------------------------------------

test('the cards are the browser, then this node, then the rest — and only what the list holds', () => {
  const cards = deviceCards(LIST);
  assert.deepEqual(cards.map((c) => c.target), ['browser', 'core:node-a', 'core:node-b']);
  assert.deepEqual(cards.map((c) => c.tier), ['T0', 'T1', 'T1']);
  assert.ok(cards.every((c) => ['T0', 'T1'].includes(c.tier)), 'no T2/T3/T4 card exists');
});

test('a card carries the ceiling, the precision and the memory the ceiling costs', () => {
  const [web, core] = deviceCards(LIST);
  const rows = (card) => Object.fromEntries(card.rows.map((r) => [r.key, r.value]));
  // 2^24 single-precision complex amplitudes are 8 B each; 2^28 double ones 16 B.
  assert.deepEqual(rows(web), { where: 'ten komputer', max_qubits: '24', precision: 'f32', memory: '128 MB' });
  assert.deepEqual(rows(core), { where: 'node, z którym jesteś połączony', max_qubits: '28', precision: 'f64', memory: '4.0 GB' });
});

test('a refused target keeps the server\'s own reason and is marked unavailable', () => {
  const card = deviceCards(LIST)[2];
  assert.equal(card.available, false);
  assert.equal(card.reason, 'runs on another node cannot stream their evolution here yet');
  assert.equal(deviceCards(LIST)[1].reason, '');
  assert.equal(deviceCards({ targets: [{ ...remote, reason: null }] })[0].reason, 'bez podanego powodu');
});

test('the totals count what takes a run now', () => {
  assert.deepEqual(deviceTotals(LIST), { available: 2, total: 3, widest: 28, nodes: 2 });
  assert.deepEqual(deviceTotals({ targets: [] }), { available: 0, total: 0, widest: 0, nodes: 0 });
  assert.equal(deviceTotals({ targets: [remote] }).widest, 0, 'a refused target does not widen the register');
});

test('a probe width is a whole number from 1 up, otherwise the previous one', () => {
  assert.equal(clampProbeQubits('12', 5), 12);
  assert.equal(clampProbeQubits('12.9', 5), 12);
  assert.equal(clampProbeQubits('', 5), 5);
  assert.equal(clampProbeQubits('0', 5), 5);
  assert.equal(clampProbeQubits('-3', 5), 5);
  assert.equal(clampProbeQubits('abc', 5), 5);
  assert.equal(clampProbeQubits('900', 5), PROBE_MAX_QUBITS);
  assert.deepEqual(probeRequest(probeState({ qubits: 26, needsKernel: true })), {
    numQubits: 26, fromBrowser: true, needsKernel: true,
  });
});

test('the answer reads like the hint under a run select, with the tiers that were skipped', () => {
  const view = resolutionView({
    target: 'core:node-a', tier: 'T1', nodeId: 'node-a', reason: 'wider than the browser tier',
    unavailable: [{ tier: 'T2', reason: 'not part of this build' }],
  }, LIST.targets);
  assert.deepEqual(view, {
    tone: 'ok', headline: 'auto → T1 · node-a', reason: 'wider than the browser tier',
    skipped: [{ tier: 'T2', reason: 'not part of this build' }],
  });
  const none = resolutionView({ target: '', tier: 'none', reason: 'too wide', unavailable: [] }, LIST.targets);
  assert.equal(none.tone, 'warn');
  assert.equal(none.headline, 'auto → żadna warstwa nie przyjmie tego obwodu');
  assert.equal(resolutionView(null, LIST.targets), null);
});

// ---- the view --------------------------------------------------------------

function fakeScreen({ list = LIST, resolve } = {}) {
  const root = window.document.createElement('div');
  root.className = 'tq-root';
  window.document.body.appendChild(root);
  const screen = {
    root,
    tab: 'devices',
    instanceId: 'tentaquant-0a1b2c3d',
    disposed: false,
    requests: [],
    probe: null,
    async tq(kind, payload = {}) {
      this.requests.push([kind, payload]);
      if (kind === 'tentaQuantTargetListRequest') return list;
      if (kind === 'tentaQuantTargetResolveRequest') return resolve(payload);
      throw new Error(`unexpected ${kind}`);
    },
  };
  const host = window.document.createElement('div');
  root.appendChild(host);
  return { screen, host };
}

const cleanup = () => { window.document.body.innerHTML = ''; };
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const resolvesTo = (payload) => (payload.numQubits > 28
  ? { target: '', tier: 'none', nodeId: null, reason: 'too wide for every tier', unavailable: [] }
  : { target: 'core:node-a', tier: 'T1', nodeId: 'node-a', reason: 'fits Core', unavailable: [] });

test('the tab draws one card per listed target, the unavailable note and a live first answer', async () => {
  const { screen, host } = fakeScreen({ resolve: resolvesTo });
  await drawDevices(screen, host);
  await wait(0);
  assert.equal(host.querySelectorAll('.dev-card').length, 3);
  assert.equal(host.querySelectorAll('.dev-card.is-disabled').length, 1);
  assert.match(host.querySelector('.dev-card.is-disabled .dev-reason').textContent, /cannot stream their evolution/);
  assert.equal(host.querySelectorAll('tf-stat-card').length, 3);
  assert.equal(host.querySelectorAll('.dev-missing-row').length, 2);
  assert.match(host.querySelector('.dev-missing-row').textContent, /T2.*quantum-python/);
  // The first probe goes out without anybody touching the control.
  assert.deepEqual(screen.requests.at(-1), ['tentaQuantTargetResolveRequest', { numQubits: 5, fromBrowser: true, needsKernel: false }]);
  assert.equal(host.querySelector('.dev-answer .cr-title').textContent, 'auto → T1 · node-a');
  cleanup();
});

test('changing the width asks the rule once, after the typing settles', async () => {
  const { screen, host } = fakeScreen({ resolve: resolvesTo });
  await drawDevices(screen, host);
  await wait(0);
  const input = host.querySelector('#tq-probe-qubits');
  const before = screen.requests.length;
  for (const value of ['2', '29']) {
    input.value = value;
    input.dispatchEvent(new window.Event('input', { bubbles: true }));
  }
  await wait(350);
  const asked = screen.requests.slice(before);
  assert.equal(asked.length, 1, 'two keystrokes inside the debounce make one question');
  assert.equal(asked[0][1].numQubits, 29);
  assert.match(host.querySelector('.dev-answer .cr-title').textContent, /żadna warstwa/);
  assert.ok(host.querySelector('.dev-answer .check-result.warn'));
  cleanup();
});

test('the two switches change the question they send', async () => {
  const { screen, host } = fakeScreen({ resolve: resolvesTo });
  await drawDevices(screen, host);
  await wait(0);
  host.querySelector('#tq-probe-kernel').dispatchEvent(new window.CustomEvent('change', { detail: { checked: true } }));
  host.querySelector('#tq-probe-browser').dispatchEvent(new window.CustomEvent('change', { detail: { checked: false } }));
  await wait(350);
  assert.deepEqual(screen.requests.at(-1)[1], { numQubits: 5, fromBrowser: false, needsKernel: true });
  cleanup();
});

test('an older answer never overwrites a newer one', async () => {
  const gates = [];
  const { screen, host } = fakeScreen({
    resolve: (payload) => new Promise((resolve) => gates.push(() => resolve(resolvesTo(payload)))),
  });
  await drawDevices(screen, host);
  await wait(0);
  const first = gates.shift();
  host.querySelector('#tq-probe-qubits').value = '40';
  host.querySelector('#tq-probe-qubits').dispatchEvent(new window.Event('input', { bubbles: true }));
  await wait(350);
  const second = gates.shift();
  second();
  await wait(0);
  first();
  await wait(0);
  assert.match(host.querySelector('.dev-answer .cr-title').textContent, /żadna warstwa/, 'the 40-qubit answer stays');
  cleanup();
});

test('a probe that fails is an alert, a failed list is an alert, and an empty list says so', async () => {
  const probeFails = fakeScreen({ resolve: () => { throw new Error('rule down'); } });
  await drawDevices(probeFails.screen, probeFails.host);
  await wait(0);
  assert.equal(probeFails.host.querySelector('.dev-answer tf-alert').getAttribute('message'), 'rule down');

  const listFails = fakeScreen();
  listFails.screen.tq = async () => { throw new Error('boom'); };
  await drawDevices(listFails.screen, listFails.host);
  assert.equal(listFails.host.querySelector('tf-alert').getAttribute('message'), 'boom');

  const empty = fakeScreen({ list: { targets: [], unavailable: [] } });
  await drawDevices(empty.screen, empty.host);
  assert.ok(empty.host.querySelector('tf-empty-state'));
  cleanup();
});

test('an answer that arrives after the tab changed paints nothing', async () => {
  const { screen, host } = fakeScreen({ resolve: resolvesTo });
  const pending = drawDevices(screen, host);
  screen.tab = 'course';
  await pending;
  assert.equal(host.querySelector('.dev-card'), null);
  cleanup();
});
