// =============================================================================
// Plik: sdk-runtime/data-stat-labels-renderer.test.js
// Opis: Testy KeyValue/StatCard/Stat/Badge/Chip/Tag — chunk 3.3d-2.
// =============================================================================

import { window as harnessWindow } from './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  KEY_VALUE_TAG, STAT_CARD_TAG, STAT_TAG, BADGE_TAG, CHIP_TAG, TAG_TAG,
} from './data-stat-labels-renderer.js';

// The harness exports bound globals; a bound class has no .prototype, which
// breaks `class X extends HTMLElement`. Restore the raw constructor before
// loading web components (dynamic import runs after the harness).
globalThis.HTMLElement = harnessWindow.HTMLElement;
await import('../components/tf-stat-card.js');
await import('../components/tf-chip.js');
await import('../components/tf-key-value.js');

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
// Attach to the document so custom elements upgrade (connectedCallback)
function mount(el) {
  document.body.appendChild(el);
  return el;
}

// ============================================================================
// KeyValue
// ============================================================================

test('KeyValue renderuje <tf-key-value> z N row', () => {
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
  mount(el);
  assertEq(el.tagName, 'TF-KEY-VALUE');
  const rows = el.querySelectorAll('tbody tr');
  assertEq(rows.length, 2);
  assertEq(rows[0].querySelector('.tf-kv-key').textContent, 'Name');
  assertEq(rows[0].querySelector('.tf-kv-value').textContent, 'Ala');
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
  mount(el);
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

test('StatCard clickable=true ustawia role=button na <tf-stat-card>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'Revenue' }],
    [2, { kind: 'literal', value: '12345' }],
    [8, true],
  ]));
  assertEq(el.tagName, 'TF-STAT-CARD');
  assertEq(el.getAttribute('role'), 'button');
  assertEq(el.getAttribute('tabindex'), '0');
  assert(el.classList.contains('tf-stat-card--clickable'));
});

test('StatCard clickable=false renderuje <tf-stat-card> bez role', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }],
    [8, false],
  ]));
  assertEq(el.tagName, 'TF-STAT-CARD');
  assertEq(el.getAttribute('role'), null);
});

test('StatCard trend=up renderuje delta + delta-type', () => {
  setup();
  const engine = makeEngine();
  const trend = [[0, 'up'], [1, 12.5]];
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1k' }],
    [5, trend], [8, false],
  ]));
  mount(el);
  assertEq(el.getAttribute('delta-type'), 'up');
  assertEq(el.getAttribute('delta'), '12.5%');
  const d = el.querySelector('.tf-stat-card-delta');
  assert(d != null);
  assert(d.classList.contains('up'));
  assert(d.textContent.includes('12.5%'));
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
  mount(el);
  const f = el.querySelector('.tf-footnote');
  assert(f != null);
  assert(f.classList.contains('tf-footnote--tone-warning'));
  assertEq(f.querySelector('.tf-footnote__content').textContent, 'Updated 1h ago');
});

test('StatCard accent ustawia atrybut accent', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_CARD_TAG, [
    [0, { kind: 'literal', value: 'R' }],
    [2, { kind: 'literal', value: '1' }],
    [7, 'critical'], [8, false],
  ]));
  mount(el);
  assertEq(el.getAttribute('accent'), 'danger');
  assert(el.querySelector('.tf-stat-card.accent-danger') != null);
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
  mount(el);
  const v = el.querySelector('.tf-stat-card-value').textContent;
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

test('Stat renderuje <tf-stat-card size> compact variant', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STAT_TAG, [
    [0, { kind: 'literal', value: 'Users' }],
    [1, { kind: 'literal', value: '42' }],
    [4, 'lg'],
  ]));
  mount(el);
  assertEq(el.tagName, 'TF-STAT-CARD');
  assertEq(el.getAttribute('size'), 'lg');
  assert(el.querySelector('.tf-stat.tf-stat--size-lg') != null);
  assertEq(el.querySelector('.tf-stat__label').textContent, 'Users');
  assertEq(el.querySelector('.tf-stat__value').textContent, '42');
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

test('Badge variant=solid + tone=success renderuje <tf-badge>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BADGE_TAG, [
    [0, 'solid'], [1, 'success'],
    [2, { kind: 'literal', value: 'NEW' }],
    [5, 99], [6, false],
  ]));
  assertEq(el.tagName, 'TF-BADGE');
  assertEq(el.getAttribute('tone'), 'success');
  assertEq(el.getAttribute('value'), 'NEW');
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

