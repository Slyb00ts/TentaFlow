// =============================================================================
// File: sdk-runtime/molecule-shell-renderer.test.js
// Description: Tests for AppShell (0x0006), LoginShell (0x0007),
// WizardShell (0x000B), EmptyState (0x0003), ErrorBoundary (0x0008),
// WelcomeHero (0x0009) — chunk 3.3f.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  APP_SHELL_TAG, LOGIN_SHELL_TAG, WIZARD_SHELL_TAG,
  EMPTY_STATE_TAG, ERROR_BOUNDARY_TAG, WELCOME_HERO_TAG,
} from './molecule-shell-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';

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
const ICON_NAMED = (name) => ({ kind: 'named', name, size: null, tone: null });

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
function btnComp(label) {
  return comp(BUTTON_TAG, [
    [0, 'primary'],
    [1, 'neutral'],
    [2, LIT(label)],
    [5, 'md'],
    [6, false],
    [9, 'default'],
  ], { id: `btn-${label}` });
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

// ============================================================================
// AppShell (0x0006)
// ============================================================================

test('AppShell renders sidebar and content slots', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(APP_SHELL_TAG, [
    [0, 'sidebar'],
    [1, 'main-content'],
    [3, 'xl'],
    [4, false],
  ]));
  assert(el.classList.contains('tf-app-shell'), 'class');
  const sidebar = el.querySelector('[data-slot-id="sidebar"]');
  const content = el.querySelector('[data-slot-id="main-content"]');
  assert(sidebar != null, 'sidebar slot');
  assert(content != null, 'content slot');
  assert(sidebar.classList.contains('tf-app-shell__sidebar'), 'sidebar class');
});

test('AppShell renders collapsible sidebar with toggle', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(APP_SHELL_TAG, [
    [0, 'nav'],
    [1, 'body'],
    [3, 'md'],
    [4, true],
  ]));
  assert(el.classList.contains('tf-app-shell--collapsible'), 'collapsible');
  const toggle = el.querySelector('.tf-app-shell__sidebar-toggle');
  assert(toggle != null, 'toggle present');
});

test('AppShell renders optional header slot', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(APP_SHELL_TAG, [
    [0, 'sb'],
    [1, 'ct'],
    [2, 'top-header'],
    [3, 'sm'],
    [4, false],
  ]));
  const header = el.querySelector('[data-slot-id="top-header"]');
  assert(header != null, 'header slot');
  assert(header.classList.contains('tf-app-shell__header'), 'header class');
});

test('AppShell defaults sidebar_width to xl', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(APP_SHELL_TAG, [
    [0, 'sb'],
    [1, 'ct'],
    [4, false],
  ]));
  const sidebar = el.querySelector('.tf-app-shell__sidebar');
  assert(sidebar.classList.contains('tf-app-shell__sidebar--width-xl'), 'default xl');
});

// ============================================================================
// LoginShell (0x0007)
// ============================================================================

test('LoginShell renders centred card with logo, title, content', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(LOGIN_SHELL_TAG, [
    [0, ICON_NAMED('core')],
    [1, LIT('Sign In')],
    [3, 'login-form'],
  ]));
  assert(el.classList.contains('tf-login-shell'), 'class');
  assert(el.querySelector('.tf-login-shell__logo') != null, 'logo');
  assert(el.querySelector('.tf-login-shell__title').textContent === 'Sign In', 'title');
  const slot = el.querySelector('[data-slot-id="login-form"]');
  assert(slot != null, 'content slot');
});

test('LoginShell renders subtitle and footer', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(LOGIN_SHELL_TAG, [
    [0, ICON_NAMED('apps')],
    [1, LIT('Welcome')],
    [2, LIT('Enter your credentials')],
    [3, 'form'],
    [4, 'footer-links'],
  ]));
  assert(el.querySelector('.tf-login-shell__subtitle').textContent === 'Enter your credentials', 'subtitle');
  const footer = el.querySelector('[data-slot-id="footer-links"]');
  assert(footer != null, 'footer slot');
  assert(footer.classList.contains('tf-login-shell__footer'), 'footer class');
});

test('LoginShell requires logo', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LOGIN_SHELL_TAG, [
    [1, LIT('No Logo')],
    [3, 'f'],
  ])));
});

