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

test('observe auto-registers dynamic slot so later handleSlotContent renders into it', () => {
  setup();
  const { sm } = makeSlotManager();

  // Static declared slot exists before observe (mirrors handlePanelShell order).
  const staticSlot = document.createElement('div');
  staticSlot.setAttribute('data-slot-id', 'content');
  document.body.appendChild(staticSlot);
  sm.registerSlot('content', staticSlot);

  sm.observe(document.body);

  // Before observe wires the dynamic slot, handleSlotContent for it warns+returns.
  assert(!sm.hasSlot('modal-body-1'), 'dynamic slot absent before insertion');

  // Overlay renderer creates a dynamic container with a new data-slot-id
  // (e.g. modal body slot) under the observed root.
  const modalBody = document.createElement('div');
  modalBody.setAttribute('data-slot-id', 'modal-body-1');
  document.body.appendChild(modalBody);

  assert(sm.hasSlot('modal-body-1'), 'dynamic slot should be auto-registered');

  // A later SlotContent for the dynamic slot must render into it, not warn+return.
  sm.handleSlotContent({ slot_id: 'modal-body-1', fragment: comp('Modal body') });
  assertEq(modalBody.children.length, 1, 'dynamic slot received content');
  assertEq(modalBody.children[0].textContent, 'Modal body', 'dynamic slot content text');

  // Static slot still works (no regression).
  sm.handleSlotContent({ slot_id: 'content', fragment: comp('Static') });
  assert(staticSlot.querySelector('[data-test-rendered]'), 'static slot still renders');

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

test('handleSlotContent buffers content for unregistered slot and replays on registerSlot', () => {
  setup();
  const { sm } = makeSlotManager();

  // Content arrives before the dynamic overlay container is registered.
  sm.handleSlotContent({ slot_id: 'add_camera_body', fragment: comp('Buffered body') });
  assert(!sm.hasSlot('add_camera_body'), 'slot must still be absent (only buffered)');

  // Later the overlay renderer's container is registered explicitly.
  const el = document.createElement('div');
  el.setAttribute('data-slot-id', 'add_camera_body');
  document.body.appendChild(el);
  sm.registerSlot('add_camera_body', el);

  assertEq(el.children.length, 1, 'buffered content replayed into container');
  assertEq(el.children[0].textContent, 'Buffered body', 'replayed content text');

  sm.destroy();
});

test('buffered content replays when observe auto-registers a dynamic slot', () => {
  setup();
  const { sm } = makeSlotManager();
  sm.observe(document.body);

  // SlotContent for the dynamic overlay footer arrives before its DOM node.
  sm.handleSlotContent({ slot_id: 'add_camera_footer', fragment: comp('Buffered footer') });
  assert(!sm.hasSlot('add_camera_footer'), 'slot absent until container inserted');

  // Overlay renderer inserts the container; MutationObserver auto-registers it.
  const footer = document.createElement('div');
  footer.setAttribute('data-slot-id', 'add_camera_footer');
  document.body.appendChild(footer);

  assert(sm.hasSlot('add_camera_footer'), 'dynamic slot auto-registered');
  assertEq(footer.children.length, 1, 'buffered content replayed on auto-register');
  assertEq(footer.children[0].textContent, 'Buffered footer', 'replayed footer text');

  sm.destroy();
});

test('repeated SlotContent before registration overwrites pending (last wins)', () => {
  setup();
  const { sm } = makeSlotManager();

  sm.handleSlotContent({ slot_id: 'pending-slot', fragment: comp('First') });
  sm.handleSlotContent({ slot_id: 'pending-slot', fragment: comp('Second') });

  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('pending-slot', el);

  assertEq(el.children.length, 1, 'single child after replay');
  assertEq(el.children[0].textContent, 'Second', 'only the latest pending content replays');

  sm.destroy();
});

test('buffered state_overlay is applied when pending content replays', () => {
  setup();
  const store = makeStore();
  const renderer = makeRenderer(store);
  const sm = new SlotManager({ store, componentRenderer: renderer });

  sm.handleSlotContent({
    slot_id: 'overlay-pending',
    fragment: comp('Body'),
    state_overlay: [{ path: PATH('title'), value: 'Add camera' }],
  });
  assertEq(store.read(PATH('title')), undefined, 'overlay deferred until replay');

  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('overlay-pending', el);

  assertEq(store.read(PATH('title')), 'Add camera', 'overlay applied on replay');
  assertEq(el.children.length, 1, 'fragment rendered on replay');

  sm.destroy();
});

test('destroy clears pending content buffer', () => {
  setup();
  const { sm } = makeSlotManager();

  sm.handleSlotContent({ slot_id: 'leaky-slot', fragment: comp('Stale') });
  sm.destroy();

  // A fresh manager (mirrors panel rebuild) must not inherit the stale buffer.
  const { sm: sm2 } = makeSlotManager();
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm2.registerSlot('leaky-slot', el);

  assertEq(el.children.length, 0, 'no stale content leaked into rebuilt panel');

  sm2.destroy();
});

test('unregisterSlot drops pending content so a removed overlay does not replay', () => {
  setup();
  const { sm } = makeSlotManager();

  // Register, then buffer fresh content arriving after the container was gone.
  const el = document.createElement('div');
  document.body.appendChild(el);
  sm.registerSlot('overlay-slot', el);
  sm.unregisterSlot('overlay-slot');

  sm.handleSlotContent({ slot_id: 'overlay-slot', fragment: comp('Late') });
  // Overlay container removed again before it ever re-registers.
  sm.unregisterSlot('overlay-slot');

  const el2 = document.createElement('div');
  document.body.appendChild(el2);
  sm.registerSlot('overlay-slot', el2);

  assertEq(el2.children.length, 0, 'pending dropped on unregister, no replay');

  sm.destroy();
});

// =============================================================================
// Reconcile (in-place patch on repeated SlotContent)
// =============================================================================
//
// These tests prove the slot manager patches the existing DOM in place instead
// of tearing it down on every SlotContent, so focus/scroll/typed values and
// live store subscriptions survive an addon re-pushing a near-identical
// fragment.

const STACK_TAG = 0x0103;
const TEXT_TAG = 0xFFF1; // leaf renderer that reflects field[0] as textContent
const LEAF_INPUT_TAG = 0xFFF2; // leaf renderer that subscribes to a store path

// Count live subscriptions so we can assert no leak / no double-register.
let leafSubCount = 0;

function registerReconcileRenderers() {
  // Real Stack renderer would import layout-containers; we register a minimal
  // transparent-container stand-in under the SAME tag (0x0103) so
  // transparentContainerChildKey(0x0103)===2 applies. Children live in field 2.
  try {
    registerComponentRenderer(STACK_TAG, (component, ctx) => {
      const el = document.createElement('div');
      el.classList.add('tf-stack');
      // Reflect padding (field 3) into a class so a shell change is observable.
      const padding = ctx.readField(component.fields, 3);
      if (padding != null) el.classList.add(`tf-stack--padding-${padding}`);
      const children = ctx.readField(component.fields, 2) || [];
      for (const child of children) el.appendChild(ctx.renderChild(child));
      return el;
    });
  } catch { /* already registered */ }

  try {
    // Text leaf whose content is bound to a store path (field 0). This mirrors
    // the production path: declarative text reacts to the store, so re-pushing
    // an unchanged fragment leaves the node alone and the text updates in place.
    registerComponentRenderer(TEXT_TAG, (component, ctx) => {
      const el = document.createElement('span');
      const path = ctx.readField(component.fields, 0);
      const apply = () => {
        let v;
        try { v = ctx.store.read(path); } catch { v = undefined; }
        el.textContent = v == null ? '' : String(v);
      };
      apply();
      ctx.registerCleanup(ctx.store.subscribe(path, apply));
      return el;
    });
  } catch { /* already registered */ }

  try {
    registerComponentRenderer(LEAF_INPUT_TAG, (component, ctx) => {
      const el = document.createElement('input');
      const path = ctx.readField(component.fields, 0);
      const apply = () => {
        let v;
        try { v = ctx.store.read(path); } catch { v = undefined; }
        const next = v == null ? '' : String(v);
        // Mirror the real input contract: do not clobber a focused field.
        if (el.value !== next && document.activeElement !== el) el.value = next;
      };
      apply();
      const off = ctx.store.subscribe(path, apply);
      leafSubCount += 1;
      ctx.registerCleanup(() => { leafSubCount -= 1; off(); });
      return el;
    });
  } catch { /* already registered */ }
}

function setupReconcile() {
  _clearComponentRendererRegistry();
  leafSubCount = 0;
  registerReconcileRenderers();
  document.body.innerHTML = '';
}

// Text leaf bound to a store path (field 0). Same path → structurally
// identical component → reconcile reuses the node and content tracks the store.
const text = (path) => ({
  tag: TEXT_TAG, id: 't',
  fields: [[0, path]],
  handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
});

const leafInput = (path) => ({
  tag: LEAF_INPUT_TAG, id: 'in',
  fields: [[0, path]],
  handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
});

const stack = (children, padding) => ({
  tag: STACK_TAG, id: 'stk',
  fields: padding != null ? [[2, children], [3, padding]] : [[2, children]],
  handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
});

test('reconcile: bound text patch reuses the same DOM node (no rebuild)', () => {
  setupReconcile();
  const { sm, store } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  sm.handleSlotContent({
    slot_id: 's',
    fragment: stack([text(PATH('label'))]),
    state_overlay: [{ path: PATH('label'), value: 'A' }],
  });
  const rootBefore = host.firstChild;
  const childBefore = rootBefore.firstChild;
  assertEq(childBefore.textContent, 'A', 'initial bound text');

  // Re-push the SAME fragment with a new store value via overlay. The
  // component is structurally identical, so reconcile leaves the node alone
  // and its live subscription patches the text in place.
  sm.handleSlotContent({
    slot_id: 's',
    fragment: stack([text(PATH('label'))]),
    state_overlay: [{ path: PATH('label'), value: 'B' }],
  });
  assert(host.firstChild === rootBefore, 'container element reused');
  assert(rootBefore.firstChild === childBefore, 'child element reused (not replaced)');
  assertEq(childBefore.textContent, 'B', 'bound text patched in place');
  sm.destroy();
});

test('reconcile: focused input keeps focus and value across re-push', () => {
  setupReconcile();
  const { sm, store } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  const frag = stack([leafInput(PATH('q'))]);
  sm.handleSlotContent({ slot_id: 's', fragment: frag });
  const input = host.firstChild.firstChild;
  assert(input.tagName === 'INPUT', 'input rendered');

  // User focuses and types — simulate the in-progress value + focus.
  input.focus();
  input.value = 'typed-by-user';
  assert(document.activeElement === input, 'input is focused');

  // Addon re-pushes the SAME fragment (structurally identical leaf).
  sm.handleSlotContent({ slot_id: 's', fragment: stack([leafInput(PATH('q'))]) });

  assert(host.firstChild.firstChild === input, 'input DOM node reused');
  assert(document.activeElement === input, 'focus preserved');
  assertEq(input.value, 'typed-by-user', 'typed value preserved');
  // Still exactly one live subscription — no leak, no double-register.
  assertEq(leafSubCount, 1, 'subscription not duplicated for unchanged leaf');
  sm.destroy();
});

test('reconcile: tag change at a position replaces that element', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  sm.handleSlotContent({ slot_id: 's', fragment: stack([text(PATH('x'))]) });
  const childBefore = host.firstChild.firstChild;
  assertEq(childBefore.tagName, 'SPAN', 'first child is span');

  sm.handleSlotContent({ slot_id: 's', fragment: stack([leafInput(PATH('q'))]) });
  const childAfter = host.firstChild.firstChild;
  assert(childAfter !== childBefore, 'changed-tag child was replaced');
  assertEq(childAfter.tagName, 'INPUT', 'replacement is input');
  assertEq(leafSubCount, 1, 'new leaf subscription registered exactly once');
  sm.destroy();
});

test('reconcile: appending/removing a tail child keeps the surviving nodes', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  const two = () => [text(PATH('one')), text(PATH('two'))];
  sm.handleSlotContent({ slot_id: 's', fragment: stack(two()) });
  const root = host.firstChild;
  const c0 = root.children[0];
  const c1 = root.children[1];

  // Add a third child at the tail.
  sm.handleSlotContent({
    slot_id: 's',
    fragment: stack([text(PATH('one')), text(PATH('two')), text(PATH('three'))]),
    state_overlay: [{ path: PATH('three'), value: 'three' }],
  });
  assert(root.children[0] === c0, 'first child unchanged');
  assert(root.children[1] === c1, 'second child unchanged');
  assertEq(root.children.length, 3, 'third child appended');
  assertEq(root.children[2].textContent, 'three', 'appended text');

  // Remove the tail child again.
  sm.handleSlotContent({ slot_id: 's', fragment: stack(two()) });
  assert(root.children[0] === c0, 'first child still unchanged after removal');
  assert(root.children[1] === c1, 'second child still unchanged after removal');
  assertEq(root.children.length, 2, 'tail child removed');
  sm.destroy();
});

