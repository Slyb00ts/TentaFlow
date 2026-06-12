// =============================================================================
// File: sdk-runtime/form-datetime-renderer.test.js
// Description: Tests for DatePicker (0x0314) / DateRangePicker (0x0315) /
// TimePicker (0x0316) / DateTimePicker (0x0317). Date pickers render through
// the <tf-datepicker> calendar component (imported for happy-dom upgrade);
// time/datetime pickers use native inputs.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-datepicker.js';
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
// tf-datepicker builds its calendar in connectedCallback, so tests that
// click calendar days mount the rendered tree first.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

// ============================================================================
// DatePicker
// ============================================================================

test('DatePicker renders <tf-datepicker> with min/max attrs + label', () => {
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
  assert(el.classList.contains('tf-datepicker-wrapper'));
  assert(el.classList.contains('tf-datepicker--format-medium'));
  assertEq(el.getAttribute('data-first-day-of-week'), 'monday');
  assertEq(el.querySelector('.tf-datepicker__label').textContent, 'Data');
  const picker = el.querySelector('tf-datepicker');
  assertEq(picker.getAttribute('min'), '2024-01-01');
  assertEq(picker.getAttribute('max'), '2025-12-31');
});

test('DatePicker reactive value sync from store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('d'), value: '2024-06-15' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ]));
  const picker = el.querySelector('tf-datepicker');
  assertEq(picker.getAttribute('value'), '2024-06-15');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('d'), op: { kind: 'set', value: '2024-07-01' } }],
  });
  assertEq(picker.getAttribute('value'), '2024-07-01');
});

test('DatePicker non-ISO store value is ignored (picker keeps last value)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('d'), value: '2024-06-15' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ]));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('d'), op: { kind: 'set', value: '15/06/2024' } }],
  });
  assertEq(el.querySelector('tf-datepicker').getAttribute('value'), '2024-06-15');
});

test('DatePicker picker change re-emits value + kind=tstr on wrapper', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const picker = el.querySelector('tf-datepicker');
  picker.value = '2024-06-15';
  picker.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: '2024-06-15', kind: 'tstr' });
});

test('DatePicker calendar day click emits SDK change with clicked ISO date', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ])));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  const day = el.querySelector('.tf-dp-day:not(.other)');
  const iso = day.dataset.date;
  day.click();
  // The renderer re-emits the SDK-shaped payload on the wrapper.
  const sdk = events.find((d) => d && d.kind === 'tstr');
  assert(sdk != null, 'expected SDK change event with kind=tstr');
  assertEq(sdk.value, iso);
  assertEq(el.querySelector('tf-datepicker').value, iso);
});

test('DatePicker day click emits exactly ONE SDK change, raw detail never leaks', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
  ])));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  const day = el.querySelector('.tf-dp-day:not(.other)');
  const iso = day.dataset.date;
  // tf-datepicker dispatches a BUBBLING change with detail { value, date };
  // the renderer must stop it so only the SDK re-emit reaches the wrapper.
  day.click();
  assertEq(events.length, 1);
  assertEq(events[0], { value: iso, kind: 'tstr' });
  assert(!('date' in events[0]), 'raw tf-datepicker { value, date } detail leaked');
});

test('DatePicker disabled date: bubbling raw change never reaches wrapper', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [7, ['2024-06-15']],
  ]));
  const picker = el.querySelector('tf-datepicker');
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  picker.value = '2024-06-15';  // disabled — renderer reverts, emits nothing
  picker.dispatchEvent(new CustomEvent('change', {
    bubbles: true, detail: { value: '2024-06-15', date: new Date() },
  }));
  assertEq(events, []);
});

test('DatePicker disabled_dates reverts to previous value', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [7, ['2024-06-15', '2024-06-16']],
  ]));
  const picker = el.querySelector('tf-datepicker');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  picker.value = '2024-06-15';  // disabled
  picker.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, null);
  assertEq(picker.value, '');  // reverted to empty (initial lastValid)
});

test('DatePicker preset Today emits current date with preset_id', () => {
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
  const btn = el.querySelector('.tf-datepicker__presets [data-preset-id=today]');
  assertEq(btn.tagName.toLowerCase(), 'tf-button');
  assertEq(btn.textContent, 'Dzisiaj');
  btn.click();
  assert(got != null);
  assertEq(got.preset_id, 'today');
  assertEq(got.kind, 'tstr');
  assert(/^\d{4}-\d{2}-\d{2}$/.test(got.value));
  assertEq(el.querySelector('tf-datepicker').value, got.value);
});