// ============================================================================
// WizardShell (0x000B)
// ============================================================================

test('WizardShell renders steps and content/footer slots', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('wizard_step'), value: 'step2' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const steps = [
    { 0: 'step1', 1: LIT('Personal'), 2: false },
    { 0: 'step2', 1: LIT('Payment'), 2: false },
    { 0: 'step3', 1: LIT('Review'), 2: true },
  ];
  const el = engine.render(comp(WIZARD_SHELL_TAG, [
    [0, steps],
    [1, BOUND('wizard_step')],
    [2, 'wiz-body'],
    [3, 'wiz-footer'],
    [4, true],
  ]));
  assert(el.classList.contains('tf-wizard-shell'), 'class');
  assert(el.classList.contains('tf-wizard-shell--cancellable'), 'cancellable');
  const stepEls = el.querySelectorAll('.tf-wizard-shell__step');
  assertEq(stepEls.length, 3, 'three steps');
  assert(stepEls[1].classList.contains('tf-wizard-shell__step--current'), 'step2 current');
  assert(stepEls[2].classList.contains('tf-wizard-shell__step--optional'), 'step3 optional');
  assert(el.querySelector('[data-slot-id="wiz-body"]') != null, 'content slot');
  assert(el.querySelector('[data-slot-id="wiz-footer"]') != null, 'footer slot');
});

test('WizardShell requires current_step_id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(WIZARD_SHELL_TAG, [
    [0, []],
    [2, 'a'],
    [3, 'b'],
    [4, false],
  ])));
});

test('WizardShell renders without steps', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(WIZARD_SHELL_TAG, [
    [1, LIT('s1')],
    [2, 'content'],
    [3, 'footer'],
    [4, false],
  ]));
  const steps = el.querySelector('.tf-wizard-shell__steps');
  assert(steps == null, 'no steps nav when empty');
});

// ============================================================================
// EmptyState (0x0003)
// ============================================================================

test('EmptyState renders icon, heading, variant', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_STATE_TAG, [
    [0, ICON_NAMED('file')],
    [1, LIT('No items')],
    [5, 'default'],
  ]));
  assert(el.classList.contains('tf-empty-state'), 'class');
  assert(el.classList.contains('tf-empty-state--variant-default'), 'variant');
  assert(el.querySelector('.tf-empty-state__heading').textContent === 'No items', 'heading');
  assert(el.getAttribute('role') === 'status', 'role');
});

test('EmptyState renders message and actions', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(EMPTY_STATE_TAG, [
    [0, ICON_NAMED('search')],
    [1, LIT('Nothing found')],
    [2, LIT('Try adjusting your filters')],
    [3, btnComp('Clear')],
    [5, 'illustrated'],
  ]));
  assert(el.querySelector('.tf-empty-state__message').textContent === 'Try adjusting your filters', 'message');
  assert(el.querySelector('.tf-empty-state__actions') != null, 'actions');
  assert(el.classList.contains('tf-empty-state--variant-illustrated'), 'illustrated');
});

test('EmptyState rejects non-Button primary_action', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(EMPTY_STATE_TAG, [
    [0, ICON_NAMED('x')],
    [1, LIT('E')],
    [3, comp(0x9999, [], { id: 'bad' })],
    [5, 'default'],
  ])));
});

test('EmptyState requires icon', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(EMPTY_STATE_TAG, [
    [1, LIT('No icon')],
    [5, 'default'],
  ])));
});

// ============================================================================
// ErrorBoundary (0x0008)
// ============================================================================

test('ErrorBoundary renders title and message', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(ERROR_BOUNDARY_TAG, [
    [1, LIT('Something went wrong')],
    [2, LIT('Please try again')],
    [3, []],
  ]));
  assert(el.classList.contains('tf-error-boundary'), 'class');
  assert(el.getAttribute('role') === 'alert', 'role');
  assert(el.querySelector('.tf-error-boundary__title').textContent === 'Something went wrong', 'title');
  assert(el.querySelector('.tf-error-boundary__message').textContent === 'Please try again', 'message');
});

