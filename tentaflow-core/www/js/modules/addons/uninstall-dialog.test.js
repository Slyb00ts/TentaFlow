// =============================================================================
// File: modules/addons/uninstall-dialog.test.js
// Description: The uninstall dialog against a stubbed transport: it renders
// the teardown plan (removed vs kept paths with sizes, dependents), keeps the
// danger button locked until the instance name is retyped exactly, and only
// then sends AddonUninstallRequest and closes. Runs under happy-dom with the
// `/js/` resolver hook.
// =============================================================================

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from 'node:module';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';
import { readFileSync } from 'node:fs';

const here = fileURLToPath(import.meta.url);
const WWW_ROOT = pathResolve(dirname(here), '..', '..', '..');
const hookSource = `
  const WWW_ROOT_URL = ${JSON.stringify(pathToFileURL(WWW_ROOT + '/').href)};
  export async function resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('/js/')) {
      return { url: new URL('.' + specifier, WWW_ROOT_URL).href, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  }
`;
register('data:text/javascript,' + encodeURIComponent(hookSource), import.meta.url);

if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
// Locale files come from disk so the dialog renders real strings (the
// dependents line interpolates names into the translation); every other
// fetch (component stylesheets) answers empty.
globalThis.fetch = (url) => {
  const m = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (m) {
    const text = readFileSync(pathResolve(WWW_ROOT, 'i18n', `${m[1]}.json`), 'utf8');
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)), text: () => Promise.resolve(text) });
  }
  return Promise.resolve({ ok: true, text: () => Promise.resolve('') });
};
// i18n persists the chosen language; Node has no Web Storage.
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}
// codec.js starts a WASM fetch at import time that rejects under Node; the
// dialog under test never touches the codec because the transport is stubbed.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { I18n } = await import('../../i18n.js');
const { openUninstallDialog } = await import('./uninstall-dialog.js');
await import('../../protocol/codec.js').then((m) => m.codecReady).catch(() => {});
ApiBinary.one = () => Promise.resolve({});
ApiBinary.action = () => Promise.resolve({});
// The default language is already 'en' (a no-op for setLanguage), so load
// Polish: the assertions only look at interpolated names and paths.
await I18n.setLanguage('pl');

const flush = () => new Promise((r) => setTimeout(r, 0));
const click = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true, composed: true }));

const calls = [];
function stubTransport(fixtures) {
  const answer = (kind, payload) => {
    calls.push({ kind, payload });
    if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
    const f = fixtures[kind];
    return Promise.resolve(typeof f === 'function' ? f(payload) : f);
  };
  ApiBinary.one = answer;
  ApiBinary.action = answer;
}

const plan = {
  addonId: 'tentanas-0a1b2c3d',
  displayName: 'TentaNas',
  entries: [
    { path: '/var/lib/tentaflow/orgs/default/addons/tentanas-0a1b2c3d', kind: 'tentanas_data_dir', description: 'instance data directory', removed: true, sizeBytes: 3 * 1024 * 1024 },
    { path: '/usr/local/libexec/tentanas-helper', kind: 'tentanas_helper', description: 'privilege helper', removed: false, sizeBytes: 2048 },
  ],
  dependents: [{ addonId: 'backup-11111111', displayName: 'Backup', optional: false }],
};

function open(fixtures, onDone) {
  calls.length = 0;
  document.body.innerHTML = '';
  const win = openUninstallDialog({ addonId: plan.addonId, displayName: 'TentaNas', onDone });
  return win;
}

function confirmButton(win) {
  return win.querySelector('tf-button[data-action="confirm"]');
}

test('renders removed and kept entries with sizes and the dependents warning', async () => {
  stubTransport({ addonTeardownPlanRequest: plan });
  const win = open();
  await flush();
  assert.equal(calls[0].kind, 'addonTeardownPlanRequest');
  assert.deepEqual(calls[0].payload, { addonId: plan.addonId });
  const removed = win.querySelectorAll('.uninstall-entry.removed');
  const kept = win.querySelectorAll('.uninstall-entry.kept');
  assert.equal(removed.length, 1);
  assert.equal(kept.length, 1);
  assert.match(removed[0].textContent, /Dane aplikacji na tym węźle/, 'the entry says in words what and where');
  assert.match(removed[0].textContent, /3\.0 MB/);
  assert.match(kept[0].textContent, /Helper uprawnień/);
  // MAJOR 3: no path at all — the per-instance ones carry the instance id.
  assert.doesNotMatch(win.textContent, /0a1b2c3d|\/var\/lib|\/usr\/local/, 'no path, no instance id on screen');
  assert.match(win.querySelector('.alert.warn').textContent, /Backup/);
  assert.match(win.querySelector('.uninstall-total').textContent, /3\.0 MB/);
  assert.equal(win._titleEl.textContent, 'Odinstaluj dodatek', 'the title survives the subtitle set after the plan');
  win.remove();
});

