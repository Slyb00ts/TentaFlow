// =============================================================================
// File: modules/addons/disable-dialog.test.js
// Description: The disable confirmation (n18d) against a stubbed transport,
// wave 10: a fleet-wide disable lists every node by name with its mode chip
// and ITS OWN consequences (each asked of that node), words the loading,
// offline and failed rows, never shows an id, and patches rows in place;
// "Wyłącz i zatrzymaj udostępnianie…" sends one four-eyes request for THIS
// node and shows what the approver will read. Runs under happy-dom with the
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
const { confirmDisable } = await import('./disable-dialog.js');
await import('../../protocol/codec.js').then((m) => m.codecReady).catch(() => {});
await I18n.setLanguage('pl');

const flush = () => new Promise((r) => setTimeout(r, 0));
const settle = async (n = 6) => { for (let i = 0; i < n; i += 1) await flush(); };
const click = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true, composed: true }));
const act = (win, action) => win.dispatchEvent(new window.CustomEvent('action', { detail: { action }, cancelable: true }));

const LOCAL = 'a'.repeat(64);
const PEER = 'b'.repeat(64);
const GONE = 'c'.repeat(64);
const BROKEN = 'd'.repeat(64);
const ADDON = 'tentanas-1a2b3c4d';

const calls = [];
function stubTransport(answer) {
  const handler = (kind, payload, opts) => {
    calls.push({ kind, payload, target: opts?.targetNodeId || null });
    return answer(kind, payload, opts?.targetNodeId || null);
  };
  ApiBinary.one = handler;
  ApiBinary.action = handler;
}

const node = (nodeId, name, over = {}) => ({ nodeId, name, local: false, online: true, status: 'ready', unpaired: false, ...over });

const localPreview = {
  addonId: ADDON, displayName: 'TentaNas', nodeName: 'helios', backgroundOnDisable: true, privilege: 'helper',
  consequences: [
    { kind: 'tentanas_api_closed', effect: 'stops', countVars: {}, names: [] },
    { kind: 'tentanas_smb_shares_continue', effect: 'continues', countVars: { n: 2 }, names: [] },
    { kind: 'tentanas_iscsi_targets_continue', effect: 'continues', countVars: { n: 1 }, names: [] },
  ],
  nodes: [
    node(LOCAL, 'helios', { local: true }),
    node(PEER, 'atlas'),
    node(GONE, 'kronos', { online: false }),
    node(BROKEN, ''),
  ],
};
const peerPreview = {
  addonId: ADDON, displayName: 'TentaNas', nodeName: 'atlas', backgroundOnDisable: true, privilege: 'password',
  consequences: [
    { kind: 'tentanas_api_closed', effect: 'stops', countVars: {}, names: [] },
    { kind: 'tentanas_nfs_shares_continue', effect: 'continues', countVars: { n: 3 }, names: [] },
    { kind: 'tentanas_schedules_stop', effect: 'stops', countVars: { n: 1 }, names: [] },
  ],
};

function fleet({ peer = () => Promise.resolve(peerPreview), stop } = {}) {
  stubTransport((kind, payload, target) => {
    if (kind === 'addonDisablePreviewRequest' && !target) return Promise.resolve(localPreview);
    if (kind === 'addonDisablePreviewRequest' && target === PEER) return peer();
    if (kind === 'addonDisablePreviewRequest' && target === BROKEN) return Promise.reject(Object.assign(new Error('internal'), { code: 'Internal' }));
    if (kind === 'addonDisablePreviewRequest' && target === GONE) return Promise.reject(Object.assign(new Error('gone'), { code: 'NodeUnreachable' }));
    if (kind === 'tentaNasSharingStopRequest' && stop) return stop(payload, target);
    return Promise.reject(new Error(`unexpected request ${kind}`));
  });
}

async function open(packageId = 'tentanas') {
  calls.length = 0;
  // Only the windows go: the toast container stays attached (a toast is how
  // a refusal is said).
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const outcome = confirmDisable({ addonId: ADDON, displayName: 'TentaNas', packageId });
  await settle();
  const win = [...document.querySelectorAll('tf-window')].at(-1);
  return { win, outcome };
}

const rowOf = (win, id) => win.querySelector(`tr[data-node="${id}"]`);
const rowText = (win, id) => rowOf(win, id).textContent.replace(/\s+/g, ' ');

