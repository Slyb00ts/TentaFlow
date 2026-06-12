// =============================================================================
// File: sdk-runtime/feedback-inline-renderer.test.js
// Description: Tests for Alert (0x0501), Banner (0x0502), Callout (0x0503),
// Toast (0x0504), Hint (0x0505), OfflineBanner (0x050F) — chunk 3.3e-1.
// Alert/Toast render through the <tf-alert>/<tf-toast> web components, which
// are imported after the DOM harness so elements upgrade for real behavior.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-alert.js';
import '../components/tf-toast.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  ALERT_TAG, BANNER_TAG, CALLOUT_TAG, TOAST_TAG, HINT_TAG, OFFLINE_BANNER_TAG,
} from './feedback-inline-renderer.js';

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
// Alert
// ============================================================================

function alertFields({
  tone = 'info', variant = 'default', icon = null,
  title = null, message = LIT('Something happened'),
  actions = null, dismissible = false,
} = {}) {
  const f = [[0, tone], [1, variant]];
  if (icon != null) f.push([2, icon]);
  if (title != null) f.push([3, title]);
  f.push([4, message]);
  if (actions != null) f.push([5, actions]);
  f.push([6, dismissible]);
  return f;
}

// Button (0x0401) child used for Alert.actions.
function alertButtonComp(label) {
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

test('Alert renders as tf-alert with mapped tone and variant class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ tone: 'success', variant: 'filled' })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-ALERT');
  assertEq(el.getAttribute('tone'), 'success');
  assert(el.classList.contains('tf-alert--variant-filled'));
  assert(el.querySelector('.tf-alert').classList.contains('success'));
});

test('Alert maps critical tone to danger', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ tone: 'critical' })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('tone'), 'danger');
});

test('Alert renders message text via BindRef', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ message: LIT('Disk full') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('message'), 'Disk full');
  assertEq(el.querySelector('.tf-alert-message').textContent, 'Disk full');
});

test('Alert reacts to message patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('msg'), value: 'Old message' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(ALERT_TAG, alertFields({ message: BOUND('msg') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('message'), 'Old message');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('msg'), op: { kind: 'set', value: 'New message' } }] });
  assertEq(el.getAttribute('message'), 'New message');
});

test('Alert rejects missing message', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(ALERT_TAG, [[0, 'info'], [1, 'default'], [6, false]])));
});

test('Alert dismissible=true shows close button dispatching dismiss event', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ dismissible: true })));
  document.body.appendChild(el);
  assert(el.hasAttribute('dismissable'));
  const closeBtn = el.querySelector('.tf-alert-close');
  assert(closeBtn != null);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  closeBtn.click();
  assert(dismissed);
});

test('Alert renders title when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ title: LIT('Warning') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Warning');
  assertEq(el.querySelector('.tf-alert-title').textContent, 'Warning');
});

test('Alert renders tone icon from tf-alert', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ icon: { kind: 'named', name: 'alert' } })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-alert-icon') != null);
});

test('Alert renders action buttons in preserved actions slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(ALERT_TAG, alertFields({ actions: [alertButtonComp('Retry')] })));
  document.body.appendChild(el);
  const actionsEl = el.querySelector('.tf-alert-content .tf-alert__actions');
  assert(actionsEl != null);
  assertEq(actionsEl.children[0].tagName, 'TF-BUTTON');
});

test('Alert rejects invalid tone', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(ALERT_TAG, alertFields({ tone: 'nope' }))));
});

test('Alert rejects invalid variant', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(ALERT_TAG, alertFields({ variant: 'bad' }))));
});

// ============================================================================
// Banner
// ============================================================================

function bannerFields({
  tone = 'info', icon = null, message = LIT('Update available'),
  action = null, dismissible = false, position = 'inline',
} = {}) {
  const f = [[0, tone]];
  if (icon != null) f.push([1, icon]);
  f.push([2, message]);
  if (action != null) f.push([3, action]);
  f.push([4, dismissible], [5, position]);
  return f;
}

test('Banner renders with tone and position classes', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(BANNER_TAG, bannerFields({ tone: 'warning', position: 'top' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-banner'));
  assert(el.classList.contains('tf-banner--tone-warning'));
  assert(el.classList.contains('tf-banner--position-top'));
});

test('Banner renders message text', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(BANNER_TAG, bannerFields({ message: LIT('Hello banner') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-banner__message').textContent, 'Hello banner');
});

test('Banner reacts to message patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('bm'), value: 'Initial' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(BANNER_TAG, bannerFields({ message: BOUND('bm') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-banner__message').textContent, 'Initial');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('bm'), op: { kind: 'set', value: 'Updated' } }] });
  assertEq(el.querySelector('.tf-banner__message').textContent, 'Updated');
});

test('Banner rejects missing message', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(BANNER_TAG, [[0, 'info'], [4, false], [5, 'inline']])));
});