test('the confirm button stays locked until the exact instance name is typed', async () => {
  stubTransport({ addonTeardownPlanRequest: plan, addonUninstallRequest: { ok: true } });
  const win = open();
  await flush();
  const btn = confirmButton(win);
  assert.ok(btn.hasAttribute('disabled'), 'locked before typing');

  const input = win.querySelector('#uninstall-retype');
  input.value = 'Tenta';
  input.dispatchEvent(new window.CustomEvent('input'));
  assert.ok(btn.hasAttribute('disabled'), 'partial name keeps it locked');

  // A confirm action while locked must not reach the backend.
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await flush();
  assert.ok(!calls.some((c) => c.kind === 'addonUninstallRequest'), 'no uninstall while locked');

  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  assert.ok(!btn.hasAttribute('disabled'), 'exact name unlocks');
  win.remove();
});

test('confirm sends the uninstall request, closes the window and runs onDone', async () => {
  let done = 0;
  stubTransport({ addonTeardownPlanRequest: plan, addonUninstallRequest: { ok: true } });
  const win = open(undefined, () => { done += 1; });
  await flush();
  const input = win.querySelector('#uninstall-retype');
  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await flush();
  await flush();
  const uninstall = calls.find((c) => c.kind === 'addonUninstallRequest');
  assert.ok(uninstall, 'uninstall sent');
  assert.deepEqual(uninstall.payload, { addonId: plan.addonId, acknowledgedNodes: [] });
  assert.equal(done, 1);
  // tf-window removes itself after its 240 ms closing animation.
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(document.querySelector('tf-window'), null, 'window closed');
});

test('a failed uninstall keeps the window open and unlocks the button again', async () => {
  stubTransport({
    addonTeardownPlanRequest: plan,
    addonUninstallRequest: () => Promise.reject(new Error('helper busy')),
  });
  const win = open();
  await flush();
  const input = win.querySelector('#uninstall-retype');
  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await flush();
  await flush();
  assert.ok(document.querySelector('tf-window'), 'window still open');
  assert.ok(!confirmButton(win).hasAttribute('disabled'), 'retry possible');
  win.remove();
});

test('a plan request failure shows the error instead of the retype field', async () => {
  stubTransport({ addonTeardownPlanRequest: () => Promise.reject(new Error('offline')) });
  const win = open();
  await flush();
  assert.match(win.querySelector('.alert.warn').textContent, /offline/);
  assert.equal(win.querySelector('#uninstall-retype'), null);
  assert.ok(confirmButton(win).hasAttribute('disabled'));
  win.remove();
});

// ----- MAJOR 22 (n18a): the uninstall reaches every node, each on its own row -----

const HELIOS = 'a'.repeat(64);
const ATLAS = 'b'.repeat(64);
const ORION = 'c'.repeat(64);
const TABBIE = 'd'.repeat(64);

const fleetPlan = {
  ...plan,
  privilege: 'helper',
  backupFile: 'app-backups/tentanas-helios-….json',
  nodes: [
    { nodeId: HELIOS, name: 'helios', local: true, online: true, status: 'ready', lastKnown: true, lastBlocks: [] },
    { nodeId: ATLAS, name: 'atlas', local: false, online: true, status: 'ready', lastKnown: true, lastBlocks: [] },
    { nodeId: ORION, name: '', local: false, online: false, status: 'ready', lastKnown: true, lastBlocks: [] },
    { nodeId: TABBIE, name: 'tabbie', local: false, online: true, status: 'unsupported' },
  ],
};
const atlasPlan = {
  ...plan,
  privilege: 'password',
  backupFile: 'app-backups/tentanas-atlas-….json',
  entries: [
    { path: '/etc/samba/tentanas.conf', kind: 'tentanas_smb_config', description: 'smb', removed: true, sizeBytes: 0, countVars: { n: 2 } },
  ],
  nodes: [],
};

