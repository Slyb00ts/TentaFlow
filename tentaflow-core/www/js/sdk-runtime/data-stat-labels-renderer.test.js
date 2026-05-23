// =============================================================================
// Plik: sdk-runtime/data-stat-labels-renderer.test.js
// Opis: Testy KeyValue/StatCard/Stat/Badge/Chip/Tag — chunk 3.3d-2.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  KEY_VALUE_TAG, STAT_CARD_TAG, STAT_TAG, BADGE_TAG, CHIP_TAG, TAG_TAG,
} from './data-stat-labels-renderer.js';

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
// KeyValue
// ============================================================================

test('KeyValue renderuje <dl> z N row', () => {
  setup();
  const engine = makeEngine();
  const item = (lbl, val) => [
    [0, { kind: 'literal', value: lbl }],
    [1, { kind: 'literal', value: val }],
  ];
  const el = engine.render(comp(KEY_VALUE_TAG, [
    [0, [item('Name', 'Ala'), item('Age', '30')]],
    [1, 'default'], [2, 'horizontal'],
  ]));
  assertEq(el.tagName, 'DL');
  assertEq(el.querySelectorAll('.tf-keyvalue__row').length, 2);
});

test('KeyValue item z action_id renderuje clickable + emit item_click', () => {
  setup();
  const engine = makeEngine();
  const item = [
    [0, { kind: 'literal', value: 'Email' }],
    [1, { kind: 'literal', value: 'a@b.com' }],
    [4, 'open_email'],
  ];
  const el = engine.render(comp(KEY_VALUE_TAG, [
    [0, [item]],
    [1, 'default'], [2, 'horizontal'],
  ]));
  let got = null;
  el.addEventListener('item_click', (e) => { got = e.detail; });
  el.querySelector('.tf-keyvalue__value--clickable').click();
  assertEq(got, { action_id: 'open_email', item_index: 0 });
});

test('KeyValue item invalid action_id grammar throws', () => {
  setup();
  const engine = makeEngine();
  const item = [
    [0, { kind: 'literal', value: 'L' }],
    [1, { kind: 'literal', value: 'V' }],
    [4, 'Bad Action!'],
  ];
  assertThrows(() => engine.render(comp(KEY_VALUE_TAG, [
    [0, [item]], [1, 'default'], [2, 'stacked'],
  ])));
});

test('KeyValue layout=grid ustawia klasę', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(KEY_VALUE_TAG, [
    [0, []], [1, 'default'], [2, 'grid'],
  ]));
  assert(el.classList.contains('tf-keyvalue--layout-grid'));
});

test('KeyValue unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(KEY_VALUE_TAG, [
    [0, []], [1, 'default'], [2, 'stacked'], [99, 'x'],
  ])));
});

// ============================================================================
// StatCard
// ============================================================================

test('StatCard clickable=true renderuje <button>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'Revenue' }],
    [2, { kind: 'literal', value: '12345' }],
    [8, true],
  ]));
  assertEq(el.tagName, 'BUTTON');
  assertEq(el.getAttribute('type'), 'button');
});

test('StatCard clickable=false renderuje <article>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }],
    [8, false],
  ]));
  assertEq(el.tagName, 'ARTICLE');
});

test('StatCard trend=up renderuje arrow + percent', () => {
  setup();
  const engine = makeEngine();
  const trend = [[0, 'up'], [1, 12.5]];
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1k' }],
    [5, trend], [8, false],
  ]));
  const t = el.querySelector('.tf-trend--up');
  assert(t != null);
  assertEq(t.querySelector('.tf-trend__arrow').textContent, '▲');
  assertEq(t.querySelector('.tf-trend__percent').textContent, '12.5%');
});

test('StatCard footnote renderuje tone class', () => {
  setup();
  const engine = makeEngine();
  const fn = [
    [0, 'warning'],
    [2, { kind: 'literal', value: 'Updated 1h ago' }],
  ];
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }],
    [6, fn], [8, false],
  ]));
  const f = el.querySelector('.tf-footnote');
  assert(f != null);
  assert(f.classList.contains('tf-footnote--tone-warning'));
  assertEq(f.querySelector('.tf-footnote__content').textContent, 'Updated 1h ago');
});

