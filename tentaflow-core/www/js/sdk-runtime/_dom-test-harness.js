// =============================================================================
// Plik: sdk-runtime/_dom-test-harness.js
// Opis: Wspólny setup `happy-dom` dla testów Node-side renderowania
// komponentów. Importowany przed jakimkolwiek modułem rendererowym żeby
// `globalThis.document`/`Element`/`HTMLElement`/`customElements` było
// dostępne. W browserze ten plik nie jest używany.
// =============================================================================

import { Window } from 'happy-dom';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const window = new Window({ url: 'http://localhost/sdk-tests/' });

// Renderers reach codec.js through ApiBinary, and its module-level
// `codecReady` fetches `wasm_glue_bg.wasm` next to the glue as a `file:` URL
// under Node. undici refuses that scheme, and the unhandled rejection would
// kill the test process after every assertion already passed — so `file:`
// URLs are served from disk and the real codec initialises (~60 ms).
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const href = String(url);
  if (href.startsWith('file:')) {
    const body = readFileSync(fileURLToPath(href));
    const type = href.endsWith('.wasm') ? 'application/wasm' : 'application/octet-stream';
    return Promise.resolve(new Response(body, { headers: { 'Content-Type': type } }));
  }
  return hostFetch(url, init);
};

// Eksponujemy gołe globals, których oczekuje rendering engine. Listę
// trzymamy explicite — żaden „assign every Window key” głęboki kopirajt
// żeby nie mieszać z hostem Node.
const EXPORTED_GLOBALS = [
  'document',
  'window',
  'Element',
  'HTMLElement',
  'Node',
  'Event',
  'CustomEvent',
  'MouseEvent',
  'KeyboardEvent',
  'customElements',
  'getComputedStyle',
  'requestAnimationFrame',
  'cancelAnimationFrame',
];

// Only plain functions need `this` pinned to the window object. DOM classes
// (HTMLElement, Event, ...) MUST be exposed unbound — a bound constructor has
// no own `prototype`, which silently breaks custom elements that extend it
// (no `style`, attributeChangedCallback never fires).
const BOUND_FUNCTIONS = new Set([
  'getComputedStyle',
  'requestAnimationFrame',
  'cancelAnimationFrame',
]);

for (const key of EXPORTED_GLOBALS) {
  const value = key === 'window' ? window : window[key];
  if (value === undefined) continue;
  globalThis[key] = BOUND_FUNCTIONS.has(key) ? value.bind(window) : value;
}

export function resetHarnessDocument() {
  window.document.body.innerHTML = '';
  window.document.head.innerHTML = '';
}

export { window };
