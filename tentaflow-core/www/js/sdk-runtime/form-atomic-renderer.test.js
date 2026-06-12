// =============================================================================
// File: sdk-runtime/form-atomic-renderer.test.js
// Description: Tests for Toggle (0x030A) / Checkbox (0x030B) / Radio (0x030C)
// rendered through the <tf-toggle> / <tf-checkbox> / <tf-radio> web components.
// Components are imported so happy-dom upgrades them on mount. tf-toggle
// imports '/js/lib/sfx.js' with a browser-absolute specifier, so a module
// resolve hook maps '/js/...' onto the www root before the dynamic import.
// =============================================================================

import './_dom-test-harness.js';
import { registerHooks } from 'node:module';
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('/js/')) {
      return nextResolve(new URL(`../..${specifier}`, import.meta.url).href, context);
    }
    return nextResolve(specifier, context);
  },
});
await import('../components/tf-toggle.js');
import '../components/tf-checkbox.js';
import '../components/tf-radio.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { TOGGLE_TAG, CHECKBOX_TAG, RADIO_TAG } from './form-atomic-renderer.js';

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
function makeEngine(store) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: { emit() {} },
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
// Components build their light DOM in connectedCallback, so tests mount.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

// ============================================================================
// Toggle (0x030A)
// ============================================================================

test('Toggle renders <tf-toggle> with reactive checked attr from store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')],
      [1, { kind: 'literal', value: 'Powiadomienia' }],
      [3, 'md'],
      [6, 'trailing'],
    ])
  ));
  const toggle = el.querySelector('tf-toggle');
  assert(toggle != null, 'expected tf-toggle host element');
  assertEq(toggle.hasAttribute('checked'), false);
  const sw = toggle.querySelector('[role=switch]');
  assertEq(sw.getAttribute('aria-checked'), 'false');
  assertEq(el.querySelector('.tf-toggle__label').textContent, 'Powiadomienia');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('on'), op: { kind: 'set', value: true } }],
  });
  assertEq(toggle.hasAttribute('checked'), true);
  assertEq(sw.getAttribute('aria-checked'), 'true');
});

test('Toggle click re-emits change with SDK { value, kind: bool } payload', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')],
      [1, { kind: 'literal', value: 'X' }],
      [3, 'sm'], [6, 'leading'],
    ])
  ));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  el.querySelector('tf-toggle [role=switch]').click();
  // Exactly one SDK event — the raw component event must not leak to wrapper.
  assertEq(events, [{ value: true, kind: 'bool' }]);
});

test('Toggle disabled: raw component change never leaks to wrapper', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('on'), value: false },
      { path: PATH('locked'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
      [3, 'md'], [5, { kind: 'bound', path: PATH('locked') }], [6, 'trailing'],
    ])
  ));
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  // The raw component event bubbles; even on a muted toggle it must be
  // stopped before it can reach the wrapper with its { checked } detail.
  el.querySelector('tf-toggle').dispatchEvent(
    new CustomEvent('change', { bubbles: true, detail: { checked: true } })
  );
  assertEq(events, []);
});

test('Toggle uses default tone=primary when field absent', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
      [3, 'md'], [6, 'trailing'],
    ])
  );
  assert(el.classList.contains('tf-toggle--tone-primary'));
  assert(el.classList.contains('tf-toggle--size-md'));
  assert(el.classList.contains('tf-toggle--label-trailing'));
});

test('Toggle disabled BindRef sets disabled attr and blocks click', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('on'), value: false },
      { path: PATH('locked'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
      [3, 'md'], [5, { kind: 'bound', path: PATH('locked') }], [6, 'trailing'],
    ])
  ));
  const toggle = el.querySelector('tf-toggle');
  assertEq(toggle.hasAttribute('disabled'), true);
  assertEq(
    toggle.querySelector('[role=switch]').getAttribute('aria-disabled'),
    'true'
  );
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  toggle.querySelector('[role=switch]').click();
  assertEq(received, null);
});

test('Toggle label_position=leading places label before tf-toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'L' }],
      [3, 'md'], [6, 'leading'],
    ])
  );
  assertEq(el.children[0].classList.contains('tf-toggle__label'), true);
  assertEq(el.children[1].tagName.toLowerCase(), 'tf-toggle');
});

test('Toggle hint BindRef renders reactive .tf-toggle__hint', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('on'), value: false },
      { path: PATH('h'), value: 'pomocniczy' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
      [2, { kind: 'bound', path: PATH('h') }],
      [3, 'md'], [6, 'trailing'],
    ])
  );
  const hint = el.querySelector('.tf-toggle__hint');
  assertEq(hint.textContent, 'pomocniczy');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('h'), op: { kind: 'set', value: 'inny' } }],
  });
  assertEq(hint.textContent, 'inny');
});

test('Toggle without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(TOGGLE_TAG, [[0, PATH('on')], [3, 'md'], [6, 'trailing']]))
  );
});

