// =============================================================================
// File: sdk-runtime/feedback-overlay-renderer.test.js
// Description: Tests for Modal (0x0509), Drawer (0x050A), Popover (0x050B),
// Sheet (0x050C), GateScreen (0x050D), ConfirmationDialog (0x050E) —
// chunk 3.3e-3. Dialog overlays render through the <tf-modal> web component,
// which is imported after the DOM harness so elements upgrade for real
// ESC/backdrop/close behavior.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-modal.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  MODAL_TAG, DRAWER_TAG, POPOVER_TAG, SHEET_TAG,
  GATE_SCREEN_TAG, CONFIRMATION_DIALOG_TAG,
} from './feedback-overlay-renderer.js';

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
// Modal
// ============================================================================

function modalFields({
  title = LIT('Modal Title'), subtitle = null, bodySlot = 'body-1',
  footerSlot = null, size = 'md', dismissible = false,
  preventScroll = false, closable = false, icon = null,
} = {}) {
  const f = [[0, title], [2, bodySlot], [4, size], [5, dismissible], [6, preventScroll], [7, closable]];
  if (subtitle != null) f.push([1, subtitle]);
  if (footerSlot != null) f.push([3, footerSlot]);
  if (icon != null) f.push([8, icon]);
  return f;
}

test('Modal renders as open tf-modal with size class on card', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ size: 'lg' })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-MODAL');
  assertEq(el.getAttribute('variant'), 'modal');
  assertEq(el.getAttribute('size'), 'lg');
  assert(el.hasAttribute('open'));
  const card = el.querySelector('.tf-modal-card');
  assert(card != null);
  assertEq(card.getAttribute('role'), 'dialog');
  assertEq(card.getAttribute('aria-modal'), 'true');
  assert(card.classList.contains('tf-modal--size-lg'));
});

test('Modal renders title via BindRef', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ title: LIT('My Modal') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'My Modal');
  assertEq(el.querySelector('.tf-modal-title').textContent, 'My Modal');
});

test('Modal title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('mt'), value: 'Old' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(MODAL_TAG, modalFields({ title: BOUND('mt') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Old');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('mt'), op: { kind: 'set', value: 'New' } }] });
  assertEq(el.getAttribute('title'), 'New');
});

test('Modal renders body slot with data-slot-id inside tf-modal body', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ bodySlot: 'modal-body' })));
  document.body.appendChild(el);
  const slotEl = el.querySelector('.tf-modal-body [data-slot-id="modal-body"]');
  assert(slotEl != null);
});

test('Modal renders header icon before the title when Modal.icon set', () => {
  setup();
  const engine = makeEngine(makeStore());
  const icon = { kind: 'named', name: 'share', size: null, tone: null };
  const el = engine.render(comp(MODAL_TAG, modalFields({ icon })));
  document.body.appendChild(el);
  const header = el.querySelector('.tf-modal-header');
  const iconEl = header.querySelector('.tf-modal-title-icon');
  assert(iconEl != null, 'header icon rendered');
  // Icon precedes the title in DOM order.
  const kids = [...header.children];
  const titleEl = header.querySelector('.tf-modal-title');
  assert(kids.indexOf(iconEl) < kids.indexOf(titleEl), 'icon must come before the title');
});

test('Modal without icon has no header icon', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields()));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-modal-title-icon') == null);
});

test('Modal renders footer slot when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ footerSlot: 'modal-footer' })));
  document.body.appendChild(el);
  const slotEl = el.querySelector('.tf-modal-footer [data-slot-id="modal-footer"]');
  assert(slotEl != null);
});

test('Modal closable=true shows close button dispatching dismiss', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ closable: true })));
  document.body.appendChild(el);
  assert(!el.hasAttribute('no-close'));
  const closeBtn = el.querySelector('.tf-modal-close');
  assert(closeBtn != null);
  assert(closeBtn.style.display !== 'none');
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  closeBtn.click();
  assert(dismissed);
});

test('Modal closable=false hides close button', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ closable: false })));
  document.body.appendChild(el);
  assert(el.hasAttribute('no-close'));
  assertEq(el.querySelector('.tf-modal-close').style.display, 'none');
});

test('Modal dismissible=true dispatches dismiss on ESC', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ dismissible: true })));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(dismissed);
});

test('Modal dismissible=true dispatches dismiss on backdrop click', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ dismissible: true })));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  el.querySelector('.tf-modal-backdrop').click();
  assert(dismissed);
});

test('Modal dismissible=false ignores ESC and backdrop', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ dismissible: false })));
  document.body.appendChild(el);
  assert(el.hasAttribute('no-dismiss'));
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  el.querySelector('.tf-modal-backdrop').click();
  assert(!dismissed);
});

