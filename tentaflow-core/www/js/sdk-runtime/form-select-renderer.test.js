// =============================================================================
// File: sdk-runtime/form-select-renderer.test.js
// Description: Tests for Select (0x0303) rendered through the <tf-select> web
// component with native <option>/<optgroup> children. Option values are
// serialized as "<kind>:<value>" strings; the renderer converts the component
// change back to the SDK { value, kind } SelectValue via the __tfReemit
// pattern (raw events are stopImmediatePropagation'd).
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-select.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { SELECT_TAG } from './form-select-renderer.js';

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
// tf-select consumes its light-DOM <option> children in connectedCallback,
// so tests that touch the inner native <select> mount the element first.
function mount(el) {
  document.body.appendChild(el);
  return el;
}

/// Helper: SelectOption as FieldMap [[key, value], ...].
function opt({ value, label, icon, disabled = false, groupId, description } = {}) {
  const f = [[0, value], [1, label], [3, disabled]];
  if (icon != null) f.push([2, icon]);
  if (groupId != null) f.push([4, groupId]);
  if (description != null) f.push([5, description]);
  return f;
}
function tstrOpt(v, lbl) {
  return opt({
    value: { kind: 'tstr', value: v },
    label: { kind: 'literal', value: lbl },
  });
}
function selectFields({ path = PATH('sel'), options = [], searchable = false, clearable = false, virtualize = false, size = 'md', ...rest } = {}) {
  const f = [
    [0, path], [1, options],
    [4, searchable], [5, clearable], [6, virtualize], [8, size],
  ];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}
/// Helper: pick an option on the inner native <select> and fire change.
function pick(el, serialized) {
  const select = el.querySelector('select');
  select.value = serialized;
  select.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
}

// ============================================================================
// Render + option serialization
// ============================================================================

test('Select renders <tf-select> with serialized option values', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'Frukty' },
  }))));
  assertEq(el.tagName.toLowerCase(), 'tf-select');
  assert(el.classList.contains('tf-select--size-md'));
  const options = el.querySelectorAll('option');
  assertEq(options.length, 2);
  assertEq(options[0].value, 'tstr:a');
  assertEq(options[0].textContent, 'Apple');
  assertEq(options[1].value, 'tstr:b');
  assertEq(options[1].textContent, 'Banana');
});

test('Select placeholder renders disabled empty-value option first', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'F' },
    2: { kind: 'literal', value: 'Wybierz...' },
  }))));
  const first = el.querySelector('option');
  assertEq(first.value, '');
  assertEq(first.disabled, true);
  assertEq(first.textContent, 'Wybierz...');
});

test('Select syncs value attr from store (selected option)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'b' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'F' },
  }))));
  assertEq(el.getAttribute('value'), 'tstr:b');
  assertEq(el.querySelector('select').value, 'tstr:b');
});

test('Select reactive value sync after store push', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'F' },
  }))));
  assertEq(el.getAttribute('value'), 'tstr:a');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(el.getAttribute('value'), 'tstr:b');
  assertEq(el.querySelector('select').value, 'tstr:b');
});

test('Select store value not in options resolves to empty value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'zzz' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'F' },
  })));
  assertEq(el.getAttribute('value'), '');
});

// ============================================================================
// Change re-emission (SDK SelectValue payload)
// ============================================================================

test('Select option pick re-emits change with { value, kind } SelectValue', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const got = [];
  el.addEventListener('change', (e) => got.push(e.detail));
  pick(el, 'tstr:b');
  // Exactly one SDK event — the raw serialized-string event must be blocked.
  assertEq(got, [{ value: 'b', kind: 'tstr' }]);
});

test('Select u32 option emits numeric value with kind=u32', () => {
  setup();
  const engine = makeEngine();
  const u32opt = opt({
    value: { kind: 'u32', value: 5 },
    label: { kind: 'literal', value: 'Pięć' },
  });
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [u32opt],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('option').value, 'u32:5');
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  pick(el, 'u32:5');
  assertEq(got, { value: 5, kind: 'u32' });
});

test('Select bool option emits boolean value with kind=bool', () => {
  setup();
  const engine = makeEngine();
  const boolOpt = opt({
    value: { kind: 'bool', value: true },
    label: { kind: 'literal', value: 'Tak' },
  });
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [boolOpt],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  pick(el, 'bool:true');
  assertEq(got, { value: true, kind: 'bool' });
});

test('Select empty selection with clearable=false emits nothing', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    clearable: false,
    2: { kind: 'literal', value: 'Wybierz...' },
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  pick(el, '');
  assertEq(got, null);
});

test('Select empty selection with clearable=true emits { value: null, kind: null }', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    clearable: true,
    2: { kind: 'literal', value: 'Wybierz...' },
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  pick(el, '');
  assertEq(got, { value: null, kind: null });
});

// ============================================================================
// Groups
// ============================================================================

