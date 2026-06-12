// =============================================================================
// Plik: sdk-runtime/form-search-scroll-renderer.test.js
// Opis: Tests for SearchBox (0x0307) and ScrollContainer (0x0112) renderers.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-searchbox.js';
import { StateStore } from './state-store.js';
import { ComponentRenderer } from './component-renderer.js';
import {
  SEARCHBOX_TAG,
  SCROLLCONTAINER_TAG,
} from './form-search-scroll-renderer.js';

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
function makeEngine(store) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: { emit() {} },
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
  document.body.innerHTML = '';
}

const A11Y = { a11y: { label: { kind: 'literal', value: 'Search' } } };

/// SearchBox FieldMap helper. Required: bind_path(0), placeholder(1), variant(3).
function searchFields({ path = PATH('q'), placeholder = { kind: 'literal', value: 'Find...' }, variant = 'default', ...rest } = {}) {
  const f = [[0, path], [1, placeholder], [3, variant]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}

/// ScrollContainer FieldMap helper. Required: orientation(0), virtualize(5).
function scrollFields({ orientation = 'vertical', virtualize = false, ...rest } = {}) {
  const f = [[0, orientation], [5, virtualize]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}

// ============================================================================
// SearchBox — render
// ============================================================================

test('SearchBox renders tf-searchbox with placeholder/variant/default debounce', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields(), A11Y));
  assertEq(el.tagName.toLowerCase(), 'tf-searchbox');
  assertEq(el.getAttribute('placeholder'), 'Find...');
  assertEq(el.getAttribute('debounce'), '300');
  assert(el.classList.contains('tf-searchbox--variant-default'));
  assertEq(el.getAttribute('aria-label'), 'Search');
});

test('SearchBox variant prominent sets variant class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields({ variant: 'prominent' }), A11Y));
  assert(el.classList.contains('tf-searchbox--variant-prominent'));
});

test('SearchBox debounce_ms number sets debounce attribute', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields({ 2: 150 }), A11Y));
  assertEq(el.getAttribute('debounce'), '150');
});

test('SearchBox debounce_ms accepts BigInt within u16', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields({ 2: 500n }), A11Y));
  assertEq(el.getAttribute('debounce'), '500');
});

test('SearchBox shortcut_hint sets data-shortcut-hint', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields({ 4: 'Ctrl+K' }), A11Y));
  assertEq(el.getAttribute('data-shortcut-hint'), 'Ctrl+K');
});

test('SearchBox value comes from store via bind_path', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'hello' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields(), A11Y));
  document.body.appendChild(el);
  assertEq(el.value, 'hello');
});

// ============================================================================
// SearchBox — reactive binds
// ============================================================================

test('SearchBox value reacts to store patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields(), A11Y));
  document.body.appendChild(el);
  assertEq(el.value, 'a');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'bb' } }],
  });
  assertEq(el.value, 'bb');
});

test('SearchBox bound placeholder reacts to store patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('ph'), value: 'Type...' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields({
    placeholder: { kind: 'bound', path: PATH('ph') },
  }), A11Y));
  assertEq(el.getAttribute('placeholder'), 'Type...');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('ph'), op: { kind: 'set', value: 'Search users...' } }],
  });
  assertEq(el.getAttribute('placeholder'), 'Search users...');
});

// ============================================================================
// SearchBox — events
// ============================================================================

test('SearchBox re-emits search as {query, action_id} with on_search_action_id', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields({ 5: 'do_search' }), A11Y));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('search', (e) => { if (e.__tfReemit) got = e.detail; });
  el.dispatchEvent(new CustomEvent('search', { bubbles: true, detail: { value: 'abc' } }));
  assertEq(got, { query: 'abc', action_id: 'do_search' });
});

test('SearchBox search without action id carries action_id null', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields(), A11Y));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('search', (e) => { if (e.__tfReemit) got = e.detail; });
  el.dispatchEvent(new CustomEvent('search', { bubbles: true, detail: { value: 'x' } }));
  assertEq(got, { query: 'x', action_id: null });
});

test('SearchBox raw search is blocked (stopImmediatePropagation)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields(), A11Y));
  document.body.appendChild(el);
  // Listener registered AFTER the renderer interceptor — must only see reemit.
  const seen = [];
  el.addEventListener('search', (e) => { seen.push(e.__tfReemit === true); });
  el.dispatchEvent(new CustomEvent('search', { bubbles: true, detail: { value: 'q' } }));
  assertEq(seen, [true]);
});

test('SearchBox re-emits input/change as {value, kind:tstr}', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SEARCHBOX_TAG, searchFields(), A11Y));
  document.body.appendChild(el);
  el.value = 'typed';
  let gotInput = null, gotChange = null;
  el.addEventListener('input', (e) => { if (e.__tfReemit) gotInput = e.detail; });
  el.addEventListener('change', (e) => { if (e.__tfReemit) gotChange = e.detail; });
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  assertEq(gotInput, { value: 'typed', kind: 'tstr' });
  assertEq(gotChange, { value: 'typed', kind: 'tstr' });
});

