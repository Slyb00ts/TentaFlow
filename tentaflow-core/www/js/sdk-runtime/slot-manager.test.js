// =============================================================================
// File: sdk-runtime/slot-manager.test.js
// Description: Tests for SlotManager (chunk 3.5). Covers registration,
// content/clear/show/hide handlers, state overlay, default states,
// conditional visibility, destroy, and MutationObserver auto-registration.
// =============================================================================

import './_dom-test-harness.js';

// Minimal MutationObserver polyfill for Node.js test harness. The real browser
// API fires asynchronously; this shim patches appendChild/removeChild on the
// observed root to call back synchronously, which is sufficient for unit tests.
if (typeof globalThis.MutationObserver === 'undefined') {
  globalThis.MutationObserver = class MutationObserver {
    constructor(callback) { this._cb = callback; this._root = null; this._origAppend = null; this._origRemove = null; }
    observe(root, _opts) {
      this._root = root;
      const self = this;
      const origAppend = root.appendChild;
      const origRemove = root.removeChild;
      this._origAppend = origAppend;
      this._origRemove = origRemove;
      root.appendChild = function (child) {
        const r = origAppend.call(root, child);
        self._cb([{ addedNodes: [child], removedNodes: [] }]);
        return r;
      };
      root.removeChild = function (child) {
        const r = origRemove.call(root, child);
        self._cb([{ addedNodes: [], removedNodes: [child] }]);
        return r;
      };
    }
    disconnect() {
      if (this._root && this._origAppend) {
        this._root.appendChild = this._origAppend;
        this._root.removeChild = this._origRemove;
      }
      this._root = null;
    }
  };
}

import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
  registerComponentRenderer,
} from './component-renderer.js';
import { SlotManager } from './slot-manager.js';

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

// Dummy tag for test renderer
const TEST_TAG = 0xFFF0;

function registerTestRenderer() {
  try {
    registerComponentRenderer(TEST_TAG, (component, ctx) => {
      const el = document.createElement('div');
      el.setAttribute('data-test-rendered', 'true');
      const textField = ctx.readField(component.fields, 0);
      if (textField != null) el.textContent = String(textField);
      return el;
    });
  } catch {
    // Already registered
  }
}

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}

function makeRenderer(store) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: { emit() {} },
    locale: 'en-US',
  });
}

function comp(text) {
  return {
    tag: TEST_TAG, id: 'frag1',
    fields: text != null ? [[0, text]] : [],
    handlers: null, bind: null, a11y: null,
    visibility: null, test_id: null,
  };
}

function makeSlotManager(storeOverride) {
  const store = storeOverride || makeStore();
  const renderer = makeRenderer(store);
  return { sm: new SlotManager({ store, componentRenderer: renderer }), store };
}

function setup() {
  _clearComponentRendererRegistry();
  registerTestRenderer();
  document.body.innerHTML = '';
}

// =============================================================================
// Tests
// =============================================================================

test('registerSlot stores element and hasSlot returns true', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  el.setAttribute('data-slot-id', 'my-slot');
  document.body.appendChild(el);

  sm.registerSlot('my-slot', el);

  assert(sm.hasSlot('my-slot'), 'slot should be registered');
  assertEq(sm.getSlotElement('my-slot'), el, 'getSlotElement');
  sm.destroy();
});

test('handleSlotContent renders fragment into container', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('content-slot', el);

  sm.handleSlotContent({ slot_id: 'content-slot', fragment: comp('Hello') });

  assertEq(el.children.length, 1, 'should have one child');
  assertEq(el.children[0].getAttribute('data-test-rendered'), 'true', 'child rendered');
  assertEq(el.children[0].textContent, 'Hello', 'text content');
  sm.destroy();
});

test('handleSlotClear empties container', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('clear-slot', el);

  sm.handleSlotContent({ slot_id: 'clear-slot', fragment: comp('Content') });
  assertEq(el.children.length, 1, 'should have content');

  sm.handleSlotClear({ slot_id: 'clear-slot' });
  assertEq(el.children.length, 0, 'should be empty after clear');
  sm.destroy();
});

test('handleSlotShow removes hidden attribute', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  el.setAttribute('hidden', '');
  document.body.appendChild(el);
  sm.registerSlot('show-slot', el);

  sm.handleSlotShow({ slot_id: 'show-slot' });
  assert(!el.hasAttribute('hidden'), 'hidden should be removed');
  sm.destroy();
});

test('handleSlotHide sets hidden attribute', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('hide-slot', el);

  sm.handleSlotHide({ slot_id: 'hide-slot' });
  assert(el.hasAttribute('hidden'), 'hidden should be set');
  sm.destroy();
});

test('handleSlotContent with state_overlay applies state before rendering', () => {
  setup();
  const store = makeStore();
  const renderer = makeRenderer(store);
  const sm = new SlotManager({ store, componentRenderer: renderer });
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('overlay-slot', el);

  const overlay = [
    { path: PATH('greeting'), value: 'Hi' },
  ];

  sm.handleSlotContent({
    slot_id: 'overlay-slot',
    fragment: comp('World'),
    state_overlay: overlay,
  });

  assertEq(store.read(PATH('greeting')), 'Hi', 'overlay should have been applied');
  assertEq(el.children.length, 1, 'fragment rendered');
  sm.destroy();
});

test('default_state=loading renders spinner element', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);

  sm.registerSlot('loading-slot', el, {
    default_state: { kind: 'loading' },
  });

  assertEq(el.children.length, 1, 'should have loading child');
  assert(el.children[0].classList.contains('tf-slot-loading'), 'should have loading class');
  assertEq(el.children[0].getAttribute('role'), 'status', 'a11y role');
  sm.destroy();
});

