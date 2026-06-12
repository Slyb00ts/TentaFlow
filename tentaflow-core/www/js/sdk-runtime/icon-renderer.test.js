// =============================================================================
// Plik: sdk-runtime/icon-renderer.test.js
// Opis: Testy `renderIcon` + IconButton (chunk 3.3b-2).
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { renderIcon, ICON_NAMES } from './icon-renderer.js';
import { ICON_BUTTON_TAG } from './action-icon-button-renderer.js';

const results = [];
function test(name, fn) {
  try { fn(); results.push({ name, ok: true }); }
  catch (err) { results.push({ name, ok: false, err }); }
}
function assertEq(a, e, m) {
  const aj = JSON.stringify(a, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  const ej = JSON.stringify(e, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  if (aj !== ej) throw new Error(`${m || 'assertEq'}: expected ${ej}, got ${aj}`);
}
function assert(cond, m) { if (!cond) throw new Error(m || 'assert failed'); }
function assertThrows(fn, m) {
  let t = false; try { fn(); } catch { t = true; }
  if (!t) throw new Error(m || 'expected throw');
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}
function makeEngine(store, dispatcher) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: dispatcher || { emit() {} },
    locale: 'en-US',
  });
}
function comp(tag, fields, extra = {}) {
  return {
    tag, id: extra.id ?? 'c1', fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

// ============================================================================
// renderIcon — Named
// ============================================================================

test('renderIcon named creates svg with use href', () => {
  setup();
  const svg = renderIcon({ kind: 'named', name: 'save' }, 'icon');
  assertEq(svg.tagName.toLowerCase(), 'svg');
  assert(svg.classList.contains('tf-icon'));
  assert(svg.classList.contains('tf-icon--name-save'));
  assertEq(svg.getAttribute('aria-hidden'), 'true');
  const use = svg.querySelector('use');
  assert(use != null);
  assertEq(use.getAttribute('href'), '/img/icons.svg#icon-save');
});

test('renderIcon named with size + tone adds classes', () => {
  setup();
  const svg = renderIcon(
    { kind: 'named', name: 'star', size: 'lg', tone: 'success' },
    'icon'
  );
  assert(svg.classList.contains('tf-icon--size-lg'));
  assert(svg.classList.contains('tf-icon--tone-success'));
});

test('renderIcon named rejects unknown icon name', () => {
  setup();
  assertThrows(() => renderIcon({ kind: 'named', name: 'wzieta-z-kosmosu' }, 'x'));
});

test('renderIcon named rejects unknown size/tone', () => {
  setup();
  assertThrows(() => renderIcon({ kind: 'named', name: 'star', size: 'huge' }, 'x'));
  assertThrows(() => renderIcon({ kind: 'named', name: 'star', tone: 'evil' }, 'x'));
});

test('renderIcon named rejects unknown key', () => {
  setup();
  assertThrows(() => renderIcon({ kind: 'named', name: 'star', evil: true }, 'x'));
});

test('renderIcon ICON_NAMES has 142 entries (full spec whitelist)', () => {
  assertEq(ICON_NAMES.size, 142);
});

test('renderIcon named sprite href maps underscores to dashes', () => {
  const svg = renderIcon({ kind: 'named', name: 'arrow_down' }, 'icon');
  assertEq(svg.querySelector('use').getAttribute('href'), '/img/icons.svg#icon-arrow-down');
});

// ============================================================================
// renderIcon — Asset
// ============================================================================

test('renderIcon asset creates img with safe https src', () => {
  setup();
  const img = renderIcon(
    { kind: 'asset', ref: 'https://cdn.example.com/i.svg', alt: 'logo' },
    'icon'
  );
  assertEq(img.tagName, 'IMG');
  assertEq(img.getAttribute('src'), 'https://cdn.example.com/i.svg');
  assertEq(img.getAttribute('alt'), 'logo');
  assertEq(img.getAttribute('loading'), 'lazy');
});

test('renderIcon asset relative ref allowed', () => {
  setup();
  const img = renderIcon(
    { kind: 'asset', ref: '/addon/icons/foo.svg', size_px: 24 },
    'icon'
  );
  assertEq(img.getAttribute('src'), '/addon/icons/foo.svg');
  assertEq(img.getAttribute('width'), '24');
});

test('renderIcon asset alt absent uses aria-hidden + empty alt', () => {
  setup();
  const img = renderIcon({ kind: 'asset', ref: '/x.svg' }, 'icon');
  assertEq(img.getAttribute('alt'), '');
  assertEq(img.getAttribute('aria-hidden'), 'true');
});

test('renderIcon asset rejects javascript: scheme', () => {
  setup();
  assertThrows(() => renderIcon({ kind: 'asset', ref: 'javascript:alert(1)' }, 'x'));
});

test('renderIcon asset rejects whitespace-prefixed javascript:', () => {
  setup();
  assertThrows(() => renderIcon({ kind: 'asset', ref: ' javascript:alert(1)' }, 'x'));
});

test('renderIcon asset accepts data:image raster with base64 prefix', () => {
  setup();
  const img = renderIcon(
    { kind: 'asset', ref: 'data:image/png;base64,iVBORw==' },
    'icon'
  );
  assertEq(img.getAttribute('src'), 'data:image/png;base64,iVBORw==');
});

test('renderIcon asset rejects data:image/svg+xml (active content)', () => {
  setup();
  assertThrows(() =>
    renderIcon(
      { kind: 'asset', ref: 'data:image/svg+xml;base64,PHN2Zz4=' },
      'icon'
    )
  );
});

test('renderIcon asset rejects data:image without base64 (charset injection)', () => {
  setup();
  assertThrows(() =>
    renderIcon(
      { kind: 'asset', ref: 'data:image/png,not-base64-encoded' },
      'icon'
    )
  );
});

test('renderIcon asset rejects size_px > u16', () => {
  setup();
  assertThrows(() =>
    renderIcon({ kind: 'asset', ref: '/x.svg', size_px: 70000 }, 'icon')
  );
});

test('renderIcon asset accepts bigint size_px within u16 range', () => {
  setup();
  const img = renderIcon({ kind: 'asset', ref: '/x.svg', size_px: 24n }, 'icon');
  assertEq(img.getAttribute('width'), '24');
  assertEq(img.getAttribute('height'), '24');
});

test('renderIcon asset rejects bigint size_px out of u16 range', () => {
  setup();
  assertThrows(() =>
    renderIcon({ kind: 'asset', ref: '/x.svg', size_px: 0n }, 'icon')
  );
  assertThrows(() =>
    renderIcon({ kind: 'asset', ref: '/x.svg', size_px: 70000n }, 'icon')
  );
});

test('renderIcon rejects unknown kind', () => {
  setup();
  assertThrows(() => renderIcon({ kind: 'svg', name: 'star' }, 'x'));
});

// ============================================================================
// IconButton (0x0402)
// ============================================================================

const ICON_BUTTON_VALID = [
  [0, { kind: 'named', name: 'save' }],
  [1, 'primary'],
  [2, 'neutral'],
  [3, 'md'],
  [4, 'Save document'],
];

test('IconButton renders <tf-button> with icon attribute and aria-label', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(ICON_BUTTON_TAG, ICON_BUTTON_VALID));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-BUTTON');
  assertEq(el.getAttribute('aria-label'), 'Save document');
  assertEq(el.getAttribute('variant'), 'primary');
  // Named icon goes through the tf-button icon attribute (no child element).
  assertEq(el.getAttribute('icon'), 'save');
  // SDK size 'md' has no tf-button size mapping.
  assertEq(el.getAttribute('size'), null);
});