test('Badge reflects variant + pulse so generic CSS can target dot/pulse', () => {
  setup();
  const engine = makeEngine();
  const dot = engine.render(comp(BADGE_TAG, [
    [0, 'dot'], [1, 'success'],
    [2, { kind: 'literal', value: 'online' }],
    [5, 99], [6, true],
  ]));
  assertEq(dot.getAttribute('variant'), 'dot');
  assert(dot.hasAttribute('pulse'), 'pulse attribute set when Badge.pulse=true');

  const pill = engine.render(comp(BADGE_TAG, [
    [0, 'pulse'], [1, 'critical'],
    [2, { kind: 'literal', value: 'LIVE' }],
    [5, 99], [6, false],
  ]));
  assertEq(pill.getAttribute('variant'), 'pulse');
  assert(!pill.hasAttribute('pulse'), 'no pulse attribute when Badge.pulse=false');
});

test('Badge max=0 with a count throws', () => {
  setup();
  const engine = makeEngine();
  // max bounds the count overflow badge, so it is only validated when a count
  // is present; max=0 alongside a count is invalid.
  assertThrows(() => engine.render(comp(BADGE_TAG, [
    [0, 'solid'], [1, 'primary'],
    [2, { kind: 'literal', value: 'X' }],
    [4, 5], [5, 0], [6, false],
  ])));
});

test('Badge max=0 without a count is a plain label (no throw)', () => {
  setup();
  const engine = makeEngine();
  // A label/pill badge carries no count; its encoded default max (0) must not
  // reject the badge.
  const el = engine.render(comp(BADGE_TAG, [
    [0, 'solid'], [1, 'primary'],
    [2, { kind: 'literal', value: 'X' }],
    [5, 0], [6, false],
  ]));
  assert(el != null, 'label badge renders');
});

// ============================================================================
// Chip
// ============================================================================

test('Chip variant=solid non-interactive renderuje <tf-chip>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'solid'], [1, 'primary'],
    [2, { kind: 'literal', value: 'tag' }],
    [6, false],
  ]));
  mount(el);
  assertEq(el.tagName, 'TF-CHIP');
  const inner = el.querySelector('.tf-chip');
  assert(inner != null);
  assert(inner.classList.contains('accent')); // tone primary → status accent
  assertEq(el.getAttribute('label'), 'tag');
});

test('Chip root jest ZAWSZE <tf-chip>', () => {
  setup();
  const engine = makeEngine();
  for (const v of ['solid', 'soft', 'outline', 'removable', 'selectable', 'toggle']) {
    const el = engine.render(comp(CHIP_TAG, [
      [0, v], [1, 'primary'],
      [2, { kind: 'literal', value: 'x' }],
      [6, v === 'removable'],
    ], { id: `c_${v}` }));
    assertEq(el.tagName, 'TF-CHIP', `variant=${v}`);
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
  mount(el);
  const rm = el.querySelector('.tf-chip__remove');
  assertEq(rm.tagName, 'BUTTON');
});

test('Chip selectable role=button + tabindex=0', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(CHIP_TAG, [
    [0, 'selectable'], [1, 'primary'],
    [2, { kind: 'literal', value: 'x' }],
    [6, false],
  ]));
  mount(el);
  assert(el.hasAttribute('clickable'));
  assertEq(el.getAttribute('role'), 'button');
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
  mount(el);
  let fired = false;
  el.addEventListener('remove', () => { fired = true; });
  el.querySelector('.tf-chip__remove').click();
  assertEq(fired, true);
});

test('Chip selectable + selected BindRef → atrybut active reaktywny', () => {
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
  mount(el);
  assert(el.hasAttribute('active'));
  assert(el.querySelector('.tf-chip').classList.contains('active'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: false } }],
  });
  assertEq(el.hasAttribute('active'), false);
  assertEq(el.querySelector('.tf-chip').classList.contains('active'), false);
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
  mount(el);
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
  mount(el);
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
  mount(el);
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

test('Tag renderuje <tf-chip variant=tag> z tone+size classes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TAG_TAG, [
    [0, 'info'],
    [1, { kind: 'literal', value: 'beta' }],
    [2, 'sm'],
  ]));
  mount(el);
  assertEq(el.tagName, 'TF-CHIP');
  assertEq(el.getAttribute('variant'), 'tag');
  const tag = el.querySelector('.tf-tag');
  assert(tag != null);
  assert(tag.classList.contains('tf-tag--tone-info'));
  assert(tag.classList.contains('tf-tag--size-sm'));
  assertEq(tag.textContent, 'beta');
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