test('DatePicker preset custom offset_days=-10 emits valid ISO date', () => {
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
  el.querySelector('[data-preset-id=p10]').click();
  // Exact value depends on run time — a valid ISO date is sufficient.
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

test('DatePicker preset Last7Days uses snake_case kind=last_7_days', () => {
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
  el.querySelector('[data-preset-id="7d"]').click();
  assert(got != null && /^\d{4}-\d{2}-\d{2}$/.test(got.value));
});

test('DatePicker disabled date after preset reverts to preset value (NOT empty)', () => {
  setup();
  const engine = makeEngine();
  const presetToday = [[0, 'today'], [1, { kind: 'literal', value: 'T' }], [2, { kind: 'today' }]];
  const el = engine.render(comp(DATE_PICKER_TAG, [
    [0, PATH('d')], [1, { kind: 'literal', value: 'D' }],
    [5, 'short'], [6, 'monday'],
    [7, ['2099-01-01']],  // disabled future date
    [8, [presetToday]],
  ]));
  const picker = el.querySelector('tf-datepicker');
  el.querySelector('[data-preset-id=today]').click();
  const presetValue = picker.value;
  assert(presetValue.length > 0);
  // User then picks a disabled date — must revert to preset value, NOT empty.
  picker.value = '2099-01-01';
  picker.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(picker.value, presetValue);
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

test('DateRangePicker renders 2x <tf-datepicker> + dash', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Zakres' }],
    [3, '2024-01-01'], [4, '2025-12-31'],
    [6, 'short'], [7, 'monday'],
  ]));
  const pickers = el.querySelectorAll('tf-datepicker');
  assertEq(pickers.length, 2);
  assertEq(pickers[0].getAttribute('min'), '2024-01-01');
  assertEq(pickers[1].getAttribute('max'), '2025-12-31');
  assert(el.querySelector('.tf-daterange__dash') != null);
  assertEq(el.querySelector('.tf-daterange__label').textContent, 'Zakres');
});

test('DateRangePicker from > to reverts the to picker', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
  ]));
  const [fromP, toP] = el.querySelectorAll('tf-datepicker');
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  fromP.value = '2024-06-15';
  fromP.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(events.length, 1);
  assertEq(events[0], { value: { from: '2024-06-15', to: null }, kind: 'range', changed: 'from' });
  toP.value = '2024-06-10';  // before from
  toP.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  // Second change must be reverted and not emitted.
  assertEq(events.length, 1);
  assertEq(toP.value, '');
});

test('DateRangePicker bubbling raw picker change yields exactly one SDK range event', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
  ]));
  const [fromP] = el.querySelectorAll('tf-datepicker');
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  fromP.value = '2024-06-15';
  fromP.dispatchEvent(new CustomEvent('change', {
    bubbles: true, detail: { value: '2024-06-15', date: new Date() },
  }));
  assertEq(events.length, 1);
  assertEq(events[0], { value: { from: '2024-06-15', to: null }, kind: 'range', changed: 'from' });
  assert(!('date' in events[0]), 'raw tf-datepicker detail leaked to wrapper');
});

test('DateRangePicker max_range_days enforcement', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
    [12, 7],  // max 7 days
  ]));
  const [fromP, toP] = el.querySelectorAll('tf-datepicker');
  fromP.value = '2024-06-01';
  fromP.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  toP.value = '2024-06-15';  // 15 days span
  toP.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(toP.value, '');  // reverted
});

test('DateRangePicker preset emits range value', () => {
  setup();
  const engine = makeEngine();
  const inner = [[0, -6], [1, 0]];  // last 7 days
  const preset = [[0, 'p7'], [1, { kind: 'literal', value: '7 dni' }], [2, inner]];
  const el = engine.render(comp(DATE_RANGE_PICKER_TAG, [
    [0, PATH('from')], [1, PATH('to')],
    [2, { kind: 'literal', value: 'Z' }],
    [6, 'short'], [7, 'monday'],
    [9, [preset]],
  ]));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const btn = el.querySelector('.tf-daterange__presets [data-preset-id=p7]');
  btn.click();
  assertEq(got.kind, 'range');
  assertEq(got.preset_id, 'p7');
  assert(typeof got.value.from === 'string');
  assert(typeof got.value.to === 'string');
  const [fromP, toP] = el.querySelectorAll('tf-datepicker');
  assertEq(fromP.value, got.value.from);
  assertEq(toP.value, got.value.to);
});

test('DateRangePicker preset with from > to does NOT emit', () => {
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
  el.querySelector('[data-preset-id=bad]').click();
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

test('TimePicker bubbled native change emits exactly one SDK event', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TIME_PICKER_TAG, [
    [0, PATH('t')], [1, 'minute'], [2, 'short'], [3, 1],
    [4, { kind: 'literal', value: 'G' }],
  ]));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  const input = el.querySelector('input');
  input.value = '09:45';
  // Real-browser bubbling: the native change reaches the wrapper unless the
  // renderer stops it — only the SDK re-emit may arrive.
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(events, [{ value: '09:45', kind: 'tstr' }]);
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

test('DateTimePicker bubbled native change emits exactly one SDK event', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(DATE_TIME_PICKER_TAG, [
    [0, PATH('dt')], [1, { kind: 'literal', value: 'DT' }],
    [4, 'short'], [5, 'short'], [6, 'minute'], [7, 1], [9, 'monday'],
    [11, 'UTC'],
  ]));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  const input = el.querySelector('input');
  input.value = '2024-06-15T14:30';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(events, [{ value: '2024-06-15T14:30', kind: 'tstr', timezone: 'UTC' }]);
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
