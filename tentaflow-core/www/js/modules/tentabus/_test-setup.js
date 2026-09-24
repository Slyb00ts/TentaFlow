// =============================================================================
// File: modules/tentabus/_test-setup.js
// Description: Shared bootstrap for the TentaBus module tests: the happy-dom
// window, locale files served from disk and Polish loaded, so formatters and
// views are checked against the strings the screen really prints. Not a test
// itself (no `.test.js` suffix).
// =============================================================================

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';

export const WWW_ROOT = pathResolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');

if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
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
process.on('unhandledRejection', () => {});

const { I18n } = await import('../../i18n.js');
await I18n.setLanguage('pl');
document.documentElement.lang = 'pl';

export { window, I18n };