test('Modal rejects missing title', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(MODAL_TAG, [[2, 'body'], [4, 'md'], [5, false], [6, false], [7, false]])));
});

test('Modal rejects invalid size', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(MODAL_TAG, modalFields({ size: 'huge' }))));
});

test('Modal renders subtitle when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ subtitle: LIT('Sub') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('subtitle'), 'Sub');
  assertEq(el.querySelector('.tf-modal-subtitle').textContent, 'Sub');
});

test('Modal preventScroll adds class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ preventScroll: true })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-modal--prevent-scroll'));
});

// ============================================================================
// Drawer
// ============================================================================

function drawerFields({
  side = 'right', size = 'md', title = null, bodySlot = 'drawer-body',
  footerSlot = null, dismissible = false,
} = {}) {
  const f = [[0, side], [1, size], [3, bodySlot], [5, dismissible]];
  if (title != null) f.push([2, title]);
  if (footerSlot != null) f.push([4, footerSlot]);
  return f;
}

test('Drawer renders as tf-modal with drawer variant and size', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ side: 'left', size: 'lg' })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-MODAL');
  assertEq(el.getAttribute('variant'), 'drawer-left');
  assertEq(el.getAttribute('size'), 'lg');
  assert(el.hasAttribute('open'));
  assert(el.hasAttribute('no-close'));
  const card = el.querySelector('.tf-modal-card');
  assert(card.classList.contains('tf-modal-card--drawer-left'));
  assertEq(card.getAttribute('role'), 'dialog');
});

test('Drawer side=top maps to drawer-top variant', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ side: 'top' })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('variant'), 'drawer-top');
  assert(el.querySelector('.tf-modal-card').classList.contains('tf-modal-card--drawer-top'));
});

test('Drawer renders body slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ bodySlot: 'dr-body' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-modal-body [data-slot-id="dr-body"]') != null);
});

test('Drawer renders title when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ title: LIT('Settings') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Settings');
  assertEq(el.querySelector('.tf-modal-title').textContent, 'Settings');
});

test('Drawer title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('dt'), value: 'First' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ title: BOUND('dt') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'First');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('dt'), op: { kind: 'set', value: 'Second' } }] });
  assertEq(el.getAttribute('title'), 'Second');
});

test('Drawer dismissible=true dispatches dismiss on ESC', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ dismissible: true })));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(dismissed);
});

test('Drawer dismissible=false sets no-dismiss', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ dismissible: false })));
  document.body.appendChild(el);
  assert(el.hasAttribute('no-dismiss'));
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(!dismissed);
});

test('Drawer rejects invalid side', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(DRAWER_TAG, drawerFields({ side: 'center' }))));
});

test('Drawer renders footer slot when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ footerSlot: 'dr-footer' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-modal-footer [data-slot-id="dr-footer"]') != null);
});

// ============================================================================
// Popover
// ============================================================================

function popoverFields({
  anchorId = 'anchor-1', bodySlot = 'pop-body', placement = 'bottom',
  dismissible = false, arrow = false,
} = {}) {
  return [[0, anchorId], [1, bodySlot], [2, placement], [3, dismissible], [4, arrow]];
}

test('Popover renders with placement class and anchor data attr', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(POPOVER_TAG, popoverFields({ placement: 'top_end' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-popover'));
  assert(el.classList.contains('tf-popover--placement-top_end'));
  assertEq(el.getAttribute('data-anchor-id'), 'anchor-1');
});

test('Popover renders body slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(POPOVER_TAG, popoverFields({ bodySlot: 'pop-content' })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-popover__body').getAttribute('data-slot-id'), 'pop-content');
});

test('Popover arrow=true adds arrow element', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(POPOVER_TAG, popoverFields({ arrow: true })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-popover__arrow') != null);
});

test('Popover arrow=false has no arrow element', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(POPOVER_TAG, popoverFields({ arrow: false })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-popover__arrow') == null);
});

test('Popover dismissible=true dispatches dismiss on ESC', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(POPOVER_TAG, popoverFields({ dismissible: true })));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(dismissed);
});

test('Popover rejects invalid placement', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(POPOVER_TAG, popoverFields({ placement: 'center' }))));
});

test('Popover rejects missing anchor_id', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(POPOVER_TAG, [[1, 'body'], [2, 'bottom'], [3, false], [4, false]])));
});

// ============================================================================
// Sheet
// ============================================================================