test('Banner dismissible=true dispatches dismiss event', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(BANNER_TAG, bannerFields({ dismissible: true })));
  document.body.appendChild(el);
  let dismissed = false;
  el.addEventListener('dismiss', () => { dismissed = true; });
  el.querySelector('.tf-banner__close').click();
  assert(dismissed);
});

test('Banner rejects invalid position', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(BANNER_TAG, bannerFields({ position: 'bottom' }))));
});

// ============================================================================
// Callout
// ============================================================================

function calloutFields({
  tone = 'info', icon = null, title = null,
  content = [],
} = {}) {
  const f = [[0, tone]];
  if (icon != null) f.push([1, icon]);
  if (title != null) f.push([2, title]);
  f.push([3, content]);
  return f;
}

// Divider (0x0108) with required fields: orientation, variant, spacing.
function stubChild() {
  return {
    tag: 0x0108,
    id: 'child1',
    fields: [[0, 'horizontal'], [1, 'default'], [2, 'md']],
    handlers: null,
    bind: null,
    a11y: null,
    visibility: null,
    test_id: null,
  };
}

test('Callout renders with tone class and content children', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALLOUT_TAG, calloutFields({ tone: 'critical', content: [stubChild()] })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-callout'));
  assert(el.classList.contains('tf-callout--tone-critical'));
  assert(el.querySelector('.tf-callout__content') != null);
  assert(el.querySelector('.tf-callout__content').children.length > 0);
});

test('Callout renders title when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALLOUT_TAG, calloutFields({ title: LIT('Note'), content: [stubChild()] })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-callout__title').textContent, 'Note');
});

test('Callout title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('ct'), value: 'Old' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(CALLOUT_TAG, calloutFields({ title: BOUND('ct'), content: [stubChild()] })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-callout__title').textContent, 'Old');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('ct'), op: { kind: 'set', value: 'New' } }] });
  assertEq(el.querySelector('.tf-callout__title').textContent, 'New');
});

test('Callout rejects missing content', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(CALLOUT_TAG, [[0, 'info']])));
});

test('Callout rejects non-array content', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(CALLOUT_TAG, [[0, 'info'], [3, 'not-an-array']])));
});

test('Callout renders icon when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(CALLOUT_TAG, calloutFields({ icon: { kind: 'named', name: 'info' }, content: [stubChild()] })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-callout__icon') != null);
});

// ============================================================================
// Toast
// ============================================================================

function toastFields({
  tone = 'success', title = LIT('Saved'), body = null,
  icon = null, actionLabel = null, actionId = null,
} = {}) {
  const f = [[0, tone], [1, title]];
  if (body != null) f.push([2, body]);
  if (icon != null) f.push([3, icon]);
  if (actionLabel != null) f.push([4, actionLabel]);
  if (actionId != null) f.push([5, actionId]);
  return f;
}

test('Toast renders as persistent tf-toast with mapped tone and title', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(TOAST_TAG, toastFields({ tone: 'success', title: LIT('Done') })));
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-TOAST');
  assertEq(el.getAttribute('tone'), 'success');
  assert(el.hasAttribute('persistent'));
  assert(el.classList.contains('tf-toast--tone-success'));
  assertEq(el.getAttribute('title'), 'Done');
  assertEq(el.querySelector('.tf-toast-title').textContent, 'Done');
  assertEq(el.getAttribute('role'), 'status');
});

test('Toast maps critical tone to danger', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(TOAST_TAG, toastFields({ tone: 'critical' })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('tone'), 'danger');
});

test('Toast renders body when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(TOAST_TAG, toastFields({ body: LIT('Details here') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('message'), 'Details here');
  assertEq(el.querySelector('.tf-toast-message').textContent, 'Details here');
});

test('Toast title reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('tt'), value: 'Loading' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(TOAST_TAG, toastFields({ title: BOUND('tt') })));
  document.body.appendChild(el);
  assertEq(el.getAttribute('title'), 'Loading');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('tt'), op: { kind: 'set', value: 'Complete' } }] });
  assertEq(el.getAttribute('title'), 'Complete');
});

test('Toast rejects missing title', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(TOAST_TAG, [[0, 'info']])));
});

test('Toast action tf-button dispatches toast_action event', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(TOAST_TAG, toastFields({ actionLabel: 'Undo', actionId: 'undo_save' })));
  document.body.appendChild(el);
  const actionBtn = el.querySelector('.tf-toast__action');
  assert(actionBtn != null);
  assertEq(actionBtn.tagName, 'TF-BUTTON');
  assertEq(actionBtn.getAttribute('label'), 'Undo');
  let detail = null;
  el.addEventListener('toast_action', (e) => { detail = e.detail; });
  actionBtn.click();
  assertEq(detail.action_id, 'undo_save');
});

