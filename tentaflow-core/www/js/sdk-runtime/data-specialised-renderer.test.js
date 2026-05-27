// =============================================================================
// Plik: sdk-runtime/data-specialised-renderer.test.js
// Opis: Testy CalendarMonth (0x0223), Image (0x0224), VisuallyHidden
// (0x0225), LiveRegionComponent (0x0226) — chunk 3.3d-15.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  CALENDAR_MONTH_TAG, IMAGE_TAG, VISUALLY_HIDDEN_TAG, LIVE_REGION_TAG,
} from './data-specialised-renderer.js';

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
const LIT = (value) => ({ kind: 'literal', value });
const BOUND = (...segs) => ({ kind: 'bound', path: PATH(...segs) });

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
// CalendarMonth
// ============================================================================

function calFields({
  month = LIT('2026-05'),
  eventsPath = null,
  showWeekNumbers = false,
  firstDay = 'monday',
} = {}) {
  const f = [[0, month]];
  if (eventsPath != null) f.push([1, eventsPath]);
  f.push([2, showWeekNumbers], [3, firstDay]);
  return f;
}

test('CalendarMonth renderuje 7-kolumnowy grid z day buttons', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields()));
  document.body.appendChild(el);
  // May 2026 has 31 days.
  const dayBtns = el.querySelectorAll('button.tf-calendar__day:not(.tf-calendar__day--outside)');
  assertEq(dayBtns.length, 31);
});

test('CalendarMonth first_day=sunday ma Su jako pierwszy dow', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields({ firstDay: 'sunday' })));
  document.body.appendChild(el);
  const dows = el.querySelectorAll('.tf-calendar__dow-cell');
  assertEq(dows[0].textContent, 'Su');
});

test('CalendarMonth first_day=monday ma Mo jako pierwszy dow', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields()));
  document.body.appendChild(el);
  const dows = el.querySelectorAll('.tf-calendar__dow-cell');
  assertEq(dows[0].textContent, 'Mo');
});

test('CalendarMonth show_week_numbers renderuje week-no cells', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields({ showWeekNumbers: true })));
  document.body.appendChild(el);
  assert(el.querySelectorAll('.tf-calendar__week-no').length > 0);
});

test('CalendarMonth events_path marks day with has-event', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('ev'), value: [{ date: '2026-05-15', tone: 'success', label: 'Meeting' }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields({ eventsPath: PATH('ev') })));
  document.body.appendChild(el);
  const day15 = el.querySelector('[data-date="2026-05-15"]');
  assert(day15 != null);
  assert(day15.classList.contains('tf-calendar__day--has-event'));
  assert(day15.classList.contains('tf-calendar__day--tone-success'));
});

test('CalendarMonth reaguje na patch month', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('m'), value: '2026-01' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields({ month: BOUND('m') })));
  document.body.appendChild(el);
  // Jan 2026: 31 days.
  assertEq(el.querySelectorAll('button.tf-calendar__day:not(.tf-calendar__day--outside)').length, 31);
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('m'), op: { kind: 'set', value: '2026-02' } }] });
  // Feb 2026: 28 days.
  assertEq(el.querySelectorAll('button.tf-calendar__day:not(.tf-calendar__day--outside)').length, 28);
});

test('CalendarMonth invalid month shows error', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALENDAR_MONTH_TAG, calFields({ month: LIT('bad') })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-calendar__error') != null);
});

// ============================================================================
// Image
// ============================================================================

function imgFields({
  src = LIT('https://example.com/img.png'), alt = 'Photo',
  width = null, height = null, fit = 'cover',
  aspectRatio = null, radius = null,
  clickable = false, lazyLoad = false,
} = {}) {
  const f = [[0, src], [1, alt]];
  if (width != null) f.push([2, width]);
  if (height != null) f.push([3, height]);
  f.push([4, fit]);
  if (aspectRatio != null) f.push([5, aspectRatio]);
  if (radius != null) f.push([6, radius]);
  f.push([7, clickable], [8, lazyLoad]);
  return f;
}

test('Image renderuje <img> z src i alt', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields()));
  document.body.appendChild(el);
  const img = el.querySelector('img');
  assert(img != null);
  assertEq(img.alt, 'Photo');
  assert(img.src.includes('example.com'));
});

test('Image fit=contain ustawia object-fit', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields({ fit: 'contain' })));
  document.body.appendChild(el);
  assertEq(el.querySelector('img').style.objectFit, 'contain');
});

test('Image lazy_load=true ustawia loading=lazy', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields({ lazyLoad: true })));
  document.body.appendChild(el);
  assertEq(el.querySelector('img').loading, 'lazy');
});

test('Image clickable=true dodaje role=button', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields({ clickable: true })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('role'), 'button');
  assert(el.classList.contains('tf-image--clickable'));
});

test('Image radius=pill dodaje klasę', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields({ radius: 'pill' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-image--radius-pill'));
});

test('Image javascript: src rejected', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields({ src: LIT('javascript:alert(1)') })));
  document.body.appendChild(el);
  assert(!el.querySelector('img').hasAttribute('src'));
});

