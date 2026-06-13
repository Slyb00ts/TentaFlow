// =============================================================================
// File: sdk-runtime/form-tag-mention-renderer.test.js
// Description: Tests for TagInput (0x0308) + MentionInput (0x0309) rendered
// through the tf-tag-input / tf-mention-input web components.
// =============================================================================

import './_dom-test-harness.js';
import '../components/tf-chip.js';
import '../components/tf-tag-input.js';
import '../components/tf-mention-input.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  TAGINPUT_TAG,
  MENTIONINPUT_TAG,
  registerFormTagMentionRenderers,
} from './form-tag-mention-renderer.js';

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
  // The renderer under test is not yet wired into bootstrap — register it here.
  registerFormTagMentionRenderers();
  document.body.innerHTML = '';
}
function mount(el) {
  document.body.appendChild(el);
  return el;
}

/// Captures re-emitted SDK events (those tagged `__tfReemit`) on a host.
function capture(el, name) {
  const seen = [];
  el.addEventListener(name, (e) => { if (e.__tfReemit) seen.push(e.detail); });
  return seen;
}

function key(el, k, opts = {}) {
  el.dispatchEvent(new (globalThis.KeyboardEvent || globalThis.Event)('keydown', {
    key: k, bubbles: true, cancelable: true, ...opts,
  }));
}

// ----------------------------------------------------------------------------
// TagInput fields helper. Required keys: 0 values_path, 2 validators,
// 4 separator, 5 dedupe.
// ----------------------------------------------------------------------------
function tagFields({
  path = PATH('tags'), validators = [], separator = [','], dedupe = false, ...rest
} = {}) {
  const f = [[0, path], [2, validators], [4, separator], [5, dedupe]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}

function mentionFields({
  bind = PATH('text'), mentions = PATH('cands'), triggers = ['@'],
  actionId = 'do_mention', ...rest
} = {}) {
  const f = [[0, bind], [1, mentions], [2, triggers], [3, actionId]];
  for (const [k, v] of Object.entries(rest)) {
    const ki = Number(k);
    if (!Number.isInteger(ki)) continue;
    f.push([ki, v]);
  }
  return f;
}

function seededTagEngine(tags) {
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('tags'), value: tags }], state_revision: 0, truncated: false });
  return { store, engine: makeEngine(store) };
}

// ============================================================================
// TagInput — render / happy path
// ============================================================================

test('TagInput renders tf-tag-input host', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 1: LIT('Add tag') }))));
  assertEq(el.tagName, 'TF-TAG-INPUT');
  assertEq(el.getAttribute('placeholder'), 'Add tag');
});

test('TagInput feeds chips from the store', () => {
  setup();
  const { engine } = seededTagEngine(['alpha', 'beta']);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 1: LIT('p') }))));
  assertEq(el.querySelectorAll('tf-chip').length, 2);
  assertEq(el.tags, ['alpha', 'beta']);
});

test('TagInput add via Enter emits add + change in SDK shape', () => {
  setup();
  const { engine } = seededTagEngine([]);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 1: LIT('p') }))));
  const adds = capture(el, 'add');
  const changes = capture(el, 'change');
  const input = el.querySelector('.tf-tag-input-entry');
  input.value = 'hello';
  key(input, 'Enter');
  assertEq(adds, [{ value: 'hello', tags: ['hello'] }]);
  assertEq(changes, [{ tags: ['hello'], kind: 'array' }]);
});

test('TagInput add via separator key commits the tag', () => {
  setup();
  const { engine } = seededTagEngine([]);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ separator: [','], 1: LIT('p') }))));
  const adds = capture(el, 'add');
  const input = el.querySelector('.tf-tag-input-entry');
  input.value = 'x';
  key(input, ',');
  assertEq(adds, [{ value: 'x', tags: ['x'] }]);
});

test('TagInput remove via chip × emits remove + change', () => {
  setup();
  const { engine } = seededTagEngine(['a', 'b', 'c']);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 1: LIT('p') }))));
  const removes = capture(el, 'remove');
  const changes = capture(el, 'change');
  const chips = el.querySelectorAll('tf-chip');
  chips[1].dispatchEvent(new CustomEvent('remove'));
  assertEq(removes, [{ value: 'b', index: 1, tags: ['a', 'c'] }]);
  assertEq(changes, [{ tags: ['a', 'c'], kind: 'array' }]);
});

test('TagInput remove via Backspace on empty entry removes the last tag', () => {
  setup();
  const { engine } = seededTagEngine(['a', 'b']);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 1: LIT('p') }))));
  const removes = capture(el, 'remove');
  const input = el.querySelector('.tf-tag-input-entry');
  input.value = '';
  key(input, 'Backspace');
  assertEq(removes, [{ value: 'b', index: 1, tags: ['a'] }]);
});

test('TagInput dedupe rejects a duplicate (no add emitted)', () => {
  setup();
  const { engine } = seededTagEngine(['a']);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ dedupe: true, 1: LIT('p') }))));
  const adds = capture(el, 'add');
  const input = el.querySelector('.tf-tag-input-entry');
  input.value = 'a';
  key(input, 'Enter');
  assertEq(adds, []);
});

test('TagInput max_tags blocks add beyond the cap', () => {
  setup();
  const { engine } = seededTagEngine(['a']);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 3: 1, 1: LIT('p') }))));
  const adds = capture(el, 'add');
  const input = el.querySelector('.tf-tag-input-entry');
  input.value = 'b';
  key(input, 'Enter');
  assertEq(adds, []);
});

