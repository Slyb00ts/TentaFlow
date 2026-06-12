// =============================================================================
// File: sdk-runtime/form-combobox-renderer.test.js
// Description: Tests for Combobox (0x0305) + Autocomplete (0x0306) rendered
// through the tf-combobox web component.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-combobox.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { COMBOBOX_TAG, AUTOCOMPLETE_TAG } from './form-combobox-renderer.js';

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
function mount(el) {
  document.body.appendChild(el);
  return el;
}

/// Deterministic timer stub — debounce tests flush manually instead of
/// waiting on real timers.
function withFakeTimers(fn) {
  const timers = [];
  const realSetTimeout = globalThis.setTimeout;
  const realClearTimeout = globalThis.clearTimeout;
  // Ids start at 1 — debounce code treats the handle as truthy.
  globalThis.setTimeout = (cb, ms) => {
    timers.push({ cb, ms, cancelled: false });
    return timers.length;
  };
  globalThis.clearTimeout = (id) => {
    if (typeof id === 'number' && timers[id - 1]) timers[id - 1].cancelled = true;
    else realClearTimeout(id);
  };
  const flush = () => {
    for (const t of timers.splice(0)) {
      if (!t.cancelled) t.cb();
    }
  };
  try {
    fn(flush);
  } finally {
    globalThis.setTimeout = realSetTimeout;
    globalThis.clearTimeout = realClearTimeout;
  }
}

function tstrOpt(v, lbl, opts = {}) {
  const f = [
    [0, { kind: 'tstr', value: v }],
    [1, { kind: 'literal', value: lbl }],
    [3, opts.disabled === true],
  ];
  if (opts.groupId != null) f.push([4, opts.groupId]);
  return f;
}
function cbFields({
  path = PATH('q'), options = [], clearable = false, virtualize = false,
  size = 'md', freeInput = false, minChars = 0, remoteSearch = false, ...rest
} = {}) {
  const f = [
    [0, path], [1, options], [4, true], [5, clearable], [6, virtualize],
    [8, size], [10, freeInput], [11, minChars], [12, remoteSearch],
  ];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}
function acFields({
  path = PATH('q'), actionId = 'do_search', minChars = 1, debounceMs = 100, ...rest
} = {}) {
  const f = [[0, path], [1, actionId], [3, minChars], [4, debounceMs]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}
function keydown(el, key, mods = {}) {
  const ev = new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key, bubbles: false, cancelable: true, ...mods,
  });
  el.dispatchEvent(ev);
  return ev;
}
function typeText(input, text) {
  input.value = text;
  input.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
}

// ============================================================================
// Combobox (0x0305)
// ============================================================================

test('Combobox renders tf-combobox with inner input role=combobox + aria-autocomplete=list', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.tagName, 'TF-COMBOBOX');
  const input = el.querySelector('input.tf-combobox-input');
  assertEq(input.getAttribute('role'), 'combobox');
  assertEq(input.getAttribute('aria-autocomplete'), 'list');
  assertEq(input.getAttribute('aria-expanded'), 'false');
});

test('Combobox label bind lands as label attribute + visible label element', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
    3: { kind: 'literal', value: 'Etykieta' },
  }))));
  assertEq(el.getAttribute('label'), 'Etykieta');
  assertEq(el.querySelector('.tf-combobox-label').textContent, 'Etykieta');
});

test('Combobox searchable=false throws', () => {
  setup();
  const engine = makeEngine();
  const f = cbFields({ options: [], 3: { kind: 'literal', value: 'L' } })
    .map(([k, v]) => (k === 4 ? [4, false] : [k, v]));
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, f)));
});

test('Combobox shows the selected option label in input.value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'b' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  assertEq(el.querySelector('input').value, 'Banana');
});

test('Combobox typing filters options + opens popover', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana'), tstrOpt('c', 'Cherry')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  typeText(input, 'an');
  assertEq(el.querySelector('.tf-combobox-popover').hidden, false);
  assertEq(input.getAttribute('aria-expanded'), 'true');
  const opts = el.querySelectorAll('.tf-combobox-option');
  assertEq(opts[0].hidden, true);   // Apple
  assertEq(opts[1].hidden, false);  // Banana
  assertEq(opts[2].hidden, true);   // Cherry
});

test('Combobox min_search_chars gate — popover does NOT open below the limit', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    minChars: 2,
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  typeText(input, 'a');
  assertEq(el.querySelector('.tf-combobox-popover').hidden, true);
  typeText(input, 'ap');
  assertEq(el.querySelector('.tf-combobox-popover').hidden, false);
});

test('Combobox Enter on the active option commits SDK SelectValue + closes', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  typeText(input, 'a');  // opens, active = first visible
  keydown(input, 'ArrowDown');  // active = Banana
  keydown(input, 'Enter');
  assertEq(got, { value: 'b', kind: 'tstr' });
  assertEq(el.querySelector('.tf-combobox-popover').hidden, true);
});

test('Combobox free_input=true Enter with non-option text emits change tstr', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    freeInput: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  typeText(input, 'xyz');  // no option matches → no active option
  keydown(input, 'Enter');
  assertEq(got, { value: 'xyz', kind: 'tstr' });
});