test('IconButton rejects empty aria_label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(ICON_BUTTON_TAG, [
        [0, { kind: 'named', name: 'save' }], [1, 'primary'], [2, 'neutral'], [3, 'md'], [4, ''],
      ])
    )
  );
});

test('IconButton rejects whitespace-only aria_label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(ICON_BUTTON_TAG, [
        [0, { kind: 'named', name: 'save' }], [1, 'primary'], [2, 'neutral'], [3, 'md'], [4, '   '],
      ])
    )
  );
});

test('IconButton rejects missing icon', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(ICON_BUTTON_TAG, [
        [1, 'primary'], [2, 'neutral'], [3, 'md'], [4, 'X'],
      ])
    )
  );
});

test('IconButton disabled BindRef reactive', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('d'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(ICON_BUTTON_TAG, [
      ...ICON_BUTTON_VALID,
      [5, { kind: 'bound', path: PATH('d') }],
    ])
  );
  assert(!el.hasAttribute('disabled'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('d'), op: { kind: 'set', value: true } }],
  });
  assert(el.hasAttribute('disabled'));
});

test('IconButton loading sets aria-busy and disabled', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('l'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(ICON_BUTTON_TAG, [
      ...ICON_BUTTON_VALID,
      [6, { kind: 'bound', path: PATH('l') }],
    ])
  );
  assertEq(el.getAttribute('aria-busy'), 'true');
  assert(el.hasAttribute('disabled'));
});

test('IconButton click goes through engine handler', () => {
  setup();
  const dispatched = [];
  const engine = makeEngine(undefined, {
    emit(evt) { dispatched.push(evt); }
  });
  const el = engine.render(
    comp(ICON_BUTTON_TAG, ICON_BUTTON_VALID, {
      handlers: [['click', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.click();
  assertEq(dispatched.length, 1);
  assertEq(dispatched[0].event_kind, 'click');
});

test('IconButton rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(ICON_BUTTON_TAG, [...ICON_BUTTON_VALID, [99, 'x']]))
  );
});

test('IconButton asset icon variant renders <img>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(ICON_BUTTON_TAG, [
      [0, { kind: 'asset', ref: '/x.svg', size_px: 16, alt: 'open' }],
      [1, 'ghost'], [2, 'info'], [3, 'sm'], [4, 'Open file'],
    ])
  );
  const img = el.querySelector('img.tf-icon--asset');
  assert(img != null);
  assertEq(img.getAttribute('alt'), 'open');
});

// ---- report ----
function reportResults() {
  let pass = 0, fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) { pass++; lines.push(`✓ ${r.name}`); }
    else { fail++; lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`); }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  return { pass, fail, text: lines.join('\n') };
}
if (typeof process !== 'undefined') {
  const r = reportResults();
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}
export { reportResults };
