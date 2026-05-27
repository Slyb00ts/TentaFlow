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
function clickElement(el) {
  el.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function ensureComponentStubs() {
  if (!customElements.get('tf-button')) {
    customElements.define('tf-button', class extends HTMLElement {});
  }
  if (!customElements.get('tf-input')) {
    customElements.define('tf-input', class extends HTMLElement {});
  }
  if (!customElements.get('tf-menu')) {
    customElements.define('tf-menu', class extends HTMLElement {
      connectedCallback() {
        this._onKey = (e) => {
          if (e.key === 'Escape') this.close();
        };
        document.addEventListener('keydown', this._onKey);
      }
      disconnectedCallback() {
        document.removeEventListener('keydown', this._onKey);
      }
      open() { this.setAttribute('open', ''); }
      close() { this.removeAttribute('open'); }
    });
  }
  if (!customElements.get('tf-menu-item')) {
    customElements.define('tf-menu-item', class extends HTMLElement {
      connectedCallback() {
        this.addEventListener('click', (e) => {
          if (this.hasAttribute('disabled')) {
            e.preventDefault();
            e.stopPropagation();
            return;
          }
          this.dispatchEvent(new CustomEvent('tf-menu-select', {
            bubbles: true,
            detail: { action: this.getAttribute('action') || '' },
          }));
        });
      }
    });
  }
  if (!customElements.get('tf-menu-divider')) {
    customElements.define('tf-menu-divider', class extends HTMLElement {});
  }
}

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
  ensureComponentStubs();
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}
function renderMounted(engine, component) {
  const el = engine.render(component);
  document.body.appendChild(el);
  return el;
}

function item(id, label, extra = {}) {
  const fields = [
    [0, id],
    [1, { kind: 'literal', value: label }],
    [5, extra.danger ?? false],
    [7, extra.divider_after ?? false],
  ];
  if (extra.icon != null) fields.push([2, extra.icon]);
  if (extra.shortcut != null) fields.push([4, extra.shortcut]);
  if (extra.disabled != null) fields.push([6, extra.disabled]);
  return fields;
}

// ============================================================================
// MenuButton (0x0406)
// ============================================================================

test('MenuButton renders trigger + hidden popup z aria attrs', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'Akcje' }],
      [2, 'primary'],
      [3, [item('save', 'Zapisz'), item('delete', 'Usuń')]],
      [4, 'bottom_start'],
    ])
  );
  const trigger = el.querySelector('tf-button');
  assertEq(trigger.tagName, 'TF-BUTTON');
  assertEq(trigger.getAttribute('label'), 'Akcje');
  const popup = el.querySelector('tf-menu');
  assert(!popup.hasAttribute('open'));
  assertEq(popup.querySelectorAll('tf-menu-item').length, 2);
});

test('MenuButton trigger click toggles popup visibility', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'ghost'], [3, [item('a', 'A')]], [4, 'bottom_end'],
    ])
  );
  const trigger = el.querySelector('tf-button');
  const popup = el.querySelector('tf-menu');
  clickElement(trigger);
  assert(popup.hasAttribute('open'));
  clickElement(trigger);
  assert(!popup.hasAttribute('open'));
});

test('MenuButton item click dispatches item_click + closes popup', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'], [3, [item('save', 'Save')]], [4, 'bottom_start'],
    ])
  );
  let received = null;
  el.addEventListener('item_click', (e) => { received = e.detail; });
  const trigger = el.querySelector('tf-button');
  clickElement(trigger); // open
  const item1 = el.querySelector('tf-menu-item');
  clickElement(item1);
  assertEq(received, { item_id: 'save' });
  assert(!el.querySelector('tf-menu').hasAttribute('open'));
});

test('MenuButton Escape key zamyka popup', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'], [3, [item('a', 'A')]], [4, 'bottom_start'],
    ])
  );
  clickElement(el.querySelector('tf-button'));
  document.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(!el.querySelector('tf-menu').hasAttribute('open'));
});

test('MenuButton item z shortcut renderuje span tf-menu__item-shortcut', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('save', 'Save', { shortcut: 'Ctrl+S' })]],
      [4, 'bottom_start'],
    ])
  );
  const itemEl = el.querySelector('tf-menu-item');
  assertEq(itemEl.getAttribute('shortcut'), 'Ctrl+S');
});