test('a fleet-wide disable lists every node by name, with its mode and its own consequences', async () => {
  fleet();
  const { win, outcome } = await open();
  const rows = [...win.querySelectorAll('tr[data-node]')];
  assert.equal(rows.length, 4, 'one row per node');
  // The local row: its own preview, mode A.
  assert.match(rowText(win, LOCAL), /helios/);
  assert.match(rowText(win, LOCAL), /2 udziały SMB nadal serwują dane/);
  assert.equal(rowOf(win, LOCAL).querySelector('[data-role="mode-chip"]').getAttribute('label'), 'tryb A');
  // The peer's row: asked OF THE PEER (forwarded), its consequences, mode B.
  const asked = calls.filter((c) => c.kind === 'addonDisablePreviewRequest' && c.target === PEER);
  assert.equal(asked.length, 1, 'the peer is asked for its own consequences');
  assert.deepEqual(asked[0].payload, { addonId: ADDON });
  assert.match(rowText(win, PEER), /atlas/);
  assert.match(rowText(win, PEER), /3 udziały NFS nadal serwują dane/);
  assert.match(rowText(win, PEER), /1 harmonogram .* się zatrzyma/);
  assert.doesNotMatch(rowText(win, PEER), /SMB/, 'the local node\'s shares are not the peer\'s');
  assert.equal(rowOf(win, PEER).querySelector('[data-role="mode-chip"]').getAttribute('label'), 'tryb B');
  // Offline and failed rows, worded; a node without a name is named in words.
  assert.match(rowText(win, GONE), /węzeł nie odpowiada — jego skutki nie są znane/);
  assert.equal(rowOf(win, GONE).querySelector('[data-role="node-chip"]').getAttribute('label'), 'offline');
  assert.ok(!rowOf(win, GONE).querySelector('[data-act="retry"]').hasAttribute('hidden'), 'an offline row offers a retry');
  assert.match(rowText(win, BROKEN), /Węzeł bez nazwy/);
  assert.match(rowText(win, BROKEN), /nie udało się pobrać skutków z tego węzła/);
  assert.ok(!calls.some((c) => c.kind === 'addonDisablePreviewRequest' && c.target === GONE), 'a node the roster calls offline is not asked until a retry');
  // Never an id.
  assert.doesNotMatch(win.textContent, /[0-9a-f]{16}|tentanas-1a2b|tentanas_|addon_disable\./, 'no id, no code, no raw key');
  act(win, 'cancel');
  assert.equal(await outcome, false);
});

test('a row still loading says so, and is patched in place when its node answers', async () => {
  let answer;
  fleet({ peer: () => new Promise((r) => { answer = r; }) });
  const { win, outcome } = await open();
  assert.match(rowText(win, PEER), /Wczytywanie|Ładowanie/);
  const chip = rowOf(win, PEER).querySelector('[data-role="node-chip"]');
  const localItems = [...rowOf(win, LOCAL).querySelectorAll('.disable-consequence')];
  answer(peerPreview);
  await settle();
  assert.equal(rowOf(win, PEER).querySelector('[data-role="node-chip"]'), chip, 'the same chip element');
  assert.deepEqual([...rowOf(win, LOCAL).querySelectorAll('.disable-consequence')], localItems, 'the other rows are untouched');
  assert.match(rowText(win, PEER), /3 udziały NFS/);
  assert.equal(rowOf(win, PEER).querySelector('[data-role="note"]').textContent, '', 'the loading note is gone');
  act(win, 'cancel');
  await outcome;
});

test('a retry asks the node again, even one the roster called offline', async () => {
  fleet();
  const { win, outcome } = await open();
  stubTransport((kind, _p, target) => (target === GONE ? Promise.resolve({ ...peerPreview, nodeName: 'kronos' }) : Promise.reject(new Error('x'))));
  click(rowOf(win, GONE).querySelector('[data-act="retry"]'));
  await settle();
  assert.ok(calls.some((c) => c.kind === 'addonDisablePreviewRequest' && c.target === GONE));
  assert.match(rowText(win, GONE), /3 udziały NFS/);
  assert.ok(rowOf(win, GONE).querySelector('[data-act="retry"]').hasAttribute('hidden'));
  act(win, 'cancel');
  await outcome;
});