test('Select groups render <optgroup> with labels + grouped options', () => {
  setup();
  const engine = makeEngine();
  const grp1 = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const grp2 = [[0, 'vg'], [1, { kind: 'literal', value: 'Warzywa' }]];
  const apple = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'Apple' }],
    [3, false], [4, 'fr'],
  ];
  const carrot = [
    [0, { kind: 'tstr', value: 'c' }],
    [1, { kind: 'literal', value: 'Carrot' }],
    [3, false], [4, 'vg'],
  ];
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [apple, carrot],
    9: [grp1, grp2],
    3: { kind: 'literal', value: 'L' },
  })));
  // Renderer output contract (pre-mount light DOM): optgroup per group with
  // the resolved label and the assigned options inside.
  const groups = el.querySelectorAll('optgroup');
  assertEq(groups.length, 2);
  assertEq(groups[0].getAttribute('label'), 'Owoce');
  assertEq(groups[1].getAttribute('label'), 'Warzywa');
  assertEq(groups[0].querySelector('option').value, 'tstr:a');
  assertEq(groups[1].querySelector('option').value, 'tstr:c');
  // After mount tf-select harvests the options into its native <select>;
  // both grouped options must survive and stay selectable.
  mount(el);
  const opts = el.querySelectorAll('select option');
  assertEq(opts.length, 2);
  assertEq(opts[0].value, 'tstr:a');
  assertEq(opts[1].value, 'tstr:c');
});

test('Select option with unknown group_id throws', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const bad = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'A' }],
    [3, false], [4, 'unknown_id'],
  ];
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [bad], 9: [grp],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select group with missing id/label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [], 9: [[[1, { kind: 'literal', value: 'X' }]]],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [], 9: [[[0, 'gid']]],
    3: { kind: 'literal', value: 'L' },
  }))));
});

// ============================================================================
// Disabled
// ============================================================================

test('Select disabled BindRef sets disabled attr + disables inner select', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  assertEq(el.hasAttribute('disabled'), true);
  assertEq(el.querySelector('select').disabled, true);
  // Flip off — re-enabled reactively.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: false } }],
  });
  assertEq(el.hasAttribute('disabled'), false);
  assertEq(el.querySelector('select').disabled, false);
});

test('Select disabled suppresses change re-emission', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('sel'), value: 'a' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  pick(el, 'tstr:b');
  assertEq(got, null);
});

// ============================================================================
// Label / a11y
// ============================================================================

test('Select label sets label + aria-label attrs (reactive)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Frukty' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'bound', path: PATH('lbl') },
  }))));
  assertEq(el.getAttribute('label'), 'Frukty');
  assertEq(el.getAttribute('aria-label'), 'Frukty');
  assertEq(el.querySelector('.tf-label').textContent, 'Frukty');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'Fruits' } }],
  });
  assertEq(el.getAttribute('label'), 'Fruits');
});

test('Select without label mirrors a11y.label as aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Wybierz' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
  }), { a11y: { label: { kind: 'bound', path: PATH('lbl') } } }));
  assertEq(el.getAttribute('aria-label'), 'Wybierz');
  assertEq(el.hasAttribute('label'), false);
});

test('Select without label and without a11y.label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
  }))));
});

test('Select a11y.label rejects whitespace-only initial value', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A')],
  }), { a11y: { label: { kind: 'literal', value: '  ' } } })));
});

// ============================================================================
// SelectValue / SelectOption validation
// ============================================================================

test('Select u32 SelectValue accepts BigInt in range', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [opt({
      value: { kind: 'u32', value: 4294967295n },
      label: { kind: 'literal', value: 'Max' },
    })],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('option').value, 'u32:4294967295');
});

test('Select u32 SelectValue rejects out-of-range BigInt', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [opt({
      value: { kind: 'u32', value: 4294967296n },
      label: { kind: 'literal', value: 'X' },
    })],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select i32 SelectValue accepts negative BigInt and rejects overflow', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [opt({
      value: { kind: 'i32', value: -2147483648n },
      label: { kind: 'literal', value: 'Min' },
    })],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('option').value, 'i32:-2147483648');
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [opt({
      value: { kind: 'i32', value: 2147483648n },
      label: { kind: 'literal', value: 'X' },
    })],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select SelectValue with unknown kind throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [opt({
      value: { kind: 'float', value: 1.5 },
      label: { kind: 'literal', value: 'X' },
    })],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select option missing value/label throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [[[1, { kind: 'literal', value: 'X' }], [3, false]]],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [[[0, { kind: 'tstr', value: 'a' }], [3, false]]],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select option with duplicate field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [[
      [0, { kind: 'tstr', value: 'a' }],
      [0, { kind: 'tstr', value: 'b' }],
      [1, { kind: 'literal', value: 'X' }],
    ]],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select disabled option renders disabled <option>', () => {
  setup();
  const engine = makeEngine();
  const disabledOpt = [
    [0, { kind: 'tstr', value: 'x' }],
    [1, { kind: 'literal', value: 'X' }],
    [3, true],
  ];
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [disabledOpt, tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const opts = el.querySelectorAll('option');
  assertEq(opts[0].disabled, true);
  assertEq(opts[1].disabled, false);
});

// ============================================================================
// Field validation
// ============================================================================

test('Select unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, [
    [0, PATH('sel')], [1, []], [4, false], [5, false], [6, false], [8, 'md'],
    [3, { kind: 'literal', value: 'L' }],
    [99, 'oops'],
  ])));
});

test('Select invalid size throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, selectFields({
    options: [], size: 'xl',
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Select non-boolean searchable/clearable/virtualize throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(SELECT_TAG, [
    [0, PATH('sel')], [1, []], [4, 'yes'], [5, false], [6, false], [8, 'md'],
    [3, { kind: 'literal', value: 'L' }],
  ])));
});

test('Select destroy unbinds store subscription', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('sel'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(SELECT_TAG, selectFields({
    options: [tstrOpt('a', 'A'), tstrOpt('b', 'B')],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.getAttribute('value'), 'tstr:a');
  engine.destroy(el);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('sel'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(el.getAttribute('value'), 'tstr:a');
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