test('reconcile: removed-child subscription is freed (no leak)', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  // Two inputs on distinct paths → two live subscriptions.
  sm.handleSlotContent({
    slot_id: 's',
    fragment: stack([leafInput(PATH('a')), leafInput(PATH('b'))]),
  });
  assertEq(leafSubCount, 2, 'two subscriptions live');

  // Drop the second input — its subscription must be released.
  sm.handleSlotContent({ slot_id: 's', fragment: stack([leafInput(PATH('a'))]) });
  assertEq(leafSubCount, 1, 'removed child subscription freed');

  // Re-adding it registers exactly one more (no stale double from the prior).
  sm.handleSlotContent({
    slot_id: 's',
    fragment: stack([leafInput(PATH('a')), leafInput(PATH('b'))]),
  });
  assertEq(leafSubCount, 2, 'no double-register after re-add');
  sm.destroy();
  assertEq(leafSubCount, 0, 'destroy frees all subscriptions');
});

test('reconcile: changed container shell replaces the whole subtree', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  sm.handleSlotContent({ slot_id: 's', fragment: stack([leafInput(PATH('a'))]) });
  const rootBefore = host.firstChild;
  assert(!rootBefore.classList.contains('tf-stack--padding-lg'), 'no padding yet');
  assertEq(leafSubCount, 1, 'one live sub');

  // Padding (shell field) changed → wrapper class differs → full replace,
  // old subtree destroyed, fresh one rendered.
  sm.handleSlotContent({ slot_id: 's', fragment: stack([leafInput(PATH('a'))], 'lg') });
  const rootAfter = host.firstChild;
  assert(rootAfter !== rootBefore, 'container replaced on shell change');
  assert(rootAfter.classList.contains('tf-stack--padding-lg'), 'new shell class applied');
  assertEq(leafSubCount, 1, 'old sub freed, new sub registered (net one)');
  sm.destroy();
});

