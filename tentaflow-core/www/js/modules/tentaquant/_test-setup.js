// =============================================================================
// File: modules/tentaquant/_test-setup.js
// Description: Shared bootstrap for the TentaQuant view tests: the happy-dom
// window with the `/js/` resolver hook and the locale files served from disk,
// so the helpers render real Polish strings instead of raw keys. Not a test
// itself (no `.test.js` suffix).
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
// happy-dom has no canvas backend: `getContext('2d')` answers null. The canvas
// components cope with that (they treat the canvas as a picture and skip it),
// but tf-code-editor measures its character width before drawing anything, so
// the views that embed an editor need a context that answers. Every drawing
// call is a no-op that returns the stub itself, which is what makes a chained
// call like `createLinearGradient(...).addColorStop(...)` harmless.
const CANVAS_CONTEXT = new Proxy({ canvas: { width: 0, height: 0 } }, {
  get(target, property) {
    if (property === 'measureText') return (text) => ({ width: String(text).length * 7 });
    if (property in target) return target[property];
    return () => CANVAS_CONTEXT;
  },
  set(target, property, value) { target[property] = value; return true; },
});
if (window.HTMLCanvasElement) {
  window.HTMLCanvasElement.prototype.getContext = () => CANVAS_CONTEXT;
}

// tf-* components adopt the shared stylesheet through `instanceof Document`.
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
// Locale files come from disk so the helpers return real strings; every other
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
process.on('unhandledRejection', () => {});

const { I18n } = await import('../../i18n.js');
// The default language is already 'en' (a no-op for setLanguage), so load
// Polish, the primary locale of the app.
await I18n.setLanguage('pl');

export { window, I18n };