test('Toast renders icon in preserved icon slot', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(TOAST_TAG, toastFields({ icon: { kind: 'named', name: 'check' } })));
  document.body.appendChild(el);
  const iconEl = el.querySelector('.tf-toast__icon');
  assert(iconEl != null);
  // Preserved inside the tf-toast root, before the title.
  assert(iconEl.parentElement.classList.contains('tf-toast'));
});

// ============================================================================
// Hint
// ============================================================================

function hintFields({
  content = LIT('Helpful tip'), icon = null, tone = 'info',
} = {}) {
  const f = [[0, content]];
  if (icon != null) f.push([1, icon]);
  f.push([2, tone]);
  return f;
}

test('Hint renders content text', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(HINT_TAG, hintFields()));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-hint'));
  assertEq(el.querySelector('.tf-hint__content').textContent, 'Helpful tip');
});

test('Hint renders with tone class when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(HINT_TAG, hintFields({ tone: 'muted' })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-hint--tone-muted'));
});

test('Hint content reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('h'), value: 'First' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(HINT_TAG, hintFields({ content: BOUND('h') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-hint__content').textContent, 'First');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('h'), op: { kind: 'set', value: 'Second' } }] });
  assertEq(el.querySelector('.tf-hint__content').textContent, 'Second');
});

test('Hint rejects missing content', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(HINT_TAG, [])));
});

test('Hint renders icon when provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(HINT_TAG, hintFields({ icon: { kind: 'named', name: 'help' } })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-hint__icon') != null);
});

test('Hint rejects unknown field key', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(HINT_TAG, [[0, LIT('x')], [9, 'bad']])));
});

// ============================================================================
// OfflineBanner
// ============================================================================

function offlineFields({
  message = LIT('No connection'), actionLabel = null, reconnecting = LIT(false),
} = {}) {
  const f = [[0, message]];
  if (actionLabel != null) f.push([1, actionLabel]);
  f.push([2, reconnecting]);
  return f;
}

test('OfflineBanner renders message and role=status', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(OFFLINE_BANNER_TAG, offlineFields()));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-offline-banner'));
  assertEq(el.getAttribute('role'), 'status');
  assertEq(el.querySelector('.tf-offline-banner__message').textContent, 'No connection');
});

test('OfflineBanner reconnecting=true adds reconnecting class', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(OFFLINE_BANNER_TAG, offlineFields({ reconnecting: LIT(true) })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-offline-banner--reconnecting'));
});

test('OfflineBanner reconnecting reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('rc'), value: false }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(OFFLINE_BANNER_TAG, offlineFields({ reconnecting: BOUND('rc') })));
  document.body.appendChild(el);
  assert(!el.classList.contains('tf-offline-banner--reconnecting'));
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('rc'), op: { kind: 'set', value: true } }] });
  assert(el.classList.contains('tf-offline-banner--reconnecting'));
});

test('OfflineBanner rejects missing message', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(OFFLINE_BANNER_TAG, [[2, LIT(false)]])));
});

test('OfflineBanner rejects missing reconnecting', () => {
  setup();
  const engine = makeEngine(makeStore());
  assertThrows(() => engine.render(comp(OFFLINE_BANNER_TAG, [[0, LIT('msg')]])));
});

test('OfflineBanner renders action tf-button when action_label provided', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(OFFLINE_BANNER_TAG, offlineFields({ actionLabel: LIT('Retry') })));
  document.body.appendChild(el);
  const btn = el.querySelector('.tf-offline-banner__action');
  assert(btn != null);
  assertEq(btn.tagName, 'TF-BUTTON');
  assertEq(btn.getAttribute('label'), 'Retry');
});

test('OfflineBanner action button dispatches offline_action event', () => {
  setup();
  const engine = makeEngine(makeStore());
  const el = engine.render(comp(OFFLINE_BANNER_TAG, offlineFields({ actionLabel: LIT('Retry') })));
  document.body.appendChild(el);
  let fired = false;
  el.addEventListener('offline_action', () => { fired = true; });
  el.querySelector('.tf-offline-banner__action').click();
  assert(fired);
});

test('OfflineBanner message reacts to patch', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('om'), value: 'Lost' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(OFFLINE_BANNER_TAG, offlineFields({ message: BOUND('om') })));
  document.body.appendChild(el);
  assertEq(el.querySelector('.tf-offline-banner__message').textContent, 'Lost');
  store.applyPatch({ base_revision: 0, new_revision: 1, ops: [{ path: PATH('om'), op: { kind: 'set', value: 'Reconnected' } }] });
  assertEq(el.querySelector('.tf-offline-banner__message').textContent, 'Reconnected');
});

// ============================================================================
const failed = results.filter((r) => !r.ok);
console.log(`feedback-inline tests: ${results.length - failed.length}/${results.length} passed`);
for (const f of failed) console.error(`FAIL ${f.name}:`, f.err && f.err.stack || f.err);
if (failed.length > 0) process.exit(1);
