// =============================================================================
// File: modules/tentanas/node-link.test.js
// Description: The n18c link watcher on its own: which errors mean "the node
// did not answer", the backoff (doubling from 2 s, capped at 30 s as the
// mockup says), that the request which FAILED is never re-sent (it may have
// run on the node) while the ones issued afterwards are parked and sent once
// the probe gets an answer, that a probe answered with a refusal still
// counts as "the node is back", that leaving releases what waited, and that
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

function harness(probeAnswers) {
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
  const link = createNodeLink(screen, {
    transport,
    setTimer: (fn, ms) => { timers.push({ fn, ms }); return timers.length; },
    clearTimer: () => {},
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

test('a probe answered with a refusal means the node is back', async () => {
  const { link } = harness([Object.assign(new Error('not allowed'), { code: 'PolicyDenied' })]);
  await link.send(REMOTE, () => Promise.reject(lostError())).catch(() => {});
  const waiting = link.send(REMOTE, () => Promise.resolve('ok'));
  document.querySelector('.conn-overlay.nas-conn [data-action="retry"]').click();
  assert.equal(await waiting, 'ok');
  link.destroy();
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