test('ErrorBoundary renders error code and technical details', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(ERROR_BOUNDARY_TAG, [
    [0, LIT('404')],
    [1, LIT('Not Found')],
    [3, []],
    [4, LIT('Stack trace here...')],
  ]));
  assert(el.querySelector('.tf-error-boundary__code').textContent === '404', 'code');
  const details = el.querySelector('.tf-error-boundary__details');
  assert(details != null, 'details element');
  assert(details.querySelector('pre').textContent === 'Stack trace here...', 'details text');
});

test('ErrorBoundary renders actions as Button children', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(ERROR_BOUNDARY_TAG, [
    [1, LIT('Error')],
    [3, [btnComp('Retry'), btnComp('Home')]],
  ]));
  const actions = el.querySelector('.tf-error-boundary__actions');
  assert(actions != null, 'actions');
  assertEq(actions.children.length, 2, 'two action buttons');
});

test('ErrorBoundary rejects non-Button actions', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(ERROR_BOUNDARY_TAG, [
    [1, LIT('E')],
    [3, [comp(0x9999, [], { id: 'bad' })]],
  ])));
});

// ============================================================================
// WelcomeHero (0x0009)
// ============================================================================

test('WelcomeHero renders illustration, title, subtitle, primary action', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(WELCOME_HERO_TAG, [
    [0, ICON_NAMED('sparkle')],
    [1, LIT('Welcome!')],
    [2, LIT('Get started with TentaFlow')],
    [3, []],
    [4, btnComp('Start')],
  ]));
  assert(el.classList.contains('tf-welcome-hero'), 'class');
  assert(el.querySelector('.tf-welcome-hero__illustration') != null, 'illustration');
  assert(el.querySelector('.tf-welcome-hero__title').textContent === 'Welcome!', 'title');
  assert(el.querySelector('.tf-welcome-hero__subtitle').textContent === 'Get started with TentaFlow', 'subtitle');
  assert(el.querySelector('.tf-welcome-hero__actions') != null, 'actions');
});

test('WelcomeHero renders features list', () => {
  setup();
  const engine = makeEngine();
  const features = [
    { 0: ICON_NAMED('check'), 1: LIT('Fast'), 2: LIT('Blazing speed') },
    { 0: ICON_NAMED('shield'), 1: LIT('Secure') },
  ];
  const el = engine.render(comp(WELCOME_HERO_TAG, [
    [0, ICON_NAMED('star')],
    [1, LIT('Hello')],
    [2, LIT('World')],
    [3, features],
    [4, btnComp('Go')],
  ]));
  const featureEls = el.querySelectorAll('.tf-welcome-hero__feature');
  assertEq(featureEls.length, 2, 'two features');
  assert(featureEls[0].querySelector('.tf-welcome-hero__feature-title').textContent === 'Fast', 'feature title');
  assert(featureEls[0].querySelector('.tf-welcome-hero__feature-desc').textContent === 'Blazing speed', 'feature desc');
});

test('WelcomeHero requires primary_action as Button', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(WELCOME_HERO_TAG, [
    [0, ICON_NAMED('x')],
    [1, LIT('T')],
    [2, LIT('S')],
    [3, []],
    [4, comp(0x9999, [], { id: 'bad' })],
  ])));
});

test('WelcomeHero requires primary_action', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(WELCOME_HERO_TAG, [
    [0, ICON_NAMED('x')],
    [1, LIT('T')],
    [2, LIT('S')],
    [3, []],
  ])));
});

test('WelcomeHero renders optional secondary action', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(WELCOME_HERO_TAG, [
    [0, ICON_NAMED('apps')],
    [1, LIT('Hi')],
    [2, LIT('There')],
    [3, []],
    [4, btnComp('Primary')],
    [5, btnComp('Secondary')],
  ]));
  const actions = el.querySelector('.tf-welcome-hero__actions');
  assertEq(actions.children.length, 2, 'primary + secondary');
});

// ============================================================================
// Summary
// ============================================================================

const passed = results.filter((r) => r.ok).length;
const failed = results.filter((r) => !r.ok);
console.log(`\nmolecule-shell-renderer: ${passed}/${results.length} passed`);
for (const f of failed) console.error(`  FAIL: ${f.name}`, f.err);
if (failed.length > 0) process.exit(1);