test('Combobox free_input=false Enter with raw text does NOT commit', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    freeInput: false,
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  typeText(input, 'xyz');
  keydown(input, 'Enter');
  assertEq(got, null);
});

test('Combobox remote_search=true requires remote_action_id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [],
    remoteSearch: true,
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Combobox remote_search emits "search" after debounce', () => {
  setup();
  const engine = makeEngine();
  withFakeTimers((flush) => {
    const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
      options: [],
      remoteSearch: true,
      minChars: 2,
      3: { kind: 'literal', value: 'L' },
      13: 'remote_query',
    }))));
    let got = null;
    el.addEventListener('search', (e) => { got = e.detail; });
    typeText(el.querySelector('input'), 'foo');
    assertEq(got, null);  // not before the debounce fires
    flush();
    assertEq(got, { query: 'foo', action_id: 'remote_query' });
  });
});

test('Combobox typing → quick backspace below minChars cancels remote search', () => {
  setup();
  const engine = makeEngine();
  withFakeTimers((flush) => {
    const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
      options: [],
      remoteSearch: true,
      minChars: 3,
      3: { kind: 'literal', value: 'L' },
      13: 'do_q',
    }))));
    let got = null;
    el.addEventListener('search', (e) => { got = e.detail; });
    const input = el.querySelector('input');
    typeText(input, 'foo');
    typeText(input, 'f');  // drop below minChars before the debounce fires
    flush();
    assertEq(got, null);
  });
});

test('Combobox store push does NOT clobber input.value while typing (focused)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: '' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  input.focus();
  typeText(input, 'typ');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'b' } }],
  });
  assertEq(input.value, 'typ');
});

test('Combobox option mousedown commits the SelectValue', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple'), tstrOpt('b', 'Banana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  typeText(el.querySelector('input'), 'b');
  const second = el.querySelectorAll('.tf-combobox-option')[1];
  second.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
  assertEq(got, { value: 'b', kind: 'tstr' });
});

test('Combobox disabled option is not committable', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('x', 'Xapple', { disabled: true }), tstrOpt('b', 'Xbanana')],
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  typeText(input, 'x');  // both match, first is disabled → active = second
  keydown(input, 'Enter');
  assertEq(got, { value: 'b', kind: 'tstr' });
});

test('Combobox clear button emits change=null + clears the input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'a' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const clear = el.querySelector('.tf-combobox-clear');
  assertEq(clear.hidden, false);
  clear.click();
  assertEq(got, { value: null, kind: null });
  assertEq(el.querySelector('input').value, '');
});

test('Combobox disabled BindRef disables input + clear', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('q'), value: 'a' }, { path: PATH('lock'), value: true }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    clearable: true,
    3: { kind: 'literal', value: 'L' },
    7: { kind: 'bound', path: PATH('lock') },
  }))));
  assertEq(el.querySelector('input').disabled, true);
  assertEq(el.querySelector('.tf-combobox-clear').disabled, true);
  // Flip OFF — input enabled again.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lock'), op: { kind: 'set', value: false } }],
  });
  assertEq(el.querySelector('input').disabled, false);
});

test('Combobox option icon renders SVG via shared icon renderer', () => {
  setup();
  const engine = makeEngine();
  const withIcon = [
    [0, { kind: 'tstr', value: 'a' }],
    [1, { kind: 'literal', value: 'Apple' }],
    [2, { kind: 'named', name: 'check' }],
    [3, false],
  ];
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [withIcon],
    3: { kind: 'literal', value: 'L' },
  }))));
  const svg = el.querySelector('.tf-combobox-option-icon svg.tf-icon');
  assert(svg != null, 'expected SVG rendered by icon-renderer for IconRef::Named');
});

test('Combobox groups render group headers with resolved labels', () => {
  setup();
  const engine = makeEngine();
  const grp1 = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple', { groupId: 'fr' })],
    9: [grp1],
    3: { kind: 'literal', value: 'L' },
  }))));
  const header = el.querySelector('.tf-combobox-group-header');
  assertEq(header.textContent, 'Owoce');
  assert(el.querySelector('.tf-combobox-group .tf-combobox-option') != null);
});

test('Combobox option with unknown group_id throws', () => {
  setup();
  const engine = makeEngine();
  const grp = [[0, 'fr'], [1, { kind: 'literal', value: 'Owoce' }]];
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A', { groupId: 'unknown_id' })],
    9: [grp],
    3: { kind: 'literal', value: 'L' },
  }))));
});

test('Combobox without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
  }))));
});

test('Combobox a11y.label lands as aria-label on the host', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Szukaj' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'A')],
  }), { a11y: { label: { kind: 'bound', path: PATH('lbl') } } }));
  assertEq(el.getAttribute('aria-label'), 'Szukaj');
});

test('Combobox unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COMBOBOX_TAG, [
    ...cbFields({ options: [], 3: { kind: 'literal', value: 'L' } }),
    [99, 'oops'],
  ])));
});