function sheetFields({
  title = null, bodySlot = 'sheet-body', footerSlot = null,
  detents = ['medium'], currentDetent = null, dismissible = false,
} = {}) {
  const f = [[1, bodySlot], [3, detents], [5, dismissible]];
  if (title != null) f.push([0, title]);
  if (footerSlot != null) f.push([2, footerSlot]);
  if (currentDetent != null) f.push([4, currentDetent]);
  return f;
}

test('Sheet renders as bottom drawer tf-modal with detent class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields({ detents: ['large'] })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-MODAL');
  assertEq(el.getAttribute('variant'), 'drawer-bottom');
  assert(el.hasAttribute('open'));
  assert(el.classList.contains('tf-sheet'));
  assert(el.classList.contains('tf-sheet--detent-large'));
  assertEq(el.querySelector('.tf-modal-card').getAttribute('role'), 'dialog');
});

test('Sheet renders body slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields({ bodySlot: 'sh-body' })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-modal-body [data-slot-id="sh-body"]') != null);
});

test('Sheet current_detent reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sd'), value: 'small' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SHEET_TAG, sheetFields({ detents: ['small', 'large'], currentDetent: BOUND('sd') })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-sheet--detent-small'));
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('sd'), op: { kind: 'set', value: 'large' } }] });
  assert(el.classList.contains('tf-sheet--detent-large'));
  assert(!el.classList.contains('tf-sheet--detent-small'));
});

test('Sheet rejects empty detents', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SHEET_TAG, sheetFields({ detents: [] }))));
});

test('Sheet rejects invalid detent value', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(SHEET_TAG, sheetFields({ detents: ['huge'] }))));
});

test('Sheet dismissible dispatches dismiss on ESC', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields({ dismissible: true })));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(dismissed);
});

test('Sheet renders title when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields({ title: LIT('Pick one') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Pick one');
  assertEq(el.querySelector('.tf-modal-title').textContent, 'Pick one');
});

// ============================================================================
// GateScreen
// ============================================================================

function buttonComp(label) {
  return {
    tag: 0x0401, id: 'btn1',
    fields: [
      [0, 'primary'], [1, 'neutral'], [2, LIT(label)],
      [5, 'md'], [6, false], [9, 'default'],
    ],
    handlers: null, bind: null, a11y: null,
    visibility: null, test_id: null,
  };
}

function gateFields({
  icon = { kind: 'named', name: 'lock' }, title = LIT('Access Denied'),
  message = LIT('You need permission'), actions = [buttonComp('Request')],
  variant = 'permission_denied',
} = {}) {
  return [[0, icon], [1, title], [2, message], [3, actions], [4, variant]];
}

test('GateScreen renders with variant class and icon', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(GATE_SCREEN_TAG, gateFields({ variant: 'auth_required' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-gate-screen'));
  assert(el.classList.contains('tf-gate-screen--variant-auth_required'));
  assert(el.querySelector('.tf-gate-screen__icon') != null);
});

test('GateScreen renders title and message', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(GATE_SCREEN_TAG, gateFields({
    title: LIT('Locked'), message: LIT('Contact admin'),
  })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-gate-screen__title').textContent, 'Locked');
  assertEq(el.querySelector('.tf-gate-screen__message').textContent, 'Contact admin');
});

test('GateScreen title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('gt'), value: 'Initial' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(GATE_SCREEN_TAG, gateFields({ title: BOUND('gt') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-gate-screen__title').textContent, 'Initial');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('gt'), op: { kind: 'set', value: 'Updated' } }] });
  assertEq(el.querySelector('.tf-gate-screen__title').textContent, 'Updated');
});

test('GateScreen renders action buttons as tf-button', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(GATE_SCREEN_TAG, gateFields()));
  document.body.appendChild(el);
  const actionsEl = el.querySelector('.tf-gate-screen__actions');
  assert(actionsEl != null);
  assert(actionsEl.children.length > 0);
  assertEq(actionsEl.children[0].tagName, 'TF-BUTTON');
});

test('GateScreen rejects non-Button action children', () => {
  setup();
  const engine = makeEngine(makeStore());
  const badAction = { tag: 0x0108, id: 'x', fields: [[0, 'horizontal'], [1, 'default'], [2, 'md']], handlers: null, bind: null, a11y: null, visibility: null, test_id: null };
  assertThrows(() => engine.render(comp(GATE_SCREEN_TAG, gateFields({ actions: [badAction] }))));
});

test('GateScreen rejects invalid variant', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(GATE_SCREEN_TAG, gateFields({ variant: 'custom' }))));
});

test('GateScreen rejects missing icon', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(GATE_SCREEN_TAG, [[1, LIT('T')], [2, LIT('M')], [3, [buttonComp('Go')]], [4, 'maintenance']])));
});