test('Toggle rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(TOGGLE_TAG, [
        [0, PATH('on')], [1, { kind: 'literal', value: 'X' }],
        [3, 'md'], [6, 'trailing'], [99, 'rogue'],
      ])
    )
  );
});

test('Toggle without label propagates a11y.label as aria-label on tf-toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('on'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(TOGGLE_TAG, [
      [0, PATH('on')], [3, 'md'], [6, 'trailing'],
    ], { a11y: { label: { kind: 'literal', value: 'Powiadomienia' } } })
  );
  assertEq(
    el.querySelector('tf-toggle').getAttribute('aria-label'),
    'Powiadomienia'
  );
});

test('Toggle a11y.label rejects whitespace-only initial value', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(TOGGLE_TAG, [
        [0, PATH('on')], [3, 'md'], [6, 'trailing'],
      ], { a11y: { label: { kind: 'literal', value: '   ' } } })
    )
  );
});

// ============================================================================
// Checkbox (0x030B)
// ============================================================================

test('Checkbox renders <tf-checkbox> with reactive checked attr', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')],
      [1, { kind: 'literal', value: 'Zgoda' }],
      [5, 'md'],
    ])
  ));
  assertEq(el.tagName.toLowerCase(), 'tf-checkbox');
  assert(el.classList.contains('tf-checkbox--size-md'));
  assertEq(el.getAttribute('label'), 'Zgoda');
  assertEq(el.querySelector('.tf-checkbox-text').textContent, 'Zgoda');
  assertEq(el.hasAttribute('checked'), false);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('chk'), op: { kind: 'set', value: true } }],
  });
  assertEq(el.hasAttribute('checked'), true);
  assertEq(
    el.querySelector('[role=checkbox]').getAttribute('aria-checked'),
    'true'
  );
});

test('Checkbox click re-emits exactly ONE SDK change (no recursion)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }], [5, 'md'],
    ])
  ));
  // Listener registered AFTER the renderer's — exactly like the dispatcher.
  // Pre-fix the re-emit re-entered the renderer's own listener (~1500
  // recursive dispatches with a degraded payload); the guard must yield
  // exactly one SDK-shaped event per click.
  const events = [];
  el.addEventListener('change', (e) => events.push(e.detail));
  el.querySelector('.tf-checkbox-label').click();
  assertEq(events, [{ value: true, kind: 'bool' }]);
  el.querySelector('.tf-checkbox-label').click();
  assertEq(events, [
    { value: true, kind: 'bool' },
    { value: false, kind: 'bool' },
  ]);
});

test('Checkbox raw component { checked } detail never reaches dispatcher', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }], [5, 'md'],
    ])
  ));
  const seenDetails = [];
  el.addEventListener('change', (e) => seenDetails.push(e.detail));
  el.querySelector('.tf-checkbox-label').click();
  assert(
    seenDetails.every((d) => d != null && !('checked' in d) && 'kind' in d),
    'raw component { checked } detail leaked to the dispatcher listener'
  );
});

test('Checkbox indeterminate BindRef reactive (attr + aria-checked=mixed)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('chk'), value: false },
      { path: PATH('ind'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }],
      [3, { kind: 'bound', path: PATH('ind') }], [5, 'md'],
    ])
  ));
  assertEq(el.hasAttribute('indeterminate'), true);
  assertEq(
    el.querySelector('[role=checkbox]').getAttribute('aria-checked'),
    'mixed'
  );
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('ind'), op: { kind: 'set', value: false } }],
  });
  assertEq(el.hasAttribute('indeterminate'), false);
});

test('Checkbox disabled BindRef blocks click (no change emitted)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('chk'), value: false },
      { path: PATH('lock'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }],
      [4, { kind: 'bound', path: PATH('lock') }], [5, 'md'],
    ])
  ));
  assertEq(el.hasAttribute('disabled'), true);
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.querySelector('.tf-checkbox-label').click();
  assertEq(received, null);
});

test('Checkbox label BindRef updates label attr reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('chk'), value: false },
      { path: PATH('lbl'), value: 'Zgoda' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(CHECKBOX_TAG, [
      [0, PATH('chk')],
      [1, { kind: 'bound', path: PATH('lbl') }],
      [5, 'md'],
    ])
  ));
  assertEq(el.getAttribute('label'), 'Zgoda');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Consent' } }],
  });
  assertEq(el.getAttribute('label'), 'Consent');
  assertEq(el.querySelector('.tf-checkbox-text').textContent, 'Consent');
});

test('Checkbox without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(CHECKBOX_TAG, [[0, PATH('chk')], [5, 'md']]))
  );
});

test('Checkbox without label propagates a11y.label as aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('chk'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(CHECKBOX_TAG, [[0, PATH('chk')], [5, 'md']], {
      a11y: { label: { kind: 'literal', value: 'Zgoda RODO' } },
    })
  );
  assertEq(el.getAttribute('aria-label'), 'Zgoda RODO');
});

