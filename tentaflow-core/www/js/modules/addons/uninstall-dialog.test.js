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
  assert.match(removed[0].textContent, /tentanas-0a1b2c3d/);
  assert.match(removed[0].textContent, /3\.0 MB/);
  assert.match(kept[0].textContent, /tentanas-helper/);
  assert.match(win.querySelector('.alert.warn').textContent, /Backup/);
  assert.match(win.querySelector('.uninstall-total').textContent, /3\.0 MB/);
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
  assert.deepEqual(uninstall.payload, { addonId: plan.addonId });
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