const fleetCalls = [];
function stubFleet(answers) {
  fleetCalls.length = 0;
  const answer = (kind, payload, options) => {
    fleetCalls.push({ kind, payload, target: options?.targetNodeId || null });
    const f = answers[kind];
    if (!f) return Promise.reject(new Error(`unexpected request ${kind}`));
    return Promise.resolve().then(() => (typeof f === 'function' ? f(payload, options?.targetNodeId || null) : f));
  };
  ApiBinary.one = answer;
  ApiBinary.action = answer;
}

const rowOf = (win, id) => [...win.querySelectorAll('tr[data-node]')].find((tr) => tr.dataset.node === id);

test('a fleet uninstall lists every node by its real name, with its own scope, and never an id', async () => {
  stubFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : fleetPlan),
  });
  document.body.innerHTML = '';
  const win = openUninstallDialog({ addonId: plan.addonId, displayName: 'TentaNas' });
  await flush(); await flush(); await flush();
  assert.equal(win._titleEl.textContent, 'Odinstaluj TentaNas (cała flota)');
  const names = [...win.querySelectorAll('.uninstall-node-name')].map((el) => el.textContent);
  assert.deepEqual(names, ['helios', 'atlas', 'Węzeł bez nazwy', 'tabbie']);
  assert.doesNotMatch(win.querySelector('.uninstall-nodes').textContent, /aaaa|bbbb|cccc|dddd/, 'no node id on screen');
  // Each node's own plan, asked on that node.
  const forwarded = fleetCalls.filter((c) => c.kind === 'addonTeardownPlanRequest' && c.target);
  assert.deepEqual(forwarded.map((c) => c.target), [ATLAS], 'only the reachable node with work is asked');
  assert.match(rowOf(win, ATLAS).textContent, /2 share'y/);
  assert.match(rowOf(win, ORION).textContent, /węzeł nie odpowiada — odinstalowanie wykona się, gdy wróci/);
  assert.match(rowOf(win, TABBIE).textContent, /nic do zrobienia/);
  assert.equal(win.querySelector('tf-button[data-action="confirm"]').textContent, 'Odinstaluj na 3 węzłach');
  win.remove();
});

test('a node that refuses the uninstall is named and the confirm stays locked', async () => {
  const blocked = { ...atlasPlan, entries: [...atlasPlan.entries, { path: '/mnt/tentanas', kind: 'tentanas_elastic_arrays', description: 'x', removed: false, sizeBytes: 0, countVars: { n: 2 }, blocks: true }] };
  stubFleet({ addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? blocked : fleetPlan) });
  document.body.innerHTML = '';
  const win = openUninstallDialog({ addonId: plan.addonId, displayName: 'TentaNas' });
  await flush(); await flush(); await flush();
  const input = win.querySelector('#uninstall-retype');
  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  assert.ok(win.querySelector('tf-button[data-action="confirm"]').hasAttribute('disabled'), 'locked while a node refuses');
  assert.match(win.querySelector('[data-role="blocked"]').textContent, /Odinstalowanie zablokowane na węźle atlas: 2 macierze Elastic pod nadzorem TentaNas/);
  win.remove();
});

