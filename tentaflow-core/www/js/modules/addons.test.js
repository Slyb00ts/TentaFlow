// =============================================================================
// File: modules/addons.test.js
// Description: The addons screen's install path against a stubbed transport.
// Installing TentaNas must hand the admin straight to the TentaNas privilege
// channel setup step — the app finishes installing unable to run a single
// privileged command, and the install request deliberately carries no sudo
// password — while installing any other package must navigate nowhere.
// Runs under happy-dom with the `/js/` resolver hook.
// =============================================================================

import { window } from '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from 'node:module';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';
import { readFileSync } from 'node:fs';

const here = fileURLToPath(import.meta.url);
const WWW_ROOT = pathResolve(dirname(here), '..', '..');
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
globalThis.fetch = (url) => {
  const m = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (m) {
    const text = readFileSync(pathResolve(WWW_ROOT, 'i18n', `${m[1]}.json`), 'utf8');
    return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)), text: () => Promise.resolve(text) });
  }
  return Promise.resolve({ ok: true, text: () => Promise.resolve('') });
};
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}
// codec.js starts a WASM fetch at import time that rejects under Node; the
// screen under test never reaches the codec because the transport is stubbed.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { I18n } = await import('../i18n.js');
await I18n.setLanguage('pl');

// The modal the install flow opens is built from these; addons.js imports no
// components of its own, so the test registers the ones it renders.
await import('../components/tf-window.js');
await import('../components/tf-button.js');
await import('../components/tf-input.js');
await import('../components/tf-select.js');
await import('../components/tf-chip.js');
await import('../components/tf-searchbox.js');
await import('../components/tf-toggle.js');

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { Router } = await import('../router.js');
const { default: AddonsScreen } = await import('./addons.js');

const flush = () => new Promise((r) => setTimeout(r, 0));

const calls = [];
function stubTransport(fixtures) {
  const answer = (kind, payload) => {
    calls.push({ kind, payload });
    if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
    const f = fixtures[kind];
    try {
      return Promise.resolve(typeof f === 'function' ? f(payload) : f);
    } catch (e) {
      return Promise.reject(e);
    }
  };
  ApiBinary.one = (kind, payload) => answer(kind, payload);
  ApiBinary.action = (kind, payload) => answer(kind, payload);
  ApiBinary.list = (kind, options = {}) => answer(kind, options.payload)
    .then((body) => body[options.arrayKey] ?? []);
}

// Every navigation the screen asks for, instead of mounting a real screen.
const navigations = [];
Router.navigate = async (id, params = null) => { navigations.push({ id, params }); return true; };

const pkg = (packageId, name) => ({
  packageId, name, latestVersion: '1.0.0', versions: ['1.0.0'],
  installedInstances: 0, singleton: false, source: 'bundled', connectionParams: [],
});

const fixtures = (over = {}) => ({
  authMeRequest: { role: 'admin' },
  addonsListRequest: { addons: [] },
  addonCatalogListRequest: { packages: [pkg('tentanas', 'TentaNas'), pkg('tentaquant', 'TentaQuant')] },
  addonInstanceInstallRequest: { ok: true },
  ...over,
});

/// Installs `packageId` the way the catalog does: the route opens the install
/// modal, the admin names the instance and confirms.
async function install(packageId, over = {}) {
  calls.length = 0;
  navigations.length = 0;
  stubTransport(fixtures(over));
  document.body.innerHTML = '<div id="main"></div>';
  const host = document.getElementById('main');
  host.innerHTML = AddonsScreen.render();
  await AddonsScreen.mount({ install: packageId });
  await flush();

  const win = [...document.querySelectorAll('tf-window')].at(-1);
  assert.ok(win, `the install modal opened for ${packageId}`);
  const name = win.querySelector('#inst-name');
  name.value = `${packageId}-1`;
  name.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  for (let i = 0; i < 6; i += 1) await flush();
  return win;
}

test('installing TentaNas hands the admin straight to the privilege channel setup step', async () => {
  await install('tentanas');

  const sent = calls.find((c) => c.kind === 'addonInstanceInstallRequest');
  assert.ok(sent, 'the install actually ran');
  assert.equal(sent.payload.packageId, 'tentanas');
  // The install request carries no secret — the owner rejected putting a sudo
  // password into it, so the elevation step has to come after, not inside.
  assert.ok(!('sudoPassword' in sent.payload), 'no sudo password rides along with the install');
  assert.deepEqual(navigations, [{ id: 'tentanas', params: { setup: '1' } }],
    'the admin is taken to the setup step immediately, not left on the addons list');
  AddonsScreen.unmount();
});

test('installing any other package navigates nowhere', async () => {
  await install('tentaquant');

  assert.ok(calls.some((c) => c.kind === 'addonInstanceInstallRequest'), 'the install ran');
  assert.deepEqual(navigations, [], 'only TentaNas needs a privilege channel chosen after install');
  AddonsScreen.unmount();
});
