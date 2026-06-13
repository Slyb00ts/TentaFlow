// =============================================================================
// File: sdk-runtime/form-range-numeric-renderer.test.js
// Description: Tests for Slider/RangeSlider/SliderRow/NumericInput/
// CurrencyInput. Slider (0x030F) renders through the <tf-slider> web component
// (imported for happy-dom upgrade); the remaining controls use native inputs.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-slider.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  SLIDER_TAG, RANGE_SLIDER_TAG, SLIDER_ROW_TAG,
  NUMERIC_INPUT_TAG, CURRENCY_INPUT_TAG,
} from './form-range-numeric-renderer.js';

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

function makeStore() { return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n }); }
function makeEngine(store) {
  return new ComponentRenderer({ store: store || makeStore(), eventDispatcher: { emit() {} }, locale: 'en-US' });
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
// tf-slider builds its inner native input in connectedCallback, so tests
// that touch it mount the rendered tree first.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

// ============================================================================
// Slider
// ============================================================================

test('Slider renders <tf-slider> with min/max/step attrs + inner range input', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 5],
    [4, { kind: 'literal', value: 'Vol' }],
    [5, true], [8, 'primary'],
  ])));
  assert(el.classList.contains('tf-slider-control--tone-primary'));
  assertEq(el.querySelector('.tf-slider__label').textContent, 'Vol');
  const slider = el.querySelector('tf-slider');
  assertEq(slider.getAttribute('min'), '0');
  assertEq(slider.getAttribute('max'), '100');
  assertEq(slider.getAttribute('step'), '5');
  const input = slider.querySelector('input');
  assertEq(input.getAttribute('type'), 'range');
  assertEq(String(input.min), '0');
  assertEq(String(input.max), '100');
  assertEq(String(input.step), '5');
});

test('Slider show_value renderuje badge', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 42 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, true], [8, 'primary'],
  ]));
  assertEq(el.querySelector('.tf-slider__value').textContent, '42');
});

test('Slider input event re-emits { value, kind: f64 } on wrapper', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false], [8, 'primary'],
  ])));
  const events = [];
  el.addEventListener('input', (e) => events.push(e.detail));
  el.addEventListener('change', (e) => events.push(['change', e.detail]));
  const input = el.querySelector('tf-slider input');
  input.value = '37';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: false }));
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(events, [
    { value: 37, kind: 'f64' },
    ['change', { value: 37, kind: 'f64' }],
  ]);
});

test('Slider bubbled native events do not duplicate SDK input/change', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false], [8, 'primary'],
  ])));
  const events = [];
  el.addEventListener('input', (e) => events.push(['input', e.detail]));
  el.addEventListener('change', (e) => events.push(['change', e.detail]));
  // Real browsers bubble the inner range input's native events to the
  // tf-slider host alongside the component CustomEvent — exactly one SDK
  // event must come out of each, and the raw string-detail event must never
  // reach the wrapper.
  const input = el.querySelector('tf-slider input');
  input.value = '37';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(events, [
    ['input', { value: 37, kind: 'f64' }],
    ['change', { value: 37, kind: 'f64' }],
  ]);
});

test('Slider min >= max throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 100], [2, 0], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false], [8, 'primary'],
  ])));
});

test('Slider step <= 0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 0],
    [4, { kind: 'literal', value: 'V' }],
    [5, false], [8, 'primary'],
  ])));
});

test('Slider marks are validated (valid accepted, malformed throw)', () => {
  setup();
  const engine = makeEngine();
  const m = [[0, 25], [1, { kind: 'literal', value: 'Low' }]];
  const m2 = [[0, 75]];
  // Valid marks render without throwing.
  engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false],
    [7, [m, m2]], [8, 'primary'],
  ]));
  // Mark without required value (key 0) throws.
  assertThrows(() => engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false],
    [7, [[[1, { kind: 'literal', value: 'NoValue' }]]]], [8, 'primary'],
  ])));
  // Mark with unknown key throws.
  assertThrows(() => engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false],
    [7, [[[0, 25], [9, 'rogue']]]], [8, 'primary'],
  ])));
});

test('Slider invalid format.kind throws nawet gdy show_value=false', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false],
    [6, { kind: 'nope' }],
    [8, 'primary'],
  ])));
});

test('Slider unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SLIDER_TAG, [
    [0, PATH('v')], [1, 0], [2, 100], [3, 1],
    [4, { kind: 'literal', value: 'V' }],
    [5, false], [8, 'primary'], [99, 'x'],
  ])));
});