test('default_state=static renders fragment', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);

  sm.registerSlot('static-slot', el, {
    default_state: { kind: 'static', fragment: comp('Default') },
  });

  assertEq(el.children.length, 1, 'should have static child');
  assertEq(el.children[0].textContent, 'Default', 'static content');
  sm.destroy();
});

test('unregisterSlot removes slot from tracking', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('unreg-slot', el);

  assert(sm.hasSlot('unreg-slot'), 'should exist before unregister');
  sm.unregisterSlot('unreg-slot');
  assert(!sm.hasSlot('unreg-slot'), 'should not exist after unregister');
  assertEq(sm.getSlotElement('unreg-slot'), null, 'getSlotElement should be null');
  sm.destroy();
});

test('destroy clears all slots and disconnects observer', () => {
  setup();
  const { sm } = makeSlotManager();
  const el1 = document.createElement('div');
  const el2 = document.createElement('div');
  document.body.appendChild(el1);
  document.body.appendChild(el2);

  sm.registerSlot('s1', el1);
  sm.registerSlot('s2', el2);
  sm.observe(document.body);

  sm.destroy();

  assert(!sm.hasSlot('s1'), 's1 should be gone');
  assert(!sm.hasSlot('s2'), 's2 should be gone');
  assertThrows(() => sm.registerSlot('s3', el1), 'should throw after destroy');
});

test('conditional visibility subscribes to store path and toggles hidden', () => {
  setup();
  const store = makeStore();
  const renderer = makeRenderer(store);
  const sm = new SlotManager({ store, componentRenderer: renderer });
  const el = document.createElement('div');
  document.body.appendChild(el);

  // Initial state: path is undefined → falsy → hidden
  sm.registerSlot('cond-slot', el, {
    visibility: { kind: 'conditional', path: PATH('visible') },
  });
  assert(el.hasAttribute('hidden'), 'should be hidden when path is falsy');

  // Set state to truthy
  store.applySnapshot({
    entries: [{ path: PATH('visible'), value: true }],
    state_revision: 1n,
    truncated: false,
  });
  assert(!el.hasAttribute('hidden'), 'should be visible when path is truthy');

  // Set state back to falsy
  store.applySnapshot({
    entries: [{ path: PATH('visible'), value: false }],
    state_revision: 2n,
    truncated: false,
  });
  assert(el.hasAttribute('hidden'), 'should be hidden again when path is falsy');

  sm.destroy();
});

test('handleSlotClear restores default_state after clearing content', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);

  sm.registerSlot('restore-slot', el, {
    default_state: { kind: 'loading' },
  });

  // Loading spinner should be present
  assertEq(el.children.length, 1, 'loading state');

  // Push content
  sm.handleSlotContent({ slot_id: 'restore-slot', fragment: comp('Pushed') });
  assertEq(el.children.length, 1, 'content replaced loading');
  assertEq(el.children[0].textContent, 'Pushed', 'content text');

  // Clear — should restore loading
  sm.handleSlotClear({ slot_id: 'restore-slot' });
  assertEq(el.children.length, 1, 'loading restored');
  assert(el.children[0].classList.contains('tf-slot-loading'), 'loading class restored');

  sm.destroy();
});

test('MutationObserver auto-registers elements with data-slot-id', () => {
  setup();
  const { sm } = makeSlotManager();
  sm.observe(document.body);

  const el = document.createElement('div');
  el.setAttribute('data-slot-id', 'auto-slot');
  document.body.appendChild(el);

  assert(sm.hasSlot('auto-slot'), 'auto-registered slot should exist');

  // Remove from DOM
  document.body.removeChild(el);

  assert(!sm.hasSlot('auto-slot'), 'auto-unregistered slot should be gone');

  sm.destroy();
});

test('observe picks up existing data-slot-id elements', () => {
  setup();
  const { sm } = makeSlotManager();

  const el = document.createElement('div');
  el.setAttribute('data-slot-id', 'pre-existing');
  document.body.appendChild(el);

  sm.observe(document.body);

  assert(sm.hasSlot('pre-existing'), 'pre-existing slot should be registered');
  assertEq(sm.getSlotElement('pre-existing'), el, 'element matches');

  sm.destroy();
});

test('constructor rejects missing store or componentRenderer', () => {
  assertThrows(() => new SlotManager({}), 'missing store');
  assertThrows(
    () => new SlotManager({ store: { subscribe() {} } }),
    'missing componentRenderer'
  );
});

test('visibility kind=hidden sets hidden immediately on register', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);

  sm.registerSlot('hidden-slot', el, {
    visibility: { kind: 'hidden' },
  });

  assert(el.hasAttribute('hidden'), 'should be hidden immediately');
  sm.destroy();
});

test('handleSlotContent replaces previous content', () => {
  setup();
  const { sm } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('replace-slot', el);

  sm.handleSlotContent({ slot_id: 'replace-slot', fragment: comp('First') });
  assertEq(el.children.length, 1, 'first content');
  assertEq(el.children[0].textContent, 'First', 'first text');

  sm.handleSlotContent({ slot_id: 'replace-slot', fragment: comp('Second') });
  assertEq(el.children.length, 1, 'still one child');
  assertEq(el.children[0].textContent, 'Second', 'replaced text');

  sm.destroy();
});

// =============================================================================
// Report
// =============================================================================

const passed = results.filter((r) => r.ok).length;
const failed = results.filter((r) => !r.ok);
console.log(`\nSlotManager tests: ${passed}/${results.length} passed`);
for (const f of failed) {
  console.error(`  FAIL: ${f.name}`);
  console.error(`    ${f.err.message}`);
  if (f.err.stack) console.error(`    ${f.err.stack.split('\n').slice(1, 3).join('\n    ')}`);
}
if (failed.length > 0) process.exit(1);