test('"Wyłącz i zatrzymaj udostępnianie…" parks one request for THIS node and shows what the approver reads', async () => {
  const approval = {
    requestId: 'r-9', operation: 'sharing_stop', subject: 'helios', status: 'pending',
    detail: 'stops sharing on helios: …',
    detailReasons: [{ code: 'sharing_stop', params: { node: 'helios', shares: 'media, projekty', targets: 'vm-store', other_shares: '1', other_targets: '0', smb: '2', nfs: '1', iscsi: '1', nvmet: '0' } }],
    expiresAt: new Date(Date.now() + 3600_000).toISOString(),
  };
  fleet({ stop: () => Promise.resolve({ approval }) });
  const { win, outcome } = await open();
  // n18d's two actions.
  assert.equal(win.querySelector('[data-action="stop"]').textContent.trim(), 'Wyłącz i zatrzymaj udostępnianie…');
  assert.equal(win.querySelector('[data-action="confirm"]').textContent.trim(), 'Wyłącz aplikację (usługi działają dalej)');
  assert.match(win.querySelector('[data-role="stop-hint"]').textContent, /zatrzymać udostępnianie danych na tym węźle/);
  act(win, 'stop');
  await settle();
  const panel = win.querySelector('[data-role="stop-panel"]');
  assert.ok(!panel.hasAttribute('hidden'));
  assert.match(panel.textContent, /Zatrzymaj udostępnianie na węźle helios i wyłącz TentaNas/);
  assert.match(panel.textContent, /2 udziały SMB przestaną serwować dane/);
  assert.match(panel.textContent, /1 target iSCSI zniknie z jądra/);
  assert.match(panel.textContent, /drugiego administratora platformy/);
  assert.ok(win.querySelector('.disable-nodes').hasAttribute('hidden'), 'the table steps aside');
  act(win, 'stop-send');
  await settle();
  const sent = calls.filter((c) => c.kind === 'tentaNasSharingStopRequest');
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].payload, {});
  assert.equal(sent[0].target, null, 'for this node: not forwarded anywhere');
  const result = win.querySelector('[data-role="stop-result"]').textContent;
  assert.match(result, /Wysłano do zatwierdzenia/);
  assert.match(result, /na węźle helios — udziały: media, projekty, 1 udział innych organizacji; targety: vm-store/);
  assert.ok(win.querySelector('[data-action="stop-send"]').hasAttribute('hidden'), 'sent once');
  assert.ok(!calls.some((c) => c.kind === 'addonToggleRequest'), 'nothing is disabled before the approval');
  act(win, 'cancel');
  assert.equal(await outcome, false, 'the switch goes back on: TentaNas stays enabled until approved');
});

test('a refused stop is worded, and nothing is parked', async () => {
  fleet({ stop: () => Promise.reject(new Error('refusal:sharing_stop_no_second_admin')) });
  const { win, outcome } = await open();
  act(win, 'stop');
  act(win, 'stop-send');
  await settle();
  assert.match(document.body.textContent, /Nie ma drugiego administratora platformy w tej organizacji/);
  assert.ok(win.querySelector('[data-role="stop-result"]').hasAttribute('hidden'));
  assert.ok(!win.querySelector('[data-action="stop-send"]').hasAttribute('hidden'), 'it can be sent again');
  act(win, 'stop-back');
  await settle();
  assert.ok(win.querySelector('[data-role="stop-panel"]').hasAttribute('hidden'));
  assert.ok(!win.querySelector('.disable-nodes').hasAttribute('hidden'));
  act(win, 'cancel');
  await outcome;
});

test('an app that does not stop sharing offers only the disable, and a node list of one keeps the old layout', async () => {
  stubTransport((kind) => (kind === 'addonDisablePreviewRequest'
    ? Promise.resolve({ ...localPreview, nodes: [node(LOCAL, 'helios', { local: true })] })
    : Promise.reject(new Error(kind))));
  const { win, outcome } = await open('tentabus');
  assert.equal(win.querySelector('[data-action="stop"]'), null);
  assert.equal(win.querySelector('[data-role="stop-hint"]'), null);
  assert.equal(win.querySelector('[data-action="confirm"]').textContent.trim(), 'Wyłącz aplikację');
  assert.equal(win.querySelector('.disable-nodes'), null);
  assert.match(win.textContent, /Skutki na węźle helios:/);
  act(win, 'confirm');
  assert.equal(await outcome, true);
});

test('a stop waiting in another organisation is said as such, never named', async () => {
  fleet({ stop: () => Promise.reject(new Error('refusal:sharing_stop_pending_elsewhere')) });
  const { win, outcome } = await open();
  act(win, 'stop');
  act(win, 'stop-send');
  await settle();
  assert.match(document.body.textContent, /zgłoszone w innej organizacji/);
  act(win, 'cancel');
  await outcome;
});
