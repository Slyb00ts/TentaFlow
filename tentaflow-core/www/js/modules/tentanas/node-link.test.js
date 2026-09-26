// =============================================================================
// File: modules/tentanas/node-link.test.js
// Description: The n18c link watcher on its own: which errors mean "the node
// did not answer", the backoff (doubling from 2 s, capped at 30 s as the
// mockup says), that the request which FAILED is never re-sent (it may have
// run on the node) while the ones issued afterwards are parked and sent once
// the probe gets an answer, that a probe answered with a refusal of the
// remote node still counts as "the node is back" (only transport loss is
// "unreachable"), that leaving releases what waited, and that
// every word of the card exists in all five locales with the same
// placeholders. Runs under happy-dom.
// =============================================================================

import { I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const { createNodeLink, isNodeUnreachable, retryDelayMs, RETRY_MAX_MS, NODE_LINK_KEYS } = await import('./node-link.js');

const REMOTE = { nodeId: 'n-remote', nodeName: 'vega', isLocal: false, online: true };
const LOCAL = { nodeId: 'n-local', nodeName: 'orion', isLocal: true };
// The forwarder's own sentence names the node by its id — exactly what must
// never reach a caller (critic wave 7, BLOCKER 1).
const NODE_ID = 'a'.repeat(64);
const lostError = () => Object.assign(new Error(`node '${NODE_ID}' did not answer: timeout`), { code: 'NodeUnreachable' });
const wordedLoss = (err) => err.code === 'NodeUnreachable' && err.message === 'Węzeł vega niedostępny' && !err.message.includes(NODE_ID);

function harness(probeAnswers, { lifecycle = null, platformDown = () => false } = {}) {
  const timers = [];
  const probes = [];
  const screen = {
    nodeId: REMOTE.nodeId, nodes: [REMOTE, LOCAL], disposed: false, recovered: [],
    loadNodes: async () => {}, leaveToFleet() { this.left = true; }, nodeRecovered(id) { this.recovered.push(id); },
  };
  const transport = {
    action: (kind, payload, opts) => {
      probes.push({ kind, opts });
      const next = probeAnswers.shift();
      return next instanceof Error ? Promise.reject(next) : Promise.resolve(next);
    },
  };
  if (lifecycle) transport.onLifecycle = (cb) => { lifecycle.push(cb); return () => lifecycle.splice(lifecycle.indexOf(cb), 1); };
  const link = createNodeLink(screen, {
    transport,
    setTimer: (fn, ms) => { timers.push({ fn, ms }); return timers.length; },
    clearTimer: () => {},
    platformDown: () => platformDown(),
  });
  return { link, screen, timers, probes };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test('only the forwarding node\'s "did not answer" is a lost node, never a refusal', () => {
  assert.equal(isNodeUnreachable(lostError()), true);
  assert.equal(isNodeUnreachable(new Error('protocol error NodeUnreachable: node x did not answer')), true);
  assert.equal(isNodeUnreachable(Object.assign(new Error('denied'), { code: 'PolicyDenied' })), false);
  assert.equal(isNodeUnreachable(new Error('refusal:job_not_cancellable')), false);
  assert.equal(isNodeUnreachable(null), false);
});

test('the backoff doubles from 2 s and stops at 30 s', () => {
  assert.deepEqual([1, 2, 3, 4, 5, 6, 12].map(retryDelayMs), [2000, 4000, 8000, 16000, 30000, 30000, 30000]);
  assert.equal(RETRY_MAX_MS, 30000);
});

test('the failed request is not re-sent; later ones wait and go once the node answers', async () => {
  const { link, screen, timers, probes } = harness([lostError(), { environment: {} }]);
  let runs = 0;
  const failing = link.send(REMOTE, () => { runs += 1; return Promise.reject(lostError()); });
  await assert.rejects(failing, wordedLoss, 'the caller gets the worded error, never the id');
  assert.equal(runs, 1, 'the failed call ran once and was handed back to its caller');
  assert.equal(link.isLost(REMOTE.nodeId), true);
  assert.equal(timers.at(-1).ms, 2000, 'first probe after 2 s');

  let sent = 0;
  const waiting = link.send(REMOTE, () => { sent += 1; return Promise.resolve('disks'); });
  await tick();
  assert.equal(sent, 0, 'a request for a lost node is parked, not sent');
  // The local node is not affected.
  assert.equal(await link.send(LOCAL, () => Promise.resolve('local')), 'local');

  // First probe: still nothing → next wait is 4 s.
  timers.at(-1).fn();
  await tick();
  assert.equal(probes[0].opts.targetNodeId, REMOTE.nodeId);
  assert.equal(timers.at(-1).ms, 4000);
  // Second probe answers → the parked request goes out and resolves.
  timers.at(-1).fn();
  assert.equal(await waiting, 'disks');
  assert.equal(sent, 1);
  assert.equal(link.isLost(REMOTE.nodeId), false);
  assert.deepEqual(screen.recovered, [REMOTE.nodeId]);
  link.destroy();
});

test('a probe answered with a refusal of the remote node means the node is back', async () => {
  const { link } = harness([Object.assign(new Error('the TentaNas app is not available'), { code: 'AppUnavailable' })]);
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  const waiting = link.send(REMOTE, () => Promise.resolve('ok'));
  document.querySelector('.conn-overlay.nas-conn [data-action="retry"]').click();
  try {
    const parked = new Promise((_, reject) => setTimeout(() => reject(new Error('still parked as unreachable')), 500));
    assert.equal(await Promise.race([waiting, parked]), 'ok');
  } finally {
    link.destroy();
  }
});

// Critic wave 9a, MINOR 9: only TRANSPORT loss is "unreachable". A node
// that answers with an application error — its environment read failing
// (`Internal`), a gate refusing (`PolicyDenied`) — ends the lost state, the
// parked requests go out (each to be shown with its own error), and the
// probe's error is shown as an error rather than an endless blurred card.
test('a node that answers with an application error is not left blurred as unreachable', async () => {
  // Every toast, recorded as it is appended (its container may be detached).
  const toasts = [];
  const append = window.Node.prototype.appendChild;
  window.Node.prototype.appendChild = function (child) {
    if (child?.classList?.contains('toast')) toasts.push(child.textContent);
    return append.call(this, child);
  };
  const { link, timers } = harness([
    new Error('socket closed'),
    Object.assign(new Error('environment probe: database is locked'), { code: 'Internal' }),
  ]);
  try {
    await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
    let sent = 0;
    const waiting = link.send(REMOTE, () => { sent += 1; return Promise.resolve('ok'); });
    timers.at(-1).fn();
    await tick();
    assert.equal(link.isLost(REMOTE.nodeId), true, 'a transport error is loss');
    assert.equal(sent, 0);
    timers.at(-1).fn();
    const parked = new Promise((_, reject) => setTimeout(() => reject(new Error('still parked as unreachable')), 500));
    assert.equal(await Promise.race([waiting, parked]), 'ok', 'the node answered: the parked request goes out');
    assert.equal(link.isLost(REMOTE.nodeId), false, 'not held blurred on "unreachable"');
    assert.equal(document.querySelector('#app-root.nas-link-lost, .nas-link-lost'), null, 'nothing stays blurred');
    await tick();
    assert.ok(toasts.some((t) => t.includes('database is locked')), `the error is shown: ${JSON.stringify(toasts)}`);
  } finally {
    window.Node.prototype.appendChild = append;
    link.destroy();
  }
});

// Critic wave 7, MINOR 4: only an ANSWER from the node — a success or a
// protocol refusal — is recovery. A transport error (the platform socket
// down, a probe timeout) carries no code: the parked requests stay parked and
// the next probe is scheduled. The probe itself waits at most 10 s.
test('a probe that fails in the transport is no answer: nothing is released and the next probe is scheduled', async () => {
  const { link, timers, probes } = harness([new Error('socket closed'), new Error('request tentaNasEnvironmentRequest timed out after 10000ms'), { environment: {} }]);
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  let sent = 0;
  const waiting = link.send(REMOTE, () => { sent += 1; return Promise.resolve('ok'); });
  timers.at(-1).fn();
  await tick();
  assert.equal(link.isLost(REMOTE.nodeId), true, 'a closed socket is not the node answering');
  assert.equal(sent, 0, 'the parked request is not released into a dead socket');
  assert.equal(timers.at(-1).ms, 4000, 'the next probe is scheduled');
  timers.at(-1).fn();
  await tick();
  assert.equal(link.isLost(REMOTE.nodeId), true, 'a timeout is no answer either');
  assert.equal(probes[0].opts.timeoutMs, 10000, 'a probe waits 10 s, not the forwarder\'s 45 s');
  timers.at(-1).fn();
  assert.equal(await waiting, 'ok');
  assert.equal(sent, 1);
  link.destroy();
});

// Critic wave 7, MINOR 1: n18c line 1 is "Kolejna próba za 7 s", following
// the ring, and the whole app is blurred, not only the TentaNas pane.
test('the card counts down to the next attempt in words and blurs the whole app', async () => {
  const app = document.createElement('div');
  app.id = 'app-root';
  document.body.appendChild(app);
  const { link } = harness([]);
  try {
    await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
    const card = document.querySelector('.conn-overlay.nas-conn');
    assert.equal(card.querySelector('.conn-retry-info .line-1').textContent, 'Kolejna próba za 2 s');
    assert.equal(card.querySelector('.conn-retry-info .line-2').textContent, 'próba 1 · backoff do 30 s');
    assert.ok(app.classList.contains('nas-link-lost'), 'the app root carries the blur');
    const css = readFileSync(new URL('../../../css/tentanas.css', import.meta.url), 'utf8');
    assert.match(css, /\n\.nas-link-lost \{\s*filter: blur/, 'the blur applies wherever the class lands');
  } finally {
    link.destroy();
    app.remove();
  }
  assert.equal(app.classList.contains('nas-link-lost'), false);
});

// Critic wave 7, MINOR 3: a node lost while its card could not be shown (the
// platform overlay was up) gets its card when the platform's socket opens
// again, and a probe at once — never parked requests with nothing on screen.
test('when the platform comes back, a lost node\'s card is shown and the node is probed at once', async () => {
  const lifecycle = [];
  let down = true;
  const { link, probes } = harness([lostError()], { lifecycle, platformDown: () => down });
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  const card = document.querySelector('.conn-overlay.nas-conn');
  const shown = () => card.classList.contains('visible');
  assert.equal(shown(), false, 'the platform overlay outranks the node card');
  assert.equal(lifecycle.length, 1, 'the link listens to the platform socket');
  lifecycle[0]({ type: 'reconnect-scheduled' });
  assert.equal(probes.length, 0, 'only an open socket is news');
  down = false;
  lifecycle[0]({ type: 'open' });
  assert.equal(shown(), true, 'the card is shown as soon as the platform is back');
  await tick();
  assert.equal(probes.length, 1, 'and the node is probed at once');
  assert.equal(link.isLost(REMOTE.nodeId), true, 'still lost: the probe got no answer');
  link.destroy();
  assert.equal(lifecycle.length, 0, 'destroy stops listening');
});

test('leaving the lost node releases every parked request with the error that parked it', async () => {
  const { link, screen } = harness([]);
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  const waiting = link.send(REMOTE, () => Promise.resolve('never'));
  document.querySelector('.conn-overlay.nas-conn [data-action="fleet"]').click();
  await assert.rejects(waiting, (err) => err.code === 'NodeUnreachable' && !err.message.includes(NODE_ID), 'a released waiter gets no id either');
  assert.equal(screen.left, true);
  assert.equal(link.isLost(REMOTE.nodeId), false);
  link.destroy();
  assert.equal(document.querySelector('.conn-overlay.nas-conn'), null);
});

test('a dialog open when the node goes away is inert until it answers, and the card sits above dialogs', async () => {
  const dialog = document.createElement('tf-window');
  document.body.appendChild(dialog);
  const { link } = harness([{ environment: {} }]);
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  assert.equal(dialog.inert, true, 'nothing under the card can be clicked or confirmed with Enter');
  const waiting = link.send(REMOTE, () => Promise.resolve('ok'));
  document.querySelector('.conn-overlay.nas-conn [data-action="retry"]').click();
  await waiting;
  assert.equal(dialog.inert, false, 'the dialog is usable again once the node answers');
  link.destroy();
  dialog.remove();
  const css = readFileSync(new URL('../../../css/tentanas.css', import.meta.url), 'utf8');
  const z = Number(/\.conn-overlay\.nas-conn\s*\{\s*z-index:\s*(\d+)/.exec(css)?.[1]);
  assert.ok(z > 9991 && z < 9999, `above tf-modal (9991), below the platform overlay (9999): ${z}`);
});

test('a node the screen is not showing raises no overlay', async () => {
  const { link, screen } = harness([]);
  screen.nodeId = null;
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  assert.equal(link.isLost(REMOTE.nodeId), false);
  link.destroy();
});

test('every word of the card exists in all five locales with the same placeholders', () => {
  const load = (l) => JSON.parse(readFileSync(new URL(`../../../i18n/${l}.json`, import.meta.url), 'utf8')).tentanas;
  const get = (tree, key) => key.split('.').reduce((n, k) => (n ? n[k] : undefined), tree);
  const holes = (s) => [...String(s).matchAll(/\{(\w+)/g)].map((m) => m[1]).sort().join(',');
  const pl = load('pl');
  for (const locale of ['pl', 'en', 'de', 'fr', 'es']) {
    const tree = load(locale);
    for (const key of NODE_LINK_KEYS) {
      const value = get(tree, key);
      assert.equal(typeof value, 'string', `${locale}: ${key}`);
      assert.equal(holes(value), holes(get(pl, key)), `${locale}: ${key} placeholders`);
    }
  }
  assert.ok(I18n);
});