// ============================================================================
// RangeSlider
// ============================================================================

test('RangeSlider renderuje 2x range inputs', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(RANGE_SLIDER_TAG, [
    [0, PATH('lo')], [1, PATH('hi')],
    [2, 0], [3, 100], [4, 1],
    [5, { kind: 'literal', value: 'R' }],
    [6, false], [9, 'primary'], [10, 5],
  ]));
  assertEq(el.querySelectorAll('input[type=range]').length, 2);
});

test('RangeSlider min_separation enforcement revert', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lo'), value: 10 }, { path: PATH('hi'), value: 50 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(RANGE_SLIDER_TAG, [
    [0, PATH('lo')], [1, PATH('hi')],
    [2, 0], [3, 100], [4, 1],
    [5, { kind: 'literal', value: 'R' }],
    [6, false], [9, 'primary'], [10, 20],  // separation=20
  ]));
  const [minI, maxI] = el.querySelectorAll('input');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  minI.value = '48';  // hi=50, separation by 2 < 20 → revert
  minI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(minI.value, '10');
  assertEq(got, null);
});

test('RangeSlider valid range emit change.range', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lo'), value: 10 }, { path: PATH('hi'), value: 50 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(RANGE_SLIDER_TAG, [
    [0, PATH('lo')], [1, PATH('hi')],
    [2, 0], [3, 100], [4, 1],
    [5, { kind: 'literal', value: 'R' }],
    [6, false], [9, 'primary'], [10, 5],
  ]));
  const [minI, maxI] = el.querySelectorAll('input');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  maxI.value = '60';
  maxI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: { min: 10, max: 60 }, kind: 'range', changed: 'max' });
});

test('RangeSlider bubbled native events do not leak raw to wrapper', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lo'), value: 10 }, { path: PATH('hi'), value: 50 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(RANGE_SLIDER_TAG, [
    [0, PATH('lo')], [1, PATH('hi')],
    [2, 0], [3, 100], [4, 1],
    [5, { kind: 'literal', value: 'R' }],
    [6, false], [9, 'primary'], [10, 5],
  ]));
  const [minI, maxI] = el.querySelectorAll('input');
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  el.addEventListener('input', (e) => events.push(['raw-input', e.detail]));
  // Bubbled native change → exactly one validated SDK range event.
  maxI.value = '60';
  maxI.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  // RangeSlider emits no SDK input — a bubbled native input must vanish.
  minI.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  assertEq(events, [{ value: { min: 10, max: 60 }, kind: 'range', changed: 'max' }]);
});

test('RangeSlider min_separation > (max-min) throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RANGE_SLIDER_TAG, [
    [0, PATH('lo')], [1, PATH('hi')],
    [2, 0], [3, 10], [4, 1],
    [5, { kind: 'literal', value: 'R' }],
    [6, false], [9, 'primary'], [10, 20],
  ])));
});

// ============================================================================
// SliderRow
// ============================================================================

test('SliderRow zawsze renderuje value badge + layout class', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 5 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SLIDER_ROW_TAG, [
    [0, PATH('v')], [1, 0], [2, 10], [3, 1],
    [4, { kind: 'literal', value: 'Bright' }],
    [7, 'primary'], [8, 'horizontal'],
  ]));
  assert(el.classList.contains('tf-slider-row--layout-horizontal'));
  assertEq(el.querySelector('.tf-slider-row__value').textContent, '5');
});

test('SliderRow bubbled native events do not leak raw to wrapper', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('v'), value: 5 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SLIDER_ROW_TAG, [
    [0, PATH('v')], [1, 0], [2, 10], [3, 1],
    [4, { kind: 'literal', value: 'Bright' }],
    [7, 'primary'], [8, 'horizontal'],
  ]));
  const events = [];
  el.addEventListener('input', (e) => events.push(['input', e.detail]));
  el.addEventListener('change', (e) => events.push(['change', e.detail]));
  const input = el.querySelector('input');
  input.value = '7';
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(events, [
    ['input', { value: 7, kind: 'f64' }],
    ['change', { value: 7, kind: 'f64' }],
  ]);
});

test('SliderRow bez label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SLIDER_ROW_TAG, [
    [0, PATH('v')], [1, 0], [2, 10], [3, 1],
    [7, 'primary'], [8, 'horizontal'],
  ])));
});

