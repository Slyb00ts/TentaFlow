// =============================================================================
// Plik: sdk-runtime/form-datetime-renderer.test.js
// Opis: Testy DatePicker/DateRangePicker/TimePicker/DateTimePicker — chunk 3.3c-4.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  DATE_PICKER_TAG, DATE_RANGE_PICKER_TAG, TIME_PICKER_TAG, DATE_TIME_PICKER_TAG,
} from './form-datetime-renderer.js';

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

// ============================================================================
// DatePicker
// ============================================================================

test('DatePicker renderuje <input type=date> z min/max', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')],
    [1, { kind: 'literal', value: 'Data' }],
    [2, '2024-01-01'],
    [3, '2025-12-31'],
    [5, 'medium'],
    [6, 'monday'],
  ]));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('type'), 'date');
  assertEq(input.getAttribute('min'), '2024-01-01');
  assertEq(input.getAttribute('max'), '2025-12-31');
});

test('DatePicker reactive value sync ze store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('d'), value: '2024-06-15' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ]));
  assertEq(el.querySelector('input').value, '2024-06-15');
});

test('DatePicker change emituje value + kind=tstr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '2024-06-15';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: '2024-06-15', kind: 'tstr' });
});

test('DatePicker disabled_dates revert do poprzedniej wartości', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [7, ['2024-06-15', '2024-06-16']],
  ]));
  const input = el.querySelector('input');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  input.value = '2024-06-15';  // disabled
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, null);
  assertEq(input.value, '');  // revert do pustego (initial lastValid)
});

test('DatePicker preset Today emituje aktualną datę', () => {
  setup();
  const engine = makeEngine();
  const preset = [[0, 'today'], [1, { kind: 'literal', value: 'Dzisiaj' }], [2, { kind: 'today' }]];
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [8, [preset]],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-datepicker__preset').click();
  assert(got != null);
  assertEq(got.preset_id, 'today');
  assertEq(got.kind, 'tstr');
  assert(/^\d{4}-\d{2}-\d{2}$/.test(got.value));
});

test('DatePicker preset custom offset_days=-10', () => {
  setup();
  const engine = makeEngine();
  const preset = [[0, 'p10'], [1, { kind: 'literal', value: '-10' }], [2, { kind: 'custom', offset_days: -10 }]];
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [8, [preset]],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-datepicker__preset').click();
  // Wystarczy że value jest valid ISO date — dokładna wartość zależy od testu time'u.
  assert(/^\d{4}-\d{2}-\d{2}$/.test(got.value));
});

test('DatePicker min_date > max_date throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [2, '2025-12-31'], [3, '2024-01-01'],
    [5, 'short'], [6, 'monday'],
  ])));
});

test('DatePicker invalid min_date format throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [2, '2024/01/01'],
    [5, 'short'], [6, 'monday'],
  ])));
});

test('DatePicker unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [99, 'x'],
  ])));
});

test('DatePicker preset Last7Days używa snake_case kind=last_7_days', () => {
  setup();
  const engine = makeEngine();
  const preset = [[0, '7d'], [1, { kind: 'literal', value: '7d' }], [2, { kind: 'last_7_days' }]];
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [8, [preset]],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-datepicker__preset').click();
  assert(got != null && /^\d{4}-\d{2}-\d{2}$/.test(got.value));
});

test('DatePicker disabled date po preset revert do preset value (NIE do pustego)', () => {
  setup();
  const engine = makeEngine();
  const presetToday = [[0, 'today'], [1, { kind: 'literal', value: 'T' }], [2, { kind: 'today' }]];
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [7, ['2099-01-01']],  // disabled future date
    [8, [presetToday]],
  ]));
  const input = el.querySelector('input');
  // Wybierz preset Today.
  el.querySelector('.tf-datepicker__preset').click();
  const presetValue = input.value;
  assert(presetValue.length > 0);
  // Teraz user wybiera disabled date — powinno revert do preset value, NIE do pustego.
  input.value = '2099-01-01';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(input.value, presetValue);
});

test('DatePicker bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [5, 'short'], [6, 'monday'],
  ])));
});

// ============================================================================
// DateRangePicker
// ============================================================================

test('DateRangePicker renderuje 2x input type=date + dash', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Zakres' }],
    [6, 'short'], [7, 'monday'],
  ]));
  const inputs = el.querySelectorAll('input[type=date]');
  assertEq(inputs.length, 2);
  assert(el.querySelector('.tf-daterange__dash') != null);
});

