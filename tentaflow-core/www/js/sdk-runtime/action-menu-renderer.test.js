// =============================================================================
// Plik: sdk-runtime/action-menu-renderer.test.js
// Opis: Testy MenuButton (0x0406) + Menu (0x0407) — chunk 3.3b-5.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { MENU_BUTTON_TAG, MENU_TAG } from './action-menu-renderer.js';

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

function item(id, label, extra = {}) {
  return {
    id,
    label: { kind: 'literal', value: label },
    danger: extra.danger ?? false,
    divider_after: extra.divider_after ?? false,
    ...(extra.icon != null ? { icon: extra.icon } : {}),
    ...(extra.shortcut != null ? { shortcut: extra.shortcut } : {}),
    ...(extra.disabled != null ? { disabled: extra.disabled } : {}),
  };
}

// ============================================================================
// MenuButton (0x0406)
// ============================================================================

test('MenuButton renders trigger + hidden popup z aria attrs', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'Akcje' }],
      [2, 'primary'],
      [3, [item('save', 'Zapisz'), item('delete', 'Usuń')]],
      [4, 'bottom_start'],
    ])
  );
  const trigger = el.querySelector('.tf-menu-button__trigger');
  assertEq(trigger.tagName, 'BUTTON');
  assertEq(trigger.getAttribute('aria-haspopup'), 'menu');
  assertEq(trigger.getAttribute('aria-expanded'), 'false');
  assertEq(trigger.textContent, 'Akcje');
  const popup = el.querySelector('.tf-menu-button__popup');
  assertEq(popup.getAttribute('role'), 'menu');
  assert(popup.hasAttribute('hidden'));
  assertEq(popup.querySelectorAll('.tf-menu__item').length, 2);
});

test('MenuButton trigger click toggles popup visibility', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'ghost'], [3, [item('a', 'A')]], [4, 'bottom_end'],
    ])
  );
  const trigger = el.querySelector('.tf-menu-button__trigger');
  const popup = el.querySelector('.tf-menu-button__popup');
  trigger.click();
  assert(!popup.hasAttribute('hidden'));
  assertEq(trigger.getAttribute('aria-expanded'), 'true');
  trigger.click();
  assert(popup.hasAttribute('hidden'));
});

test('MenuButton item click dispatches item_click + closes popup', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'], [3, [item('save', 'Save')]], [4, 'bottom_start'],
    ])
  );
  let received = null;
  el.addEventListener('item_click', (e) => { received = e.detail; });
  const trigger = el.querySelector('.tf-menu-button__trigger');
  trigger.click(); // open
  const item1 = el.querySelector('.tf-menu__item');
  item1.click();
  assertEq(received, { item_id: 'save' });
  assert(el.querySelector('.tf-menu-button__popup').hasAttribute('hidden'));
});

test('MenuButton Escape key zamyka popup', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'], [3, [item('a', 'A')]], [4, 'bottom_start'],
    ])
  );
  el.querySelector('.tf-menu-button__trigger').click();
  el.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(el.querySelector('.tf-menu-button__popup').hasAttribute('hidden'));
});

test('MenuButton item z shortcut renderuje span tf-menu__item-shortcut', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('save', 'Save', { shortcut: 'Ctrl+S' })]],
      [4, 'bottom_start'],
    ])
  );
  const sc = el.querySelector('.tf-menu__item-shortcut');
  assertEq(sc.textContent, 'Ctrl+S');
});

test('MenuButton item danger=true → tf-menu__item--danger', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('rm', 'Delete', { danger: true })]],
      [4, 'bottom_start'],
    ])
  );
  assert(el.querySelector('.tf-menu__item--danger') != null);
});

test('MenuButton item disabled reactive blocks click', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('d'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('save', 'Save', { disabled: { kind: 'bound', path: PATH('d') } })]],
      [4, 'bottom_start'],
    ])
  );
  let received = null;
  el.addEventListener('item_click', (e) => { received = e.detail; });
  el.querySelector('.tf-menu-button__trigger').click();
  const it = el.querySelector('.tf-menu__item');
  assertEq(it.getAttribute('aria-disabled'), 'true');
  it.click();
  assertEq(received, null);
  // Po wyłączeniu disabled item powinien działać.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('d'), op: { kind: 'set', value: false } }],
  });
  it.click();
  assertEq(received, { item_id: 'save' });
});

test('MenuButton item divider_after renderuje li role=separator', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [
        item('a', 'A', { divider_after: true }),
        item('b', 'B'),
      ]],
      [4, 'bottom_start'],
    ])
  );
  assert(el.querySelector('.tf-menu__divider') != null);
});

test('MenuButton rejects bez trigger_label i bez trigger_icon (a11y)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(MENU_BUTTON_TAG, [
        [2, 'primary'], [3, [item('a', 'A')]], [4, 'bottom_start'],
      ])
    )
  );
});