test('reconcile: rejects fragment with duplicate FieldMap key (no stale accept)', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  // First a valid fragment so the reconcile path (reuse wrapper) is taken on
  // the next push.
  const good = stack([text(PATH('a'))]);
  sm.handleSlotContent({ slot_id: 's', fragment: good });
  const rootBefore = host.firstChild;
  const childBefore = rootBefore.firstChild;

  // Same tag (so canReconcile=true) but a duplicate FieldMap key. Without
  // validation fieldsToMap would silently merge duplicates and patch ahead.
  const dupKey = {
    tag: STACK_TAG, id: 'stk',
    fields: [[2, [text(PATH('a'))]], [2, [text(PATH('b'))]]],
    handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
  };
  assertThrows(
    () => sm.handleSlotContent({ slot_id: 's', fragment: dupKey }),
    'duplicate FieldMap key must be rejected'
  );
  // DOM untouched and currentFragment still the last GOOD fragment.
  assert(host.firstChild === rootBefore, 'root not replaced on rejected fragment');
  assert(rootBefore.firstChild === childBefore, 'child not patched on rejected fragment');
  assert(sm._slots.get('s').currentFragment === good, 'currentFragment unchanged (still last good)');
  sm.handleSlotContent({ slot_id: 's', fragment: stack([text(PATH('a'))]) });
  assert(host.firstChild === rootBefore, 'reconcile still works against last good fragment');
  sm.destroy();
});