test('StatCard accent ustawia klasę border', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }],
    [7, 'critical'], [8, false],
  ]));
  assert(el.classList.contains('tf-stat-card--accent-critical'));
});

test('StatCard format currency formatuje value', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: 1234.5 }],
    [4, { kind: 'currency', code: 'USD' }],
    [8, false],
  ]));
  const v = el.querySelector('.tf-stat-card__value').textContent;
  assert(v.includes('1') && v.includes('234'));
});

test('StatCard trend brak direction throws', () => {
  setup();
  const engine = makeEngine();
  const trend = [[1, 10]];
  assertThrows(() => engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }],
    [5, trend], [8, false],
  ])));
});

test('StatCard label required throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STAT_CARD_TAG, [
    [2, { kind: 'literal', value: '1' }], [8, false],
  ])));
});

test('StatCard unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }], [8, false], [99, 'x'],
  ])));
});

// ============================================================================
// Stat
// ============================================================================

test('Stat renderuje label+value+size class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_TAG, [
    [0, { kind: 'literal', value: 'Users' }],
    [1, { kind: 'literal', value: '42' }],
    [4, 'lg'],
  ]));
  assertEq(el.querySelector('.tf-stat__label').textContent, 'Users');
  assertEq(el.querySelector('.tf-stat__value').textContent, '42');
  assert(el.classList.contains('tf-stat--size-lg'));
});

test('Stat invalid size throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STAT_TAG, [
    [0, { kind: 'literal', value: 'X' }], [1, { kind: 'literal', value: '1' }],
    [4, 'huge'],
  ])));
});

// ============================================================================
// Badge
// ============================================================================

test('Badge variant=solid + tone=success renderuje klasy', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BADGE_TAG, [
    [0, 'solid'], [1, 'success'],
    [2, { kind: 'literal', value: 'NEW' }],
    [5, 99], [6, false],
  ]));
  assert(el.classList.contains('tf-badge--variant-solid'));
  assert(el.classList.contains('tf-badge--tone-success'));
  assertEq(el.querySelector('.tf-badge__label').textContent, 'NEW');
});

test('Badge count > max → overflow display N+', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('n'), value: 250 }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(BADGE_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'M' }],
    [4, { kind: 'bound', path: PATH('n') }],
    [5, 99], [6, false],
  ]));
  assertEq(el.querySelector('.tf-badge__count').textContent, '99+');
});

test('Badge count ≤ max → display raw', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BADGE_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'M' }],
    [4, { kind: 'literal', value: 42 }],
    [5, 99], [6, false],
  ]));
  assertEq(el.querySelector('.tf-badge__count').textContent, '42');
});

test('Badge variant=dot bez tekstu, z sr-only label', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BADGE_TAG, [
    [0, 'dot'], [1, 'critical'],
    [2, { kind: 'literal', value: 'New activity' }],
    [5, 99], [6, false],
  ]));
  assertEq(el.querySelector('.tf-badge__label'), null);
  assertEq(el.getAttribute('role'), 'status');
  const sr = el.querySelector('.tf-visually-hidden');
  assertEq(sr.textContent, 'New activity');
});

test('Badge max=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BADGE_TAG, [
    [0, 'solid'], [1, 'primary'],
    [2, { kind: 'literal', value: 'X' }],
    [5, 0], [6, false],
  ])));
});

// ============================================================================
// Chip
// ============================================================================

test('Chip variant=solid non-interactive renderuje <span>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'solid'], [1, 'primary'],
    [2, { kind: 'literal', value: 'tag' }],
    [6, false],
  ]));
  assertEq(el.tagName, 'SPAN');
});