test('after the confirm each node shows its own progress, result and error, worded', async () => {
  const statuses = {
    [HELIOS]: { addonId: plan.addonId, state: 'done', phase: 'done', warnings: ['tentanas_pools_not_exported'] },
    [ATLAS]: { addonId: plan.addonId, state: 'failed', phase: 'tentanas_elastic_check', warnings: [] },
  };
  stubFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : fleetPlan),
    addonUninstallRequest: { ok: true },
    addonTeardownStatusRequest: (_p, target) => {
      if (target === ORION) throw Object.assign(new Error('node did not answer'), { code: 'NodeUnreachable' });
      return statuses[target || HELIOS];
    },
  });
  document.body.innerHTML = '';
  let done = 0;
  const win = openUninstallDialog({ addonId: plan.addonId, displayName: 'TentaNas', onDone: () => { done += 1; } });
  await flush(); await flush(); await flush();
  const before = rowOf(win, ATLAS).querySelector('[data-role="state-chip"]');
  const input = win.querySelector('#uninstall-retype');
  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  assert.equal(done, 1, 'the caller refreshes once this node is done');
  assert.ok(win.isConnected, 'the window stays open to follow the other nodes');
  const state = (id) => rowOf(win, id).querySelector('[data-role="state-chip"]').getAttribute('label');
  const detail = (id) => rowOf(win, id).querySelector('[data-role="state-detail"]').textContent;
  assert.equal(state(HELIOS), 'odinstalowano z uwagami');
  assert.equal(detail(HELIOS), 'pule nie zostały wyeksportowane — zostają zaimportowane');
  assert.equal(state(ATLAS), 'nie powiodło się');
  assert.equal(detail(ATLAS), 'na kroku: sprawdzanie macierzy Elastic pod nadzorem');
  assert.equal(state(ORION), 'węzeł nie odpowiada');
  assert.equal(state(TABBIE), 'nic do zrobienia');
  assert.equal(rowOf(win, ATLAS).querySelector('[data-role="state-chip"]'), before, 'the cell is patched in place, not rebuilt');
  const asked = fleetCalls.filter((c) => c.kind === 'addonTeardownStatusRequest').map((c) => c.target);
  assert.ok(asked.includes(ATLAS) && asked.includes(ORION) && asked.includes(null), 'every node is asked on itself');
  assert.ok(!asked.includes(TABBIE), 'a node with nothing to do is not asked');
  win.remove();
});

test('a single node that refuses the uninstall says why and keeps the confirm locked', async () => {
  const blockedPlan = { ...plan, entries: [...plan.entries, { path: '/mnt/tentanas', kind: 'tentanas_elastic_arrays', description: 'x', removed: false, sizeBytes: 0, countVars: { n: 1 }, blocks: true }] };
  stubTransport({ addonTeardownPlanRequest: blockedPlan, addonUninstallRequest: { ok: true } });
  const win = open();
  await flush();
  const input = win.querySelector('#uninstall-retype');
  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  assert.ok(confirmButton(win).hasAttribute('disabled'), 'the node would refuse: no confirm');
  assert.match(win.querySelector('[data-role="blocked"]').textContent, /Odinstalowanie zablokowane: 1 macierz Elastic pod nadzorem TentaNas/);
  assert.equal(win.querySelectorAll('.uninstall-entry').length, 2, 'the blocker is not listed as a path the wipe touches');
  win.remove();
});

// ----- wave 9b round 2 --------------------------------------------------------

async function openFleet(answers) {
  stubFleet(answers);
  document.body.innerHTML = '';
  const win = openUninstallDialog({ addonId: plan.addonId, displayName: 'TentaNas' });
  await flush(); await flush();
  const input = win.querySelector('#uninstall-retype');
  input.value = 'TentaNas';
  input.dispatchEvent(new window.CustomEvent('input'));
  return win;
}

const locked = (win) => win.querySelector('tf-button[data-action="confirm"]').hasAttribute('disabled');

test('MAJOR 1: the confirm stays locked while a node\'s plan is still on its way, even with the name typed', async () => {
  let release;
  const gate = new Promise((r) => { release = r; });
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? gate.then(() => atlasPlan) : fleetPlan),
  });
  assert.ok(locked(win), 'atlas has not answered: locked although the name is typed');
  assert.match(win.querySelector('[data-role="blocked"]').textContent, /czekam na zakres węzła atlas/);
  release();
  await flush(); await flush();
  assert.ok(!locked(win), 'every node answered and none refuses: unlocked');
  win.remove();
});

