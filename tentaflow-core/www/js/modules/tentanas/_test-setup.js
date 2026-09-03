// =============================================================================
// File: modules/tentanas/_test-setup.js
// Description: Shared bootstrap for the TentaNas phase-N2 module tests: the
// happy-dom window with the `/js/` resolver hook, locale files served from
// disk so dialogs render real strings, and a fake screen shell that records
// every request the view under test sends instead of going through the
// transport. Not a test itself (no `.test.js` suffix).
// =============================================================================

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import { register } from 'node:module';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';
import { readFileSync } from 'node:fs';

const here = fileURLToPath(import.meta.url);
export const WWW_ROOT = pathResolve(dirname(here), '..', '..', '..');
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
// Locale files come from disk so the views render real strings; every other
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
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { I18n } = await import('../../i18n.js');
// The default language is already 'en' (a no-op for setLanguage), so load
// Polish, the primary locale of the app.
await I18n.setLanguage('pl');

export { window, I18n };
export const flush = () => new Promise((r) => setTimeout(r, 0));

/**
 * A stand-in for the TentaNas screen shell (`tentanas.js`): `nas` answers
 * from `fixtures` (a value or a function of the payload) and records every
 * call; `withSudo` runs the action with a fixed password or answers `null`
 * when `sudo` is null (the prompt was cancelled); `later` timers are tracked
 * so `dispose()` stops any polling chain a view started.
 */
export function fakeScreen(fixtures = {}, { admin = true, sudo = 'hunter2' } = {}) {
  const calls = [];
  const timers = [];
  const screen = {
    calls,
    isAdmin: admin,
    disposed: false,
    openWindow: null,
    jobLogs: [],
    openedPools: [],
    openedDisks: [],
    switchedTabs: [],
    nodes: [{ nodeId: 'node-orion', nodeName: 'orion', isLocal: true }],
    nas(kind, payload) {
      calls.push({ kind, payload });
      if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
      const f = fixtures[kind];
      try {
        return Promise.resolve(typeof f === 'function' ? f(payload) : f);
      } catch (e) {
        return Promise.reject(e);
      }
    },
    withSudo: async (fn) => (sudo === null ? null : fn(sudo)),
    openJobLog(jobId, onFinish) { screen.jobLogs.push({ jobId, onFinish }); },
    jobRowHtml: (j) => `<div class="job-row" data-job="${j.jobId}">${j.kind} ${j.subject}</div>`,
    wireJobRows() {},
    later(fn, ms) { const t = setTimeout(fn, ms); timers.push(t); return t; },
    currentNode: () => ({ nodeId: 'node-orion', nodeName: 'orion', isLocal: true }),
    openPool(name, poolTab, dataset) { screen.openedPools.push({ name, poolTab, dataset }); },
    openDisk(diskId) { screen.openedDisks.push(diskId); },
    switchTab(tab) { screen.switchedTabs.push(tab); },
    setLocation() {},
    drawTab() {},
    clearTimers() { for (const t of timers) clearTimeout(t); timers.length = 0; },
    dispose() {
      screen.disposed = true;
      for (const t of timers) clearTimeout(t);
      timers.length = 0;
      document.body.innerHTML = '';
    },
  };
  return screen;
}

/** Sets a tf-input's value the way a keystroke would and notifies listeners. */
export function typeInto(input, value) {
  input.value = value;
  input.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
}

/** The tf-window footer's confirm action, as tf-window dispatches it. */
export function confirmWindow(win) {
  win.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
}

export const click = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true, cancelable: true }));

/** The title a tf-window shows: the component moves the attribute into its shadow header. */
export const windowTitle = (win) => win.shadowRoot.querySelector('.tf-window-title-text').textContent;