test('Chip root jest ZAWSZE <span> (uniknięcie button-in-button)', () => {
  setup();
  const engine = makeEngine();
  for (const v of ['solid', 'soft', 'outline', 'removable', 'selectable', 'toggle']) {
    const el = engine.render(comp(CHIP_TAG, [
      [0, v], [1, 'primary'],
      [2, { kind: 'literal', value: 'x' }],
      [6, v === 'removable'],
    ], { id: `c_${v}` }));
    assertEq(el.tagName, 'SPAN', `variant=${v}`);
  }
});

test('Chip removable renderuje <button.tf-chip__remove>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'removable'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [6, true],
  ]));
  const rm = el.querySelector('.tf-chip__remove');
  assertEq(rm.tagName, 'BUTTON');
});

test('Chip selectable role=option + tabindex=0', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'selectable'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [6, false],
  ]));
  assertEq(el.getAttribute('role'), 'option');
  assertEq(el.getAttribute('tabindex'), '0');
});

test('Chip × click emituje remove event', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'removable'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [6, true],
  ]));
  let fired = false;
  el.addEventListener('remove', () => { fired = true; });
  el.querySelector('.tf-chip__remove').click();
  assertEq(fired, true);
});

test('Chip selectable + selected BindRef → aria-pressed reaktywne', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: true }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'selectable'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [5, { kind: 'bound', path: PATH('sel') }],
    [6, false],
  ]));
  assertEq(el.getAttribute('aria-pressed'), 'true');
  assert(el.classList.contains('tf-chip--selected'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: false } }],
  });
  assertEq(el.getAttribute('aria-pressed'), 'false');
});

test('Chip icon + avatar mutually exclusive throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [3, { kind: 'named', name: 'check' }],
    [4, { kind: 'initials', initials: 'AB' }],
    [6, false],
  ])));
});

test('Chip avatar kind=initials renderuje', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'Ala' }],
    [4, { kind: 'initials', initials: 'A' }],
    [6, false],
  ]));
  assertEq(el.querySelector('.tf-avatar-ref__initials').textContent, 'A');
});

test('Chip avatar kind=image z https renderuje <img>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'X' }],
    [4, { kind: 'image', ref: 'https://example.com/a.png' }],
    [6, false],
  ]));
  const img = el.querySelector('.tf-avatar-ref__img');
  assertEq(img.tagName, 'IMG');
  assertEq(img.getAttribute('src'), 'https://example.com/a.png');
});

test('Chip avatar kind=image javascript: ref throws (XSS guard)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [4, { kind: 'image', ref: 'javascript:alert(1)' }],
    [6, false],
  ])));
});

test('Chip avatar kind=icon renderuje SVG', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'X' }],
    [4, { kind: 'icon', icon: { kind: 'named', name: 'check' } }],
    [6, false],
  ]));
  assert(el.querySelector('.tf-avatar-ref__icon') != null);
});

test('Chip avatar unknown kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [4, { kind: 'video' }],
    [6, false],
  ])));
});

test('Chip avatar unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(CHIP_TAG, [
    [0, 'soft'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [4, { kind: 'initials', initials: 'A', color: 'red' }],
    [6, false],
  ])));
});

// ============================================================================
// Tag
// ============================================================================

test('Tag renderuje <span> z tone+size classes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TAG_TAG, [
    [0, 'info'],
    [1, { kind: 'literal', value: 'beta' }],
    [2, 'sm'],
  ]));
  assertEq(el.tagName, 'SPAN');
  assert(el.classList.contains('tf-tag--tone-info'));
  assert(el.classList.contains('tf-tag--size-sm'));
  assertEq(el.textContent, 'beta');
});

test('Tag invalid size throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TAG_TAG, [
    [0, 'info'], [1, { kind: 'literal', value: 'x' }], [2, 'huge'],
  ])));
});

test('Tag invalid tone throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TAG_TAG, [
    [0, 'rainbow'], [1, { kind: 'literal', value: 'x' }], [2, 'sm'],
  ])));
});

test('Tag unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TAG_TAG, [
    [0, 'info'], [1, { kind: 'literal', value: 'x' }], [2, 'sm'], [99, 'x'],
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