test('MAJOR 1: an offline node with nothing known, or a failed plan, keeps it locked — with a retry on its row', async () => {
  const unknown = { ...fleetPlan, nodes: fleetPlan.nodes.map((n) => (n.nodeId === ORION ? { ...n, lastKnown: false } : n)) };
  let atlasFails = true;
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => {
      if (target === ATLAS && atlasFails) throw new Error('forward failed');
      if (target === ORION) return atlasPlan;
      return target === ATLAS ? atlasPlan : unknown;
    },
  });
  await flush();
  assert.ok(locked(win));
  const orionRetry = rowOf(win, ORION).querySelector('[data-act="retry"]');
  const atlasRetry = rowOf(win, ATLAS).querySelector('[data-act="retry"]');
  assert.ok(!orionRetry.hasAttribute('hidden'), 'the unknown offline node offers a retry');
  assert.ok(!atlasRetry.hasAttribute('hidden'), 'so does the node whose plan failed');
  assert.match(rowOf(win, ORION).textContent, /zakres nie jest znany/);
  atlasFails = false;
  click(atlasRetry);
  click(orionRetry);
  await flush(); await flush(); await flush();
  assert.ok(atlasRetry.hasAttribute('hidden') && orionRetry.hasAttribute('hidden'), 'answered: no retry left');
  assert.ok(!locked(win), 'both answered and neither refuses');
  win.remove();
});

test('MAJOR 1: an offline node whose last published plan blocks keeps it locked and says why', async () => {
  const blocked = {
    ...fleetPlan,
    nodes: fleetPlan.nodes.map((n) => (n.nodeId === ORION
      ? { ...n, lastBlocks: [{ kind: 'tentanas_elastic_arrays', blocks: true, countVars: { n: 3 } }] }
      : n)),
  };
  const win = await openFleet({ addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : blocked) });
  await flush();
  assert.ok(locked(win));
  assert.match(win.querySelector('[data-role="blocked"]').textContent, /Odinstalowanie zablokowane na węźle Węzeł bez nazwy: 3 macierze Elastic/);
  win.remove();
});

test('MAJOR 2: a refusal on this node means nothing went out — the other rows say it never started', async () => {
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : fleetPlan),
    addonUninstallRequest: () => { throw Object.assign(new Error('refusal:teardown_blocked — 2 Elastic Array(s)'), { code: 'Conflict' }); },
    addonTeardownStatusRequest: (_p, target) => ({ addonId: plan.addonId, state: 'installed', phase: '', warnings: [], target }),
  });
  await flush();
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  const state = (id) => rowOf(win, id).querySelector('[data-role="state-chip"]').getAttribute('label');
  assert.equal(state(ATLAS), 'nie rozpoczęto');
  assert.match(rowOf(win, ATLAS).querySelector('[data-role="state-detail"]').textContent, /nic nie dotarło do tego węzła/);
  assert.equal(state(HELIOS), 'odmówiono');
  const asked = fleetCalls.filter((c) => c.kind === 'addonTeardownStatusRequest').map((c) => c.target);
  assert.ok(!asked.includes(ATLAS), 'no other node is followed after a refusal');
  win.remove();
});

test('MAJOR 7: each node shows its mode and its backup; a mode-B node\'s password arms it before the uninstall', async () => {
  const order = [];
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : fleetPlan),
    addonTeardownArmRequest: (p, target) => { order.push(['arm', target, p.sudoPassword]); return { addonId: plan.addonId, armedUntil: '' }; },
    addonUninstallRequest: () => { order.push(['uninstall']); return { ok: true }; },
    addonTeardownStatusRequest: { addonId: plan.addonId, state: 'done', phase: 'done', warnings: [] },
  });
  await flush();
  const mode = (id) => rowOf(win, id).querySelector('[data-role="mode-chip"]').getAttribute('label');
  assert.equal(mode(HELIOS), 'tryb A');
  assert.equal(mode(ATLAS), 'tryb B');
  assert.equal(rowOf(win, HELIOS).querySelector('[data-role="password"]'), null, 'mode A needs no password');
  assert.equal(rowOf(win, ATLAS).querySelector('[data-role="backup-file"]').textContent, 'app-backups/tentanas-atlas-….json');
  assert.equal(rowOf(win, ATLAS).querySelector('[data-role="backup-chip"]').getAttribute('label'), 'auto');
  rowOf(win, ATLAS).querySelector('[data-role="password"]').value = 'test-secret-not-real';
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  for (let i = 0; i < 8; i += 1) await flush();
  assert.deepEqual(order.slice(0, 2), [['arm', ATLAS, 'test-secret-not-real'], ['uninstall']], 'armed on atlas itself, before the uninstall');
  win.remove();
});