// ============================================================================
// SearchBox — validation
// ============================================================================

test('SearchBox unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ 9: 'oops' }), A11Y)));
});

test('SearchBox missing placeholder throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, [
    [0, PATH('q')], [3, 'default'],
  ], A11Y)));
});

test('SearchBox bind_path must be StatePath array', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ path: 'q' }), A11Y)));
});

test('SearchBox invalid variant throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ variant: 'huge' }), A11Y)));
});

test('SearchBox debounce_ms wrong type throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ 2: '300' }), A11Y)));
});

test('SearchBox debounce_ms BigInt out of u16 range throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ 2: 70000n }), A11Y)));
});

test('SearchBox shortcut_hint wrong type throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ 4: 5 }), A11Y)));
});

test('SearchBox on_search_action_id wrong type throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields({ 5: 7n }), A11Y)));
});

test('SearchBox without a11y.label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SEARCHBOX_TAG, searchFields())));
});

// ============================================================================
// ScrollContainer — render
// ============================================================================

test('ScrollContainer renders div with scroll classes + default full height', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields()));
  assertEq(el.tagName, 'DIV');
  assert(el.classList.contains('tf-scroll-container'));
  assert(el.classList.contains('tf-scroll-container--vertical'));
  assert(el.classList.contains('tf-scroll'));
  assertEq(el.style.height, '100%');
  assertEq(el.style.overflowY, 'auto');
  assertEq(el.style.overflowX, 'hidden');
});

test('ScrollContainer horizontal flips overflow axes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ orientation: 'horizontal' })));
  assert(el.classList.contains('tf-scroll-container--horizontal'));
  assertEq(el.style.overflowX, 'auto');
  assertEq(el.style.overflowY, 'hidden');
});

test('ScrollContainer both uses overflow auto', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ orientation: 'both' })));
  assertEq(el.style.overflow, 'auto');
});

test('ScrollContainer px height + percent max_height map to CSS', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({
    1: { kind: 'px', value: 240 },
    2: { kind: 'percent', value: 80 },
  })));
  assertEq(el.style.height, '240px');
  assertEq(el.style.maxHeight, '80%');
});

test('ScrollContainer virtualize=true adds modifier class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ virtualize: true })));
  assert(el.classList.contains('tf-scroll-container--virtualize'));
});

test('ScrollContainer renders children in order via child renderers', () => {
  setup();
  const engine = makeEngine();
  const childA = comp(SEARCHBOX_TAG, searchFields(), { ...A11Y, id: 'kid-a' });
  const childB = comp(SCROLLCONTAINER_TAG, scrollFields(), { id: 'kid-b' });
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ 3: [childA, childB] })));
  assertEq(el.children.length, 2);
  assertEq(el.children[0].getAttribute('data-component-id'), 'kid-a');
  assertEq(el.children[1].getAttribute('data-component-id'), 'kid-b');
});

test('ScrollContainer sticky_header_slot renders slot div before children', () => {
  setup();
  const engine = makeEngine();
  const child = comp(SCROLLCONTAINER_TAG, scrollFields(), { id: 'kid' });
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({
    3: [child], 4: 'hdr-slot',
  })));
  const header = el.children[0];
  assert(header.classList.contains('tf-scroll-container__header'));
  assertEq(header.getAttribute('data-slot-id'), 'hdr-slot');
  assertEq(header.style.position, 'sticky');
  assertEq(el.children[1].getAttribute('data-component-id'), 'kid');
});

test('ScrollContainer child carries reactive bind through renderChild', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'v1' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const child = comp(SEARCHBOX_TAG, searchFields(), A11Y);
  const el = engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ 3: [child] })));
  document.body.appendChild(el);
  const sb = el.querySelector('tf-searchbox');
  assertEq(sb.value, 'v1');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'v2' } }],
  });
  assertEq(sb.value, 'v2');
});

// ============================================================================
// ScrollContainer — validation
// ============================================================================

test('ScrollContainer unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ 7: true }))));
});

test('ScrollContainer invalid orientation throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ orientation: 'diagonal' }))));
});

test('ScrollContainer virtualize wrong type throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ virtualize: 1 }))));
});

test('ScrollContainer unit dimension with value throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({
    1: { kind: 'full', value: 1 },
  }))));
});

test('ScrollContainer unknown dimension kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({
    2: { kind: 'em', value: 2 },
  }))));
});

test('ScrollContainer children not an array throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ 3: 'kids' }))));
});

test('ScrollContainer sticky_header_slot wrong type throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SCROLLCONTAINER_TAG, scrollFields({ 4: 12 }))));
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