test('reconcile: rejects fragment with malformed FieldMap entry shape', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  sm.handleSlotContent({ slot_id: 's', fragment: stack([text(PATH('a'))]) });
  const rootBefore = host.firstChild;

  // Same tag, but a FieldMap entry is not a [u8, Value] pair.
  const badShape = {
    tag: STACK_TAG, id: 'stk',
    fields: [[2, [text(PATH('a'))]], ['not-a-u8-key', 'x', 'extra']],
    handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
  };
  assertThrows(
    () => sm.handleSlotContent({ slot_id: 's', fragment: badShape }),
    'malformed FieldMap entry must be rejected'
  );
  assert(host.firstChild === rootBefore, 'root not replaced on rejected fragment');
  sm.destroy();
});

test('reconcile: rejects nested child with duplicate FieldMap key', () => {
  setupReconcile();
  const { sm } = makeSlotManager();
  const host = document.createElement('div');
  document.body.appendChild(host);
  sm.registerSlot('s', host);

  sm.handleSlotContent({ slot_id: 's', fragment: stack([text(PATH('a'))]) });
  const rootBefore = host.firstChild;

  // Valid container shell, but a CHILD has a duplicate FieldMap key. Validation
  // must recurse into transparent-container children.
  const badChild = {
    tag: TEXT_TAG, id: 't',
    fields: [[0, PATH('a')], [0, PATH('b')]],
    handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
  };
  const fragment = {
    tag: STACK_TAG, id: 'stk',
    fields: [[2, [badChild]]],
    handlers: null, bind: null, a11y: null, visibility: null, test_id: null,
  };
  assertThrows(
    () => sm.handleSlotContent({ slot_id: 's', fragment }),
    'duplicate key in nested child must be rejected'
  );
  assert(host.firstChild === rootBefore, 'root not replaced on rejected nested child');
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