test('Checkbox rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(CHECKBOX_TAG, [
      [0, PATH('chk')], [1, { kind: 'literal', value: 'X' }], [5, 'md'],
      [77, 'rogue'],
    ]))
  );
});

// ============================================================================
// Radio (0x030C)
// ============================================================================

test('Radio renders <tf-radio> with value/label attrs + reactive checked class', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('view'), value: 'list' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(RADIO_TAG, [
      [0, PATH('view')],
      [1, { kind: 'tstr', value: 'list' }],
      [2, { kind: 'literal', value: 'Lista' }],
    ])
  ));
  assertEq(el.tagName.toLowerCase(), 'tf-radio');
  assertEq(el.getAttribute('value'), 'list');
  assertEq(el.getAttribute('label'), 'Lista');
  assert(el.classList.contains('tf-radio--checked'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('view'), op: { kind: 'set', value: 'grid' } }],
  });
  assert(!el.classList.contains('tf-radio--checked'));
});

test('Radio click dispatches change with SelectValue payload (tstr)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('view'), value: 'grid' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(RADIO_TAG, [
      [0, PATH('view')],
      [1, { kind: 'tstr', value: 'list' }],
      [2, { kind: 'literal', value: 'Lista' }],
    ])
  ));
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.querySelector('.tf-radio-label').click();
  assertEq(received, { value: 'list', kind: 'tstr' });
});

test('Radio u32 SelectValue accepts BigInt and emits Number value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('n'), value: 0 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(RADIO_TAG, [
      [0, PATH('n')],
      [1, { kind: 'u32', value: 5n }],
      [2, { kind: 'literal', value: 'Pięć' }],
    ])
  ));
  assertEq(el.getAttribute('value'), '5');
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.click();
  assertEq(received, { value: 5, kind: 'u32' });
});

test('Radio i32 SelectValue accepts negative BigInt in range', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(RADIO_TAG, [
      [0, PATH('n')],
      [1, { kind: 'i32', value: -2147483648n }],
      [2, { kind: 'literal', value: 'Min' }],
    ])
  );
  assertEq(el.getAttribute('value'), '-2147483648');
});

test('Radio SelectValue rejects out-of-range BigInt (u32 + i32)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(RADIO_TAG, [
      [0, PATH('n')],
      [1, { kind: 'u32', value: 0x100000000n }],
      [2, { kind: 'literal', value: 'X' }],
    ]))
  );
  assertThrows(() =>
    engine.render(comp(RADIO_TAG, [
      [0, PATH('n')],
      [1, { kind: 'i32', value: -2147483649n }],
      [2, { kind: 'literal', value: 'X' }],
    ]))
  );
});

test('Radio disabled BindRef blocks click (no change emitted)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('view'), value: 'grid' },
      { path: PATH('lock'), value: true },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(RADIO_TAG, [
      [0, PATH('view')],
      [1, { kind: 'tstr', value: 'list' }],
      [2, { kind: 'literal', value: 'Lista' }],
      [4, { kind: 'bound', path: PATH('lock') }],
    ])
  ));
  assertEq(el.hasAttribute('disabled'), true);
  let received = null;
  el.addEventListener('change', (e) => { received = e.detail; });
  el.click();
  assertEq(received, null);
});

test('Radio rejects missing required value/label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(RADIO_TAG, [[0, PATH('x')]])));
  assertThrows(() =>
    engine.render(comp(RADIO_TAG, [[0, PATH('x')], [1, { kind: 'tstr', value: 'a' }]]))
  );
});

test('Radio rejects SelectValue with unknown kind', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(RADIO_TAG, [
        [0, PATH('x')],
        [1, { kind: 'float', value: 1.5 }],
        [2, { kind: 'literal', value: 'X' }],
      ])
    )
  );
});

test('Radio rejects SelectValue with unexpected extra key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(RADIO_TAG, [
        [0, PATH('x')],
        [1, { kind: 'tstr', value: 'a', extra: 1 }],
        [2, { kind: 'literal', value: 'X' }],
      ])
    )
  );
});

test('Radio rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(RADIO_TAG, [
        [0, PATH('x')],
        [1, { kind: 'tstr', value: 'a' }],
        [2, { kind: 'literal', value: 'X' }],
        [99, 'rogue'],
      ])
    )
  );
});

test('Radio label BindRef updates label attr reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('view'), value: 'list' },
      { path: PATH('lbl'), value: 'Lista' },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(
    comp(RADIO_TAG, [
      [0, PATH('view')],
      [1, { kind: 'tstr', value: 'list' }],
      [2, { kind: 'bound', path: PATH('lbl') }],
    ])
  ));
  assertEq(el.getAttribute('label'), 'Lista');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'List' } }],
  });
  assertEq(el.getAttribute('label'), 'List');
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