test('MenuButton rejects item z badge present', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(MENU_BUTTON_TAG, [
        [0, { kind: 'literal', value: 'X' }],
        [2, 'primary'],
        [3, [{ id: 'a', label: { kind: 'literal', value: 'A' }, danger: false,
                divider_after: false, badge: { variant: 'solid', tone: 'primary', pulse: false } }]],
        [4, 'bottom_start'],
      ])
    )
  );
});

test('MenuButton rejects icon-only trigger bez a11y.label (named icon = aria-hidden)', () => {
  setup();
  const engine = makeEngine();
  // Same trigger_icon, brak trigger_label, brak a11y.label.
  assertThrows(() =>
    engine.render(
      comp(MENU_BUTTON_TAG, [
        [1, { kind: 'named', name: 'more' }],
        [2, 'ghost'], [3, [item('a', 'A')]], [4, 'bottom_start'],
      ])
    )
  );
});

test('MenuButton icon-only trigger z a11y.label OK', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [1, { kind: 'named', name: 'more' }],
      [2, 'ghost'], [3, [item('a', 'A')]], [4, 'bottom_start'],
    ], { a11y: { label: { kind: 'literal', value: 'Więcej opcji' } } })
  );
  assertEq(el.querySelector('.tf-menu-button__trigger').getAttribute('aria-label'), 'Więcej opcji');
});

test('MenuButton icon-only aria-label removed when a11y.label patched to whitespace', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'OK label' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(MENU_BUTTON_TAG, [
      [1, { kind: 'named', name: 'more' }],
      [2, 'ghost'], [3, [item('a', 'A')]], [4, 'bottom_start'],
    ], { a11y: { label: { kind: 'bound', path: PATH('lbl') } } })
  );
  const trigger = el.querySelector('.tf-menu-button__trigger');
  assertEq(trigger.getAttribute('aria-label'), 'OK label');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: '   ' } }],
  });
  // Whitespace-only label nie daje accessible name — atrybut usunięty.
  assertEq(trigger.hasAttribute('aria-label'), false);
});

test('Menu rejects duplicate MenuItem.id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(MENU_TAG, [
        [0, [item('x', 'A'), item('x', 'B')]],
        [1, false],
      ])
    )
  );
});

test('MenuButton rejects placement out of whitelist', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(MENU_BUTTON_TAG, [
        [0, { kind: 'literal', value: 'X' }],
        [2, 'primary'], [3, [item('a', 'A')]], [4, 'middle'],
      ])
    )
  );
});

// ============================================================================
// Menu (0x0407)
// ============================================================================

test('Menu renders <ul role=menu> bez search', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_TAG, [
      [0, [item('a', 'Alpha'), item('b', 'Beta')]],
      [1, false],
    ])
  );
  assertEq(el.querySelector('.tf-menu-standalone__search'), null);
  assertEq(el.querySelectorAll('.tf-menu__item').length, 2);
  assertEq(el.querySelector('[role=menu]').getAttribute('role'), 'menu');
});

test('Menu z search=true renderuje input + filtruje items po tekście', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_TAG, [
      [0, [item('a', 'Apple'), item('b', 'Banana'), item('c', 'Cherry')]],
      [1, true],
    ])
  );
  const input = el.querySelector('.tf-menu-standalone__search');
  assert(input != null);
  // Wstępnie wszystkie widoczne.
  const items = () => [...el.querySelectorAll('.tf-menu__item')]
    .filter((li) => !li.hasAttribute('hidden'))
    .map((li) => li.getAttribute('data-menu-item-id'));
  assertEq(items(), ['a', 'b', 'c']);
  input.value = 'an';
  input.dispatchEvent(new window.Event('input'));
  // Tylko Banana zawiera 'an'.
  assertEq(items(), ['b']);
  input.value = '';
  input.dispatchEvent(new window.Event('input'));
  assertEq(items(), ['a', 'b', 'c']);
});

test('Menu item click dispatches item_click na wrapper z detail.item_id', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(MENU_TAG, [
      [0, [item('save', 'Save')]],
      [1, false],
    ])
  );
  let received = null;
  el.addEventListener('item_click', (e) => { received = e.detail; });
  el.querySelector('.tf-menu__item').click();
  assertEq(received, { item_id: 'save' });
});

test('Menu rejects missing search field', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MENU_TAG, [[0, [item('a', 'A')]]])));
});

test('Menu rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(MENU_TAG, [[0, []], [1, false], [99, 'rogue']]))
  );
});

test('Menu rejects MenuItem z unknown key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(MENU_TAG, [
        [0, [{ id: 'a', label: { kind: 'literal', value: 'X' }, danger: false, divider_after: false, evil: true }]],
        [1, false],
      ])
    )
  );
});

test('Menu rejects MenuItem z empty id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(MENU_TAG, [
        [0, [{ id: '', label: { kind: 'literal', value: 'X' }, danger: false, divider_after: false }]],
        [1, false],
      ])
    )
  );
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