test('MenuButton item label traktuje HTML jako tekst', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('attack', '<img src=x onerror=alert(1)>')]],
      [4, 'bottom_start'],
    ])
  );
  const menuItem = el.querySelector('tf-menu-item');
  assertEq(menuItem.textContent, '<img src=x onerror=alert(1)>');
  assertEq(menuItem.querySelector('img'), null);
});

test('MenuButton trigger label traktuje HTML jako tekst', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: '<img src=x onerror=alert(1)>' }],
      [2, 'primary'],
      [3, [item('safe', 'Safe')]],
      [4, 'bottom_start'],
    ])
  );
  const trigger = el.querySelector('tf-button');
  assertEq(trigger.getAttribute('label'), '<img src=x onerror=alert(1)>');
});

test('MenuButton item danger=true → tf-menu__item--danger', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('rm', 'Delete', { danger: true })]],
      [4, 'bottom_start'],
    ])
  );
  assert(el.querySelector('tf-menu-item[danger]') != null);
});

test('MenuButton item disabled reactive blocks click', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('d'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [2, 'primary'],
      [3, [item('save', 'Save', { disabled: { kind: 'bound', path: PATH('d') } })]],
      [4, 'bottom_start'],
    ])
  );
  let received = null;
  el.addEventListener('item_click', (e) => { received = e.detail; });
  clickElement(el.querySelector('tf-button'));
  const it = el.querySelector('tf-menu-item');
  assertEq(it.hasAttribute('disabled'), true);
  clickElement(it);
  assertEq(received, null);
  // Po wyłączeniu disabled item powinien działać.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('d'), op: { kind: 'set', value: false } }],
  });
  clickElement(it);
  assertEq(received, { item_id: 'save' });
});

test('MenuButton item divider_after renderuje li role=separator', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
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
  assert(el.querySelector('tf-menu-divider') != null);
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
  const el = renderMounted(engine,
    comp(MENU_BUTTON_TAG, [
      [1, { kind: 'named', name: 'more' }],
      [2, 'ghost'], [3, [item('a', 'A')]], [4, 'bottom_start'],
    ], { a11y: { label: { kind: 'literal', value: 'Więcej opcji' } } })
  );
  assertEq(el.querySelector('tf-button').getAttribute('aria-label'), 'Więcej opcji');
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
  const trigger = el.querySelector('tf-button');
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
  const el = renderMounted(engine,
    comp(MENU_TAG, [
      [0, [item('a', 'Alpha'), item('b', 'Beta')]],
      [1, false],
    ])
  );
  assertEq(el.querySelector('.tf-menu-standalone__search'), null);
  assertEq(el.querySelectorAll('tf-menu-item').length, 2);
  assertEq(el.querySelector('tf-menu').hasAttribute('open'), true);
});

test('Menu z search=true renderuje input + filtruje items po tekście', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_TAG, [
      [0, [item('a', 'Apple'), item('b', 'Banana'), item('c', 'Cherry')]],
      [1, true],
    ])
  );
  const input = el.querySelector('.tf-menu-standalone__search');
  assert(input != null);
  // Wstępnie wszystkie widoczne.
  const items = () => [...el.querySelectorAll('tf-menu-item')]
    .filter((li) => !li.hasAttribute('hidden'))
    .map((li) => li.getAttribute('action'));
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
  const el = renderMounted(engine,
    comp(MENU_TAG, [
      [0, [item('save', 'Save')]],
      [1, false],
    ])
  );
  let received = null;
  el.addEventListener('item_click', (e) => { received = e.detail; });
  clickElement(el.querySelector('tf-menu-item'));
  assertEq(received, { item_id: 'save' });
});

test('Menu item label traktuje HTML jako tekst', () => {
  setup();
  const engine = makeEngine();
  const el = renderMounted(engine,
    comp(MENU_TAG, [
      [0, [item('attack', '<svg onload=alert(1)>')]],
      [1, false],
    ])
  );
  const menuItem = el.querySelector('tf-menu-item');
  assertEq(menuItem.textContent, '<svg onload=alert(1)>');
  assertEq(menuItem.querySelector('svg'), null);
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