test('TagInput validator (max_length) rejects an over-long tag', () => {
  setup();
  const { engine } = seededTagEngine([]);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({
    validators: [{ kind: 'max_length', value: 3 }], 1: LIT('p'),
  }))));
  const adds = capture(el, 'add');
  const input = el.querySelector('.tf-tag-input-entry');
  input.value = 'toolong';
  key(input, 'Enter');
  assertEq(adds, []);
  input.value = 'ok';
  key(input, 'Enter');
  assertEq(adds, [{ value: 'ok', tags: ['ok'] }]);
});

test('TagInput accepts BigInt max_tags (CBOR u32)', () => {
  setup();
  const { engine } = seededTagEngine([]);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 3: 2n, 1: LIT('p') }))));
  assertEq(el.getAttribute('max-tags'), '2');
});

test('TagInput reactive bind: store patch re-feeds chips', () => {
  setup();
  const { store, engine } = seededTagEngine(['a']);
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields({ 1: LIT('p') }))));
  assertEq(el.querySelectorAll('tf-chip').length, 1);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('tags'), op: { kind: 'set', value: ['a', 'b', 'c'] } }],
  });
  assertEq(el.querySelectorAll('tf-chip').length, 3);
  assertEq(el.tags, ['a', 'b', 'c']);
});

test('TagInput without placeholder requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TAGINPUT_TAG, tagFields())));
});

test('TagInput without placeholder accepts a11y.label', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(TAGINPUT_TAG, tagFields(), { a11y: { label: LIT('Tags') } })));
  assertEq(el.getAttribute('aria-label'), 'Tags');
});

test('TagInput unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TAGINPUT_TAG, [...tagFields({ 1: LIT('p') }), [99, 'x']])));
});

test('TagInput max_tags=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TAGINPUT_TAG, tagFields({ 3: 0, 1: LIT('p') }))));
});

// ============================================================================
// MentionInput — render / happy path
// ============================================================================

test('MentionInput renders tf-mention-input host', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(MENTIONINPUT_TAG, mentionFields({ 4: LIT('Say hi') }))));
  assertEq(el.tagName, 'TF-MENTION-INPUT');
  assertEq(el.getAttribute('placeholder'), 'Say hi');
});

test('MentionInput feeds text from the store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('text'), value: 'draft' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MENTIONINPUT_TAG, mentionFields({ 4: LIT('p') }))));
  assertEq(el.value, 'draft');
});

test('MentionInput @-trigger opens popover and emits search', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('text'), value: '' },
      { path: PATH('cands'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MENTIONINPUT_TAG, mentionFields({ 4: LIT('p') }))));
  const searches = capture(el, 'search');
  const area = el.querySelector('.tf-mention-input-area');
  area.value = '@bo';
  area.selectionStart = 3;
  area.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  assertEq(searches, [{ trigger: '@', query: 'bo', action_id: 'do_mention' }]);
  assertEq(area.getAttribute('aria-expanded'), 'true');
});

test('MentionInput suggestion select inserts mention + emits mention/change', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('text'), value: '' },
      { path: PATH('cands'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MENTIONINPUT_TAG, mentionFields({ 4: LIT('p') }))));
  const mentions = capture(el, 'mention');
  const changes = capture(el, 'change');
  const area = el.querySelector('.tf-mention-input-area');
  area.value = '@bo';
  area.selectionStart = 3;
  area.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  // Host pushes candidates after the search.
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('cands'), op: { kind: 'set', value: [{ id: 'u1', label: 'Bob' }] } }],
  });
  const opt = el.querySelectorAll('.tf-mention-input-option')[0];
  opt.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, cancelable: true }));
  assertEq(mentions, [{ id: 'u1', label: 'Bob', trigger: '@', action_id: 'do_mention' }]);
  assert(area.value.startsWith('@Bob '), 'mention inserted into textarea');
  assert(changes.length >= 1, 'change emitted on select');
  const last = changes[changes.length - 1];
  assertEq(last.kind, 'tstr');
});

test('MentionInput reactive bind: candidates patch re-feeds popover', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('text'), value: '' },
      { path: PATH('cands'), value: [] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MENTIONINPUT_TAG, mentionFields({ 4: LIT('p') }))));
  const area = el.querySelector('.tf-mention-input-area');
  area.value = '@';
  area.selectionStart = 1;
  area.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  assertEq(el.querySelectorAll('.tf-mention-input-option').length, 0);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('cands'), op: { kind: 'set', value: [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }] } }],
  });
  assertEq(el.querySelectorAll('.tf-mention-input-option').length, 2);
});

test('MentionInput change emits {value, kind: tstr}', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('text'), value: '' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(MENTIONINPUT_TAG, mentionFields({ 4: LIT('p') }))));
  const changes = capture(el, 'change');
  const area = el.querySelector('.tf-mention-input-area');
  area.value = 'plain text';
  area.selectionStart = 10;
  area.dispatchEvent(new (globalThis.Event)('input', { bubbles: true }));
  assertEq(changes, [{ value: 'plain text', kind: 'tstr' }]);
});

test('MentionInput without placeholder requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MENTIONINPUT_TAG, mentionFields())));
});

test('MentionInput empty trigger_chars throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MENTIONINPUT_TAG, mentionFields({ triggers: [], 4: LIT('p') }))));
});

test('MentionInput multi-char trigger throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MENTIONINPUT_TAG, mentionFields({ triggers: ['@@'], 4: LIT('p') }))));
});

test('MentionInput unknown field key throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(MENTIONINPUT_TAG, [...mentionFields({ 4: LIT('p') }), [99, 'x']])));
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