// ============================================================================
// ConfirmationDialog
// ============================================================================

function confirmFields({
  title = LIT('Confirm'), message = LIT('Are you sure?'), icon = null,
  tone = 'neutral', confirmLabel = LIT('Confirm'), cancelLabel = LIT('Cancel'),
  destructive = false, requireTyping = null,
} = {}) {
  const f = [[0, title], [1, message], [3, tone], [4, confirmLabel], [5, cancelLabel], [6, destructive]];
  if (icon != null) f.push([2, icon]);
  if (requireTyping != null) f.push([7, requireTyping]);
  return f;
}

test('ConfirmationDialog renders as tf-modal with tone class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ tone: 'critical' })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-MODAL');
  assert(el.classList.contains('tf-confirm-dialog'));
  assert(el.classList.contains('tf-confirm-dialog--tone-critical'));
  assert(el.hasAttribute('open'));
  assert(el.hasAttribute('no-close'));
  assertEq(el.querySelector('.tf-modal-card').getAttribute('role'), 'dialog');
});

test('ConfirmationDialog renders title and message', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({
    title: LIT('Delete?'), message: LIT('This is permanent'),
  })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Delete?');
  assertEq(el.querySelector('.tf-confirm-dialog__message').textContent, 'This is permanent');
});

test('ConfirmationDialog title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('ct'), value: 'Old title' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ title: BOUND('ct') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Old title');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('ct'), op: { kind: 'set', value: 'New title' } }] });
  assertEq(el.getAttribute('title'), 'New title');
});

test('ConfirmationDialog cancel tf-button dispatches dismiss', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields()));
  document.body.appendChild(el);
  const cancelBtn = el.querySelector('.tf-confirm-dialog__cancel');
  assertEq(cancelBtn.tagName, 'TF-BUTTON');
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  cancelBtn.click();
  assert(dismissed);
});

test('ConfirmationDialog confirm tf-button dispatches confirm event', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields()));
  document.body.appendChild(el);
  const confirmBtn = el.querySelector('.tf-confirm-dialog__confirm');
  assertEq(confirmBtn.tagName, 'TF-BUTTON');
  assertEq(confirmBtn.getAttribute('variant'), 'primary');
  let confirmed = false;
  el.addEventListener('confirm', () => { confirmed = true; });
  confirmBtn.click();
  assert(confirmed);
});

test('ConfirmationDialog destructive=true uses danger-solid confirm variant', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ destructive: true })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-confirm-dialog__confirm').getAttribute('variant'), 'danger-solid');
});

test('ConfirmationDialog require_typing disables confirm until typed', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ requireTyping: 'DELETE' })));
  document.body.appendChild(el);
  const confirmBtn = el.querySelector('.tf-confirm-dialog__confirm');
  assert(confirmBtn.hasAttribute('disabled'));
  const input = el.querySelector('.tf-confirm-dialog__typing');
  assert(input != null);
  assertEq(input.tagName, 'TF-INPUT');
  assertEq(input.getAttribute('label'), 'DELETE');
  let confirmed = false;
  el.addEventListener('confirm', () => { confirmed = true; });
  confirmBtn.click();
  assert(!confirmed);
  input.dispatchEvent(new globalThis.CustomEvent('input', { detail: { value: 'DELE' } }));
  assert(confirmBtn.hasAttribute('disabled'));
  input.dispatchEvent(new globalThis.CustomEvent('input', { detail: { value: 'DELETE' } }));
  assert(!confirmBtn.hasAttribute('disabled'));
  confirmBtn.click();
  assert(confirmed);
});

test('ConfirmationDialog ESC dispatches dismiss', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields()));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  document.dispatchEvent(new globalThis.KeyboardEvent('keydown', { key: 'Escape' }));
  assert(dismissed);
});

test('ConfirmationDialog rejects missing title', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(CONFIRMATION_DIALOG_TAG, [[1, LIT('msg')], [3, 'neutral'], [4, LIT('Ok')], [5, LIT('No')], [6, false]])));
});

test('ConfirmationDialog rejects invalid tone', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ tone: 'danger' }))));
});

test('ConfirmationDialog renders icon when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ icon: { kind: 'named', name: 'warning' } })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-confirm-dialog__icon') != null);
});

test('ConfirmationDialog confirm label reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('cl'), value: 'Yes' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ confirmLabel: BOUND('cl') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-confirm-dialog__confirm').getAttribute('label'), 'Yes');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('cl'), op: { kind: 'set', value: 'Proceed' } }] });
  assertEq(el.querySelector('.tf-confirm-dialog__confirm').getAttribute('label'), 'Proceed');
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`feedback-overlay tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