test('MAJOR 7: a rejected password removes nothing anywhere and is said on its row', async () => {
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : fleetPlan),
    addonTeardownArmRequest: () => { throw Object.assign(new Error('refusal:teardown_password_rejected'), { code: 'PolicyDenied' }); },
    addonUninstallRequest: { ok: true },
  });
  await flush();
  rowOf(win, ATLAS).querySelector('[data-role="password"]').value = 'wrong';
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  assert.ok(!fleetCalls.some((c) => c.kind === 'addonUninstallRequest'), 'nothing was uninstalled');
  assert.equal(rowOf(win, ATLAS).querySelector('[data-role="password-error"]').textContent, 'węzeł odrzucił hasło');
  assert.equal(rowOf(win, ATLAS).querySelector('[data-role="password"]').value, '', 'the password is not kept on screen');
  win.remove();
});

// ----- wave 9b round 3 --------------------------------------------------------

test('MAJOR A: a peer that would hold the uninstall back is passed by retyping its name, and the request carries it', async () => {
  const unknown = { ...fleetPlan, nodes: fleetPlan.nodes.map((n) => (n.nodeId === ORION ? { ...n, lastKnown: false } : n)) };
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : unknown),
    addonUninstallRequest: { ok: true },
    addonTeardownStatusRequest: { addonId: plan.addonId, state: 'done', phase: 'done', warnings: [] },
  });
  await flush();
  assert.ok(locked(win), 'orion never published');
  const ack = rowOf(win, ORION).querySelector('[data-role="ack"]');
  assert.ok(!ack.hasAttribute('hidden'), 'its row offers to proceed without it');
  assert.match(ack.textContent, /Wpisz LOST/, 'a node without a name is retyped as LOST — never its id');
  const input = ack.querySelector('[data-role="ack-input"]');
  input.value = 'orion';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.ok(locked(win), 'a wrong word does not count');
  input.value = 'LOST';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  assert.ok(!locked(win), 'acknowledged: unlocked');
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  const sent = fleetCalls.find((c) => c.kind === 'addonUninstallRequest');
  assert.deepEqual(sent.payload.acknowledgedNodes, [{ nodeId: ORION, confirmName: 'LOST' }]);
  win.remove();
});

test('MAJOR A: an unpaired node holds nothing back and is not counted', async () => {
  const withUnpaired = {
    ...fleetPlan,
    nodes: fleetPlan.nodes.map((n) => (n.nodeId === ORION ? { ...n, lastKnown: false, unpaired: true } : n)),
  };
  const win = await openFleet({ addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : withUnpaired) });
  await flush();
  assert.ok(!locked(win), 'the unpaired node never publishes and does not need to');
  assert.match(rowOf(win, ORION).textContent, /odłączony od floty/);
  assert.equal(win.querySelector('tf-button[data-action="confirm"]').textContent, 'Odinstaluj na 2 węzłach');
  win.remove();
});

async function armedThenStopped(extra, stop) {
  const win = await openFleet({
    addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? atlasPlan : { ...fleetPlan, privilege: 'password' }),
    addonTeardownArmRequest: { addonId: plan.addonId, armedUntil: '' },
    addonTeardownDisarmRequest: { addonId: plan.addonId, armedUntil: '' },
    ...extra,
  });
  await flush();
  rowOf(win, ATLAS).querySelector('[data-role="password"]').value = 'test-secret-not-real';
  await stop(win);
  for (let i = 0; i < 8; i += 1) await flush();
  return win;
}

const disarmed = () => fleetCalls.filter((c) => c.kind === 'addonTeardownDisarmRequest').map((c) => c.target);

test('MAJOR B: the passwords already handed out are taken back when another node refuses its own', async () => {
  let n = 0;
  const win = await armedThenStopped({
    addonTeardownArmRequest: () => { n += 1; if (n === 2) throw Object.assign(new Error('refusal:teardown_password_rejected'), { code: 'PolicyDenied' }); return {}; },
  }, async (w) => {
    rowOf(w, HELIOS).querySelector('[data-role="password"]').value = 'wrong';
    w.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  });
  assert.ok(!fleetCalls.some((c) => c.kind === 'addonUninstallRequest'));
  assert.equal(disarmed().length, 1, 'the node that accepted its password is disarmed');
  win.remove();
});