test('SliderRow invalid layout throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SLIDER_ROW_TAG, [
    [0, PATH('v')], [1, 0], [2, 10], [3, 1],
    [4, { kind: 'literal', value: 'L' }],
    [7, 'primary'], [8, 'vertical'],
  ])));
});

// ============================================================================
// NumericInput
// ============================================================================

test('NumericInput renderuje <input type=number> + min/max/step/inputmode', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [1, 0], [2, 100], [3, 0.5], [4, 2],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ]));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('type'), 'number');
  assertEq(input.getAttribute('min'), '0');
  assertEq(input.getAttribute('max'), '100');
  assertEq(input.getAttribute('step'), '0.5');
  assertEq(input.getAttribute('inputmode'), 'decimal');
});

test('NumericInput precision=0 → inputmode=numeric', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 1], [4, 0],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ]));
  assertEq(el.querySelector('input').getAttribute('inputmode'), 'numeric');
});

test('NumericInput change emituje rounded value na precision', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 0.01], [4, 2],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '3.14159';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: 3.14, kind: 'f64' });
});

test('NumericInput bubbled native change emits exactly one SDK event', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 0.01], [4, 2],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ]));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  el.addEventListener('input', (e) => events.push(['input', e.detail]));
  const input = el.querySelector('input');
  input.value = '3.14159';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  assertEq(events, [
    { value: 3.14, kind: 'f64' },
    ['input', { value: 3.14159, kind: 'f64' }],
  ]);
});

test('NumericInput pusta wartość → change.value=null', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 1], [4, 0],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: null, kind: null });
});

test('NumericInput locale_aware renderuje formatted badge', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('n'), value: 1234.5 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 0.1], [4, 1],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, true],
  ]));
  const badge = el.querySelector('.tf-numeric__formatted');
  assert(badge != null);
  assert(badge.textContent.length > 0);
});

test('NumericInput invalid format.kind throws nawet gdy locale_aware=false', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 1], [4, 0],
    [5, { kind: 'badkind' }],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ])));
});

test('NumericInput step=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [3, 0], [4, 0],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ])));
});

test('NumericInput min > max throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(NUMERIC_INPUT_TAG, [
    [0, PATH('n')], [1, 100], [2, 0], [3, 1], [4, 0],
    [6, { kind: 'literal', value: 'N' }],
    [8, 'md'], [9, false],
  ])));
});

// ============================================================================
// CurrencyInput
// ============================================================================

test('CurrencyInput defaults: step=0.01 precision=2', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CURRENCY_INPUT_TAG, [
    [0, PATH('amt')], [1, 'PLN'],
    [6, { kind: 'literal', value: 'Cena' }],
    [8, 'md'], [9, true], [10, true],
  ]));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('step'), '0.01');
});

test('CurrencyInput show_symbol=true renderuje symbol prefix', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CURRENCY_INPUT_TAG, [
    [0, PATH('amt')], [1, 'EUR'],
    [6, { kind: 'literal', value: 'C' }],
    [8, 'md'], [9, true], [10, true],
  ]));
  const sym = el.querySelector('.tf-currency__symbol');
  assert(sym != null && sym.textContent.length > 0);
});

test('CurrencyInput formatted badge renderuje currency string', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('amt'), value: 99.99 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CURRENCY_INPUT_TAG, [
    [0, PATH('amt')], [1, 'USD'],
    [6, { kind: 'literal', value: 'C' }],
    [8, 'md'], [9, true], [10, true],
  ]));
  const badge = el.querySelector('.tf-currency__formatted');
  assert(badge.textContent.includes('99'));
});

test('CurrencyInput change emituje rounded value + currency', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CURRENCY_INPUT_TAG, [
    [0, PATH('amt')], [1, 'PLN'],
    [6, { kind: 'literal', value: 'C' }],
    [8, 'md'], [9, false], [10, true],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '12.999';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: 13.00, kind: 'f64', currency: 'PLN' });
});

test('CurrencyInput invalid currency_code throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CURRENCY_INPUT_TAG, [
    [0, PATH('amt')], [1, 'pln'],  // lowercase
    [6, { kind: 'literal', value: 'C' }],
    [8, 'md'], [9, false], [10, false],
  ])));
});

test('CurrencyInput bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CURRENCY_INPUT_TAG, [
    [0, PATH('amt')], [1, 'PLN'],
    [8, 'md'], [9, false], [10, false],
  ])));
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