test('Image reaguje na patch src_ref', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('s'), value: 'a.png' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(IMAGE_TAG, imgFields({ src: BOUND('s') })));
  document.body.appendChild(el);
  assert(el.querySelector('img').src.includes('a.png'));
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('s'), op: { kind: 'set', value: 'b.png' } }] });
  assert(el.querySelector('img').src.includes('b.png'));
});

test('Image DimensionToken auto rejects extra value', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(IMAGE_TAG, imgFields({ width: { kind: 'auto', value: 10 } }))));
});

test('Image DimensionToken px rejects string value', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(IMAGE_TAG, imgFields({ width: { kind: 'px', value: '200' } }))));
});

test('Image AspectRatio fixed rejects extra ratio field', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(IMAGE_TAG, imgFields({ aspectRatio: { kind: '16:9', ratio: 1.78 } }))));
});

test('Image AspectRatio custom rejects Infinity', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(IMAGE_TAG, imgFields({ aspectRatio: { kind: 'custom', ratio: Infinity } }))));
});

test('Image width DimensionToken px', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(IMAGE_TAG, imgFields({ width: { kind: 'px', value: 200 } })));
  document.body.appendChild(el);
  assertEq(el.style.width, '200px');
});

// ============================================================================
// VisuallyHidden
// ============================================================================

function vhFields({ content = LIT('SR-only text'), asLive = null } = {}) {
  const f = [[0, content]];
  if (asLive != null) f.push([1, asLive]);
  return f;
}

test('VisuallyHidden renderuje span z CSS clip class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(VISUALLY_HIDDEN_TAG, vhFields()));
  document.body.appendChild(el);
  assertEq(el.tagName, 'SPAN');
  assert(el.classList.contains('tf-visually-hidden'));
  assertEq(el.textContent, 'SR-only text');
});

test('VisuallyHidden as_live=polite adds aria-live', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(VISUALLY_HIDDEN_TAG, vhFields({ asLive: 'polite' })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-live'), 'polite');
});

test('VisuallyHidden as_live=off does not add aria-live', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(VISUALLY_HIDDEN_TAG, vhFields({ asLive: 'off' })));
  document.body.appendChild(el);
  assert(el.getAttribute('aria-live') == null);
});

test('VisuallyHidden reaguje na patch content', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('t'), value: 'old' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(VISUALLY_HIDDEN_TAG, vhFields({ content: BOUND('t') })));
  document.body.appendChild(el);
  assertEq(el.textContent, 'old');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('t'), op: { kind: 'set', value: 'new' } }] });
  assertEq(el.textContent, 'new');
});

// ============================================================================
// LiveRegion
// ============================================================================

function lrFields({
  politeness = 'polite', content = LIT('Saved'),
  visible = true, tone = null, icon = null, clearAfterMs = null,
} = {}) {
  const f = [[0, politeness], [1, content], [2, visible]];
  if (tone != null) f.push([3, tone]);
  if (icon != null) f.push([4, icon]);
  if (clearAfterMs != null) f.push([5, clearAfterMs]);
  return f;
}

test('LiveRegion renderuje aria-live + content', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LIVE_REGION_TAG, lrFields()));
  document.body.appendChild(el);
  assertEq(el.getAttribute('aria-live'), 'polite');
  assertEq(el.getAttribute('role'), 'status');
  assertEq(el.querySelector('.tf-live-region__content').textContent, 'Saved');
});

test('LiveRegion visible=false adds visually-hidden class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LIVE_REGION_TAG, lrFields({ visible: false })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-visually-hidden'));
});

test('LiveRegion tone=critical adds tone class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(LIVE_REGION_TAG, lrFields({ tone: 'critical' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-live-region--tone-critical'));
});

test('LiveRegion rejects politeness=off (spec narrows to polite/assertive)', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(LIVE_REGION_TAG, lrFields({ politeness: 'off' }))));
});

test('LiveRegion reaguje na patch content', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('msg'), value: 'Loading...' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(LIVE_REGION_TAG, lrFields({ content: BOUND('msg') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-live-region__content').textContent, 'Loading...');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('msg'), op: { kind: 'set', value: 'Done' } }] });
  assertEq(el.querySelector('.tf-live-region__content').textContent, 'Done');
});

test('LiveRegion odrzuca clear_after_ms ujemne', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(LIVE_REGION_TAG, lrFields({ clearAfterMs: -1 }))));
});

test('LiveRegion odrzuca clear_after_ms > u32 max', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(LIVE_REGION_TAG, lrFields({ clearAfterMs: 0xFFFFFFFF + 1 }))));
});

test('LiveRegion icon renders IconRef (named)', () => {
  setup();
  const engine = makeEngine(makeStore());
  const iconRef = { kind: 'named', name: 'check', size: 'md' };
  const el = engine.render(comp(LIVE_REGION_TAG, lrFields({ icon: iconRef })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-live-region__icon') != null);
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`specialised tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