test('MAJOR B: a refusal before anything went out takes every password back', async () => {
  const win = await armedThenStopped({
    addonUninstallRequest: () => { throw Object.assign(new Error('refusal:teardown_peer_unknown — x'), { code: 'Conflict' }); },
    addonTeardownStatusRequest: { addonId: plan.addonId, state: 'installed', phase: '', warnings: [] },
  }, async (w) => {
    w.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  });
  assert.deepEqual(disarmed(), [ATLAS]);
  win.remove();
});

test('MAJOR C: a connected peer that refuses now is named with its blocker and cannot be acknowledged', async () => {
  const blocked = { ...atlasPlan, entries: [...atlasPlan.entries, { path: '', kind: 'tentanas_elastic_arrays', description: 'x', removed: false, sizeBytes: 0, countVars: { n: 2 }, blocks: true }] };
  const win = await openFleet({ addonTeardownPlanRequest: (_p, target) => (target === ATLAS ? blocked : fleetPlan) });
  await flush();
  assert.ok(locked(win));
  assert.ok(rowOf(win, ATLAS).querySelector('[data-role="ack"]').hasAttribute('hidden'), 'no acknowledgement for a node that answers');
  assert.match(rowOf(win, ATLAS).textContent, /2 macierze Elastic pod nadzorem TentaNas — najpierw je rozwiąż lub przenieś/);
  win.remove();
});

test('C4: a failure with no code may have come after replication — the passwords are left to the teardowns', async () => {
  const win = await armedThenStopped({
    addonUninstallRequest: () => { throw new Error('request timed out'); },
    addonTeardownStatusRequest: { addonId: plan.addonId, state: 'running', phase: 'tentanas_pools', warnings: [] },
  }, async (w) => {
    w.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  });
  assert.deepEqual(disarmed(), [], 'not certain nothing went out: no disarm');
  win.remove();
});

test('MAJOR B: a failure after the removal went out leaves the passwords to the teardowns', async () => {
  const win = await armedThenStopped({
    addonUninstallRequest: () => { throw Object.assign(new Error('uninstall: data dir'), { code: 'Internal' }); },
    addonTeardownStatusRequest: { addonId: plan.addonId, state: 'running', phase: 'tentanas_pools', warnings: [] },
  }, async (w) => {
    w.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  });
  assert.deepEqual(disarmed(), [], 'the removal replicated: each node consumes its own');
  win.remove();
});

test('MAJOR B: closing the dialog while a password is being handed out uninstalls nothing and takes it back', async () => {
  let release;
  const gate = new Promise((r) => { release = r; });
  const win = await armedThenStopped({
    addonTeardownArmRequest: () => gate.then(() => ({})),
    addonUninstallRequest: { ok: true },
  }, async (w) => {
    w.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
    await flush();
    w.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'cancel' }, cancelable: true }));
    release();
  });
  assert.ok(!fleetCalls.some((c) => c.kind === 'addonUninstallRequest'), 'nothing uninstalled');
  assert.deepEqual(disarmed(), [ATLAS], 'the password handed out is taken back');
  win.remove();
});

test('R1: a failed single-node uninstall never shows the node\'s raw text', async () => {
  stubTransport({
    addonTeardownPlanRequest: plan,
    addonUninstallRequest: () => Promise.reject(Object.assign(new Error('nie udalo sie usunac danych instancji "/var/lib/tentaflow/orgs/default/addons/tentanas-0a1b2c3d"'), { code: 'Internal' })),
  });
  const toasts = [];
  const append = window.Node.prototype.appendChild;
  window.Node.prototype.appendChild = function (child) {
    if (/toast-/.test(child?.className || '')) toasts.push(child.textContent);
    if (/toast-count/.test(child?.className || '')) toasts.push(this.textContent);
    return append.call(this, child);
  };
  try {
    const win = open();
    await flush();
    const input = win.querySelector('#uninstall-retype');
    input.value = 'TentaNas';
    input.dispatchEvent(new window.CustomEvent('input'));
    win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
    await flush(); await flush();
    win.remove();
  } finally {
    window.Node.prototype.appendChild = append;
  }
  const text = toasts.join('\n');
  assert.match(text, /szczegóły są w dzienniku węzła/);
  assert.doesNotMatch(text, /0a1b2c3d|\/var\/lib/);
});