test('DateRangePicker from > to revert', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
  ]));
  const [fromI, toI] = el.querySelectorAll('input');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  fromI.value = '2024-06-15';
  fromI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  toI.value = '2024-06-10';  // przed from
  toI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  // Drugi change powinien być revert'owany.
  assertEq(toI.value, '');
});

test('DateRangePicker max_range_days enforcement', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
    [12, 7],  // max 7 dni
  ]));
  const [fromI, toI] = el.querySelectorAll('input');
  fromI.value = '2024-06-01';
  fromI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  toI.value = '2024-06-15';  // 15 dni
  toI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(toI.value, '');  // revert
});

test('DateRangePicker preset emituje range value', () => {
  setup();
  const engine = makeEngine();
  const inner = [[0, -6], [1, 0]];  // ostatnie 7 dni
  const preset = [[0, 'p7'], [1, { kind: 'literal', value: '7 dni' }], [2, inner]];
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
    [9, [preset]],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-daterange__preset').click();
  assertEq(got.kind, 'range');
  assertEq(got.preset_id, 'p7');
  assert(typeof got.value.from === 'string');
  assert(typeof got.value.to === 'string');
});

test('DateRangePicker preset z from > to NIE emituje', () => {
  setup();
  const engine = makeEngine();
  // RangePresetRange { from_offset_days: 0, to_offset_days: -5 } → from > to
  const inner = [[0, 0], [1, -5]];
  const preset = [[0, 'bad'], [1, { kind: 'literal', value: 'BAD' }], [2, inner]];
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
    [9, [preset]],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  el.querySelector('.tf-daterange__preset').click();
  assertEq(got, null);
});

test('DateRangePicker max_range_days=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
    [12, 0],
  ])));
});

test('DateRangePicker bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [6, 'short'], [7, 'monday'],
  ])));
});

// ============================================================================
// TimePicker
// ============================================================================

test('TimePicker renderuje <input type=time> z step', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TIME_PICKER_TAG, [
    [0, PATH('t')], [1, 'minute'], [2, 'short'], [3, 15],
    [4, { kind: 'literal', value: 'Godzina' }],
  ]));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('type'), 'time');
  assertEq(input.getAttribute('step'), '900');  // 15 min = 900s
});

test('TimePicker step_minutes=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TIME_PICKER_TAG, [
    [0, PATH('t')], [1, 'minute'], [2, 'short'], [3, 0],
    [4, { kind: 'literal', value: 'G' }],
  ])));
});

test('TimePicker reactive value sync', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('t'), value: '14:30' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(TIME_PICKER_TAG, [
    [0, PATH('t')], [1, 'minute'], [2, 'short'], [3, 1],
    [4, { kind: 'literal', value: 'G' }],
  ]));
  assertEq(el.querySelector('input').value, '14:30');
});

test('TimePicker change emituje value', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TIME_PICKER_TAG, [
    [0, PATH('t')], [1, 'minute'], [2, 'short'], [3, 1],
    [4, { kind: 'literal', value: 'G' }],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '09:45';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: '09:45', kind: 'tstr' });
});

test('TimePicker bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TIME_PICKER_TAG, [
    [0, PATH('t')], [1, 'minute'], [2, 'short'], [3, 1],
  ])));
});

// ============================================================================
// DateTimePicker
// ============================================================================

test('DateTimePicker renderuje <input type=datetime-local>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 30], [9, 'monday'],
  ]));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('type'), 'datetime-local');
  assertEq(input.getAttribute('step'), '1800');  // 30 min
});

test('DateTimePicker timezone attribute', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 1], [9, 'monday'],
    [11, 'Europe/Warsaw'],
  ]));
  assertEq(el.getAttribute('data-timezone'), 'Europe/Warsaw');
});

test('DateTimePicker invalid timezone throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 1], [9, 'monday'],
    [11, 'not_a_tz'],
  ])));
});

test('DateTimePicker invalid min_datetime format throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [2, '2024-06-15'],  // brak T-time
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 1], [9, 'monday'],
  ])));
});

test('DateTimePicker change emituje value + timezone', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 1], [9, 'monday'],
    [11, 'UTC'],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '2024-06-15T14:30';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: '2024-06-15T14:30', kind: 'tstr', timezone: 'UTC' });
});

test('DateTimePicker step_minutes=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 0], [9, 'monday'],
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
