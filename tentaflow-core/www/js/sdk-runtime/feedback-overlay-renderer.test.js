// =============================================================================
// File: sdk-runtime/feedback-overlay-renderer.test.js
// Description: Tests for Modal (0x0509), Drawer (0x050A), Popover (0x050B),
// Sheet (0x050C), GateScreen (0x050D), ConfirmationDialog (0x050E) —
// chunk 3.3e-3.
// =============================================================================

import './_dom-test-harness.js';
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
  preventScroll = false, closable = false,
} = {}) {
  const f = [[0, title], [2, bodySlot], [4, size], [5, dismissible], [6, preventScroll], [7, closable]];
  if (subtitle != null) f.push([1, subtitle]);
  if (footerSlot != null) f.push([3, footerSlot]);
  return f;
}

test('Modal renders with role=dialog and size class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ size: 'lg' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-modal'));
  assertEq(el.getAttribute('role'), 'dialog');
  assertEq(el.getAttribute('aria-modal'), 'true');
  assert(el.querySelector('.tf-modal__container').classList.contains('tf-modal--size-lg'));
});

test('Modal renders title via BindRef', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ title: LIT('My Modal') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-modal__title').textContent, 'My Modal');
});

test('Modal title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('mt'), value: 'Old' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(MODAL_TAG, modalFields({ title: BOUND('mt') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-modal__title').textContent, 'Old');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('mt'), op: { kind: 'set', value: 'New' } }] });
  assertEq(el.querySelector('.tf-modal__title').textContent, 'New');
});

test('Modal renders body slot with data-slot-id', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ bodySlot: 'modal-body' })));
  document.body.appendChild(el);
  const bodyEl = el.querySelector('.tf-modal__body');
  assert(bodyEl != null);
  assertEq(bodyEl.getAttribute('data-slot-id'), 'modal-body');
});

test('Modal renders footer slot when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ footerSlot: 'modal-footer' })));
  document.body.appendChild(el);
  const footerEl = el.querySelector('.tf-modal__footer');
  assert(footerEl != null);
  assertEq(footerEl.getAttribute('data-slot-id'), 'modal-footer');
});

test('Modal closable=true shows close button', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(MODAL_TAG, modalFields({ closable: true })));
  document.body.appendChild(el);
  const closeBtn = el.querySelector('.tf-modal__close');
  assert(closeBtn != null);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  closeBtn.click();
  assert(dismissed);
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
  assertEq(el.querySelector('.tf-modal__subtitle').textContent, 'Sub');
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

test('Drawer renders with side and size classes', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ side: 'left', size: 'lg' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-drawer'));
  assert(el.classList.contains('tf-drawer--side-left'));
  assert(el.classList.contains('tf-drawer--size-lg'));
  assertEq(el.getAttribute('role'), 'dialog');
});

test('Drawer renders body slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ bodySlot: 'dr-body' })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-drawer__body').getAttribute('data-slot-id'), 'dr-body');
});

test('Drawer renders title when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ title: LIT('Settings') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-drawer__title').textContent, 'Settings');
});

test('Drawer title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('dt'), value: 'First' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(DRAWER_TAG, drawerFields({ title: BOUND('dt') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-drawer__title').textContent, 'First');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('dt'), op: { kind: 'set', value: 'Second' } }] });
  assertEq(el.querySelector('.tf-drawer__title').textContent, 'Second');
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
  assertEq(el.querySelector('.tf-drawer__footer').getAttribute('data-slot-id'), 'dr-footer');
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

test('Sheet renders with dialog role and detent class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields({ detents: ['large'] })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-sheet'));
  assertEq(el.getAttribute('role'), 'dialog');
  assert(el.querySelector('.tf-sheet__container').classList.contains('tf-sheet--detent-large'));
});

test('Sheet renders body slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields({ bodySlot: 'sh-body' })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-sheet__body').getAttribute('data-slot-id'), 'sh-body');
});

test('Sheet current_detent reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sd'), value: 'small' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SHEET_TAG, sheetFields({ detents: ['small', 'large'], currentDetent: BOUND('sd') })));
  document.body.appendChild(el);
  const container = el.querySelector('.tf-sheet__container');
  assert(container.classList.contains('tf-sheet--detent-small'));
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('sd'), op: { kind: 'set', value: 'large' } }] });
  assert(container.classList.contains('tf-sheet--detent-large'));
  assert(!container.classList.contains('tf-sheet--detent-small'));
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

test('Sheet renders handle', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(SHEET_TAG, sheetFields()));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-sheet__handle') != null);
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

test('GateScreen renders action buttons', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(GATE_SCREEN_TAG, gateFields()));
  document.body.appendChild(el);
  const actionsEl = el.querySelector('.tf-gate-screen__actions');
  assert(actionsEl != null);
  assert(actionsEl.children.length > 0);
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

test('ConfirmationDialog renders with tone class and alertdialog role', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ tone: 'critical' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-confirm-dialog'));
  assert(el.classList.contains('tf-confirm-dialog--tone-critical'));
  assertEq(el.getAttribute('role'), 'alertdialog');
});

test('ConfirmationDialog renders title and message', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({
    title: LIT('Delete?'), message: LIT('This is permanent'),
  })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-confirm-dialog__title').textContent, 'Delete?');
  assertEq(el.querySelector('.tf-confirm-dialog__message').textContent, 'This is permanent');
});

test('ConfirmationDialog title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('ct'), value: 'Old title' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ title: BOUND('ct') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-confirm-dialog__title').textContent, 'Old title');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('ct'), op: { kind: 'set', value: 'New title' } }] });
  assertEq(el.querySelector('.tf-confirm-dialog__title').textContent, 'New title');
});

test('ConfirmationDialog cancel dispatches dismiss', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields()));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  el.querySelector('.tf-confirm-dialog__cancel').click();
  assert(dismissed);
});

test('ConfirmationDialog confirm dispatches confirm event', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields()));
  document.body.appendChild(el);
  let confirmed = false;
  el.addEventListener('confirm', () => { confirmed = true; });
  el.querySelector('.tf-confirm-dialog__confirm').click();
  assert(confirmed);
});

test('ConfirmationDialog destructive=true adds destructive class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ destructive: true })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-confirm-dialog__confirm--destructive') != null);
});

test('ConfirmationDialog require_typing disables confirm until typed', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CONFIRMATION_DIALOG_TAG, confirmFields({ requireTyping: 'DELETE' })));
  document.body.appendChild(el);
  const confirmBtn = el.querySelector('.tf-confirm-dialog__confirm');
  assert(confirmBtn.hasAttribute('disabled'));
  const input = el.querySelector('.tf-confirm-dialog__typing-input');
  assert(input != null);
  input.value = 'DELE';
  input.dispatchEvent(new globalThis.Event('input'));
  assert(confirmBtn.hasAttribute('disabled'));
  input.value = 'DELETE';
  input.dispatchEvent(new globalThis.Event('input'));
  assert(!confirmBtn.hasAttribute('disabled'));
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
  assertEq(el.querySelector('.tf-confirm-dialog__confirm').textContent, 'Yes');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('cl'), op: { kind: 'set', value: 'Proceed' } }] });
  assertEq(el.querySelector('.tf-confirm-dialog__confirm').textContent, 'Proceed');
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`feedback-overlay tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
