// =============================================================================
// Plik: sdk-runtime/_dom-test-harness.js
// Opis: Wspólny setup `happy-dom` dla testów Node-side renderowania
// komponentów. Importowany przed jakimkolwiek modułem rendererowym żeby
// `globalThis.document`/`Element`/`HTMLElement`/`customElements` było
// dostępne. W browserze ten plik nie jest używany.
// =============================================================================

import { Window } from 'happy-dom';

const window = new Window({ url: 'http://localhost/sdk-tests/' });

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