test('Combobox Escape closes the popover', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const input = el.querySelector('input');
  typeText(input, 'a');
  assertEq(el.querySelector('.tf-combobox-popover').hidden, false);
  keydown(input, 'Escape');
  assertEq(el.querySelector('.tf-combobox-popover').hidden, true);
  assertEq(input.getAttribute('aria-expanded'), 'false');
});

test('Combobox raw component change never reaches listeners (only SDK shape)', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COMBOBOX_TAG, cbFields({
    options: [tstrOpt('a', 'Apple')],
    3: { kind: 'literal', value: 'L' },
  }))));
  const shapes = [];
  el.addEventListener('change', (e) => { shapes.push(e.detail); });
  typeText(el.querySelector('input'), 'a');
  el.querySelector('.tf-combobox-option').dispatchEvent(
    new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true })
  );
  // Exactly ONE event in the SDK shape — never the raw {value: idx, label}.
  assertEq(shapes, [{ value: 'a', kind: 'tstr' }]);
});

// ============================================================================
// Autocomplete (0x0306)
// ============================================================================

test('Autocomplete renders tf-combobox with inner input role=combobox', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'Szukaj' },
  }))));
  assertEq(el.tagName, 'TF-COMBOBOX');
  assert(el.classList.contains('tf-autocomplete'));
  const input = el.querySelector('input');
  assertEq(input.getAttribute('role'), 'combobox');
  assertEq(input.getAttribute('aria-autocomplete'), 'list');
});

test('Autocomplete debounce_ms=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    debounceMs: 0,
    6: { kind: 'literal', value: 'S' },
  }))));
});

test('Autocomplete typing emits SDK input event immediately', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'S' },
  }))));
  let got = null;
  el.addEventListener('input', (e) => { got = e.detail; });
  typeText(el.querySelector('input'), 'qu');
  assertEq(got, { value: 'qu', kind: 'tstr' });
});

test('Autocomplete typing emits search after debounce', () => {
  setup();
  const engine = makeEngine();
  withFakeTimers((flush) => {
    const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
      actionId: 'find_user', minChars: 2, debounceMs: 50,
      2: 'user_results',
      6: { kind: 'literal', value: 'S' },
    }))));
    let got = null;
    el.addEventListener('search', (e) => { got = e.detail; });
    typeText(el.querySelector('input'), 'jan');
    assertEq(got, null);
    flush();
    assertEq(got, { query: 'jan', action_id: 'find_user', result_template_id: 'user_results' });
  });
});

test('Autocomplete below minChars does NOT emit search', () => {
  setup();
  const engine = makeEngine();
  withFakeTimers((flush) => {
    const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
      minChars: 3,
      6: { kind: 'literal', value: 'S' },
    }))));
    let got = null;
    el.addEventListener('search', (e) => { got = e.detail; });
    typeText(el.querySelector('input'), 'ab');
    flush();
    assertEq(got, null);
  });
});

test('Autocomplete backspace below minChars cancels a pending search', () => {
  setup();
  const engine = makeEngine();
  withFakeTimers((flush) => {
    const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
      minChars: 3,
      6: { kind: 'literal', value: 'S' },
    }))));
    let got = null;
    el.addEventListener('search', (e) => { got = e.detail; });
    const input = el.querySelector('input');
    typeText(input, 'abc');
    typeText(input, 'a');
    flush();
    assertEq(got, null);
  });
});

test('Autocomplete result_template_id sets aria-controls + data-attr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    2: 'res_tpl',
    6: { kind: 'literal', value: 'S' },
  }), { id: 'c9' }));
  assertEq(el.getAttribute('data-result-template-id'), 'res_tpl');
  assertEq(el.getAttribute('aria-controls'), 'tf-autocomplete-c9-res_tpl');
});

test('Autocomplete store push syncs input value (one-way read)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('q'), value: 'init' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'S' },
  }))));
  assertEq(el.querySelector('input').value, 'init');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('q'), op: { kind: 'set', value: 'next' } }],
  });
  assertEq(el.querySelector('input').value, 'next');
});

test('Autocomplete change re-emits SDK {value, kind:tstr}', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'S' },
  }))));
  let got = null;
  el.addEventListener('change', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = 'final text';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(got, { value: 'final text', kind: 'tstr' });
});

test('Autocomplete focus/blur translated to SDK focus/blur events', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(AUTOCOMPLETE_TAG, acFields({
    6: { kind: 'literal', value: 'S' },
  }))));
  let focused = false;
  let blurred = false;
  el.addEventListener('focus', () => { focused = true; });
  el.addEventListener('blur', () => { blurred = true; });
  const input = el.querySelector('input');
  input.dispatchEvent(new (globalThis.Event)('focusin', { bubbles: true }));
  input.dispatchEvent(new (globalThis.Event)('focusout', { bubbles: true }));
  assertEq(focused, true);
  assertEq(blurred, true);
});

test('Autocomplete without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AUTOCOMPLETE_TAG, acFields())));
});

test('Autocomplete unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AUTOCOMPLETE_TAG, [
    ...acFields({ 6: { kind: 'literal', value: 'S' } }),
    [99, 'x'],
  ])));
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
