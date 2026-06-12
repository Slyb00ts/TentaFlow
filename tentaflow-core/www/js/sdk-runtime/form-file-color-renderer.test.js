// =============================================================================
// File: sdk-runtime/form-file-color-renderer.test.js
// Description: Tests for FileInput (0x0318) + ColorPicker (0x0319) rendered
// through the tf-file-input / tf-color-input / tf-input web components.
// =============================================================================

import './_dom-test-harness.js';
import { window as domWindow } from './_dom-test-harness.js';
// tf-input observes child mutations; the harness does not export the
// observer, so it is bridged here before the components are imported.
if (!globalThis.MutationObserver) globalThis.MutationObserver = domWindow.MutationObserver;
import '../components/tf-file-input.js';
import '../components/tf-color-input.js';
import '../components/tf-input.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  FILE_INPUT_TAG, COLOR_PICKER_TAG,
} from './form-file-color-renderer.js';

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

// ============================================================================
// FileInput
// ============================================================================

function fakeFile({ name = 'a.txt', type = 'text/plain', size = 100 } = {}) {
  // The validator only reads a minimal File-like interface.
  return { name, type, size, lastModified: 0 };
}

function dropFiles(el, files) {
  const dz = el.querySelector('.tf-file-input-dropzone');
  const ev = new (globalThis.Event)('drop', { bubbles: false, cancelable: true });
  ev.dataTransfer = { files };
  dz.dispatchEvent(ev);
}

test('FileInput renders <tf-file-input> with accept + internal dropzone', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('files')], [1, ['*/*']], [2, 1024 * 1024], [3, 1],
    [4, false], [5, false], [7, 'do_upload'],
    [8, { kind: 'literal', value: 'Pliki' }],
  ])));
  const fileEl = el.querySelector('tf-file-input');
  assert(fileEl != null, 'expected tf-file-input web component');
  const input = el.querySelector('input[type=file]');
  assertEq(input.getAttribute('accept'), '*/*');
  assert(el.querySelector('.tf-file-input-dropzone') != null);
});

test('FileInput multiple=true sets multiple attr + label from bind', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('files')], [1, []], [2, 1024], [3, 5],
    [4, true], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'Pliki' }],
  ])));
  assertEq(el.querySelector('input[type=file]').hasAttribute('multiple'), true);
  assertEq(el.querySelector('tf-file-input').getAttribute('label'), 'Pliki');
  assertEq(el.querySelector('.tf-file-input-label').textContent, 'Pliki');
});

test('FileInput multiple=false + max_files>1 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 5],
    [4, false], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
});

test('FileInput max_files=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 0],
    [4, false], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
});

test('FileInput drag_and_drop=false sets no-drop and ignores drops', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 10_000], [3, 1],
    [4, false], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  assert(el.querySelector('tf-file-input').hasAttribute('no-drop'));
  let chg = null;
  el.addEventListener('files_selected', (e) => { chg = e.detail; });
  dropFiles(el, [fakeFile()]);
  assertEq(chg, null);
});

test('FileInput capture=user sets capture attr on the native input', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, ['image/*']], [2, 1024 * 1024], [3, 1],
    [4, false], [5, false], [6, 'user'], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  assertEq(el.querySelector('tf-file-input').getAttribute('capture'), 'user');
  assertEq(el.querySelector('input[type=file]').getAttribute('capture'), 'user');
});

test('FileInput store push renders file list', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('files'), value: [{ name: 'a.txt', size: 1234 }, { name: 'b.pdf', size: 5_500_000 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('files')], [1, []], [2, 10 * 1024 * 1024], [3, 5],
    [4, true], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  const items = el.querySelectorAll('.tf-file-input__item');
  assertEq(items.length, 2);
  assertEq(items[0].querySelector('.tf-file-input__item-name').textContent, 'a.txt');
});

test('FileInput drop of unaccepted type → reject', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, ['image/*']], [2, 1024 * 1024], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  let rej = null;
  let chg = null;
  el.addEventListener('reject', (e) => { rej = e.detail; });
  el.addEventListener('files_selected', (e) => { chg = e.detail; });
  dropFiles(el, [fakeFile({ name: 'doc.pdf', type: 'application/pdf', size: 100 })]);
  assertEq(rej != null && rej.reason, 'accept');
  assertEq(chg, null);
});

test('FileInput drop with size > max → reject', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1000], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  let rej = null;
  el.addEventListener('reject', (e) => { rej = e.detail; });
  dropFiles(el, [fakeFile({ size: 5000 })]);
  assertEq(rej != null && rej.reason, 'max_size');
});

test('FileInput drop of valid file → files_selected with metadata + upload_action_id', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, ['text/*']], [2, 10_000], [3, 1],
    [4, false], [5, true], [7, 'do_upload'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  let chg = null;
  el.addEventListener('files_selected', (e) => { chg = e.detail; });
  dropFiles(el, [fakeFile({ name: 'note.txt', type: 'text/plain', size: 42 })]);
  assertEq(chg.kind, 'files');
  assertEq(chg.upload_action_id, 'do_upload');
  assertEq(chg.value, [{ name: 'note.txt', size: 42, type: 'text/plain', last_modified: 0 }]);
});

test('FileInput drop of > max_files → reject', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 10_000], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  let rej = null;
  el.addEventListener('reject', (e) => { rej = e.detail; });
  dropFiles(el, [fakeFile({ name: '1.txt' }), fakeFile({ name: '2.txt' })]);
  assertEq(rej != null && rej.reason, 'max_files');
});

test('FileInput raw component change never escapes the wrapper', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 10_000], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ])));
  let rawChange = null;
  let chg = null;
  el.addEventListener('change', (e) => { rawChange = e; });
  el.addEventListener('files_selected', (e) => { chg = e.detail; });
  dropFiles(el, [fakeFile()]);
  assertEq(rawChange, null);
  assert(chg != null && chg.kind === 'files');
});

test('FileInput unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 1],
    [4, false], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
    [99, 'x'],
  ])));
});

test('FileInput without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 1],
    [4, false], [5, false], [7, 'up'],
  ])));
});

test('FileInput a11y.label lands as aria-label on tf-file-input', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'Wybierz plik' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 1],
    [4, false], [5, false], [7, 'up'],
  ], { a11y: { label: { kind: 'bound', path: PATH('lbl') } } })));
  assertEq(el.querySelector('tf-file-input').getAttribute('aria-label'), 'Wybierz plik');
});

// ============================================================================
// ColorPicker
// ============================================================================

test('ColorPicker variant=wheel renders <tf-color-input>', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  assert(el.querySelector('tf-color-input') != null, 'expected tf-color-input web component');
  assertEq(el.querySelector('input').getAttribute('type'), 'color');
  assertEq(el.querySelector('tf-color-input').getAttribute('label'), 'C');
});

test('ColorPicker wheel syncs from #rrggbb store value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('c'), value: '#ff8800' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  assertEq(el.querySelector('tf-color-input').getAttribute('value'), '#ff8800');
  assertEq(el.querySelector('input').value, '#ff8800');
});

test('ColorPicker wheel expands #rgb and strips alpha for the native picker', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('c'), value: '#abc' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  assertEq(el.querySelector('tf-color-input').getAttribute('value'), '#aabbcc');
});

test('ColorPicker wheel change emits hex value via tf-bind-write', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '#00ff00';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(got, { value: '#00ff00', kind: 'hex' });
});

test('ColorPicker variant=swatch + default palette renders 16 swatches', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  assertEq(el.querySelectorAll('.tf-color-picker__swatch').length, 16);
});

test('ColorPicker variant=swatch with allowed_tokens uses tokens (NOT palette)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'],
    [2, ['accent_primary', 'tone_success', 'tone_critical']],
    [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  const swatches = el.querySelectorAll('.tf-color-picker__swatch');
  assertEq(swatches.length, 3);
  assertEq(swatches[0].getAttribute('data-token'), 'accent_primary');
});

test('ColorPicker variant=tokens_only without allowed_tokens throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'tokens_only'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
});

test('ColorPicker allowed_tokens=[] throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [2, []], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
});

test('ColorPicker invalid allowed_tokens value throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [2, ['not_a_token']], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
});

test('ColorPicker swatch click emits kind=token for tokens', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'tokens_only'],
    [2, ['accent_primary', 'tone_success']],
    [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  el.querySelectorAll('.tf-color-picker__swatch')[1].click();
  assertEq(got, { value: 'tone_success', kind: 'token' });
});

test('ColorPicker swatch click without tokens emits kind=hex', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  el.querySelector('.tf-color-picker__swatch').click();
  assertEq(got.kind, 'hex');
  assert(got.value.startsWith('#'));
});

test('ColorPicker compact adds a <tf-input> hex field', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  const hexEl = el.querySelector('.tf-color-picker__hex');
  assert(hexEl != null);
  assertEq(hexEl.tagName, 'TF-INPUT');
  assertEq(hexEl.getAttribute('maxlength'), '7');
});

test('ColorPicker compact valid hex commit emits tf-bind-write', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  const hexEl = el.querySelector('.tf-color-picker__hex');
  const inner = hexEl.querySelector('input');
  inner.value = '#123456';
  inner.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(got, { value: '#123456', kind: 'hex' });
});

test('ColorPicker compact hex input invalid value → revert', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('c'), value: '#abcdef' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  const hexEl = el.querySelector('.tf-color-picker__hex');
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  const inner = hexEl.querySelector('input');
  inner.value = 'not_a_hex';
  inner.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(got, null);
  assertEq(hexEl.value, '#abcdef');
});

test('ColorPicker compact show_alpha=false rejects 8-hex with alpha', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  const inner = el.querySelector('.tf-color-picker__hex input');
  inner.value = '#aabbccdd';
  inner.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(got, null);
});

test('ColorPicker unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
    [99, 'x'],
  ])));
});

test('ColorPicker never emits a public "change" — bind-write goes via tf-bind-write', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let publicChange = null;
  let internalWrite = null;
  el.addEventListener('change', (e) => { publicChange = e.detail; });
  el.addEventListener('tf-bind-write', (e) => { internalWrite = e.detail; });
  el.querySelector('.tf-color-picker__swatch').click();
  assertEq(publicChange, null);
  assert(internalWrite != null && internalWrite.kind === 'hex');
});

test('ColorPicker wheel change does NOT bubble as public change on the wrapper', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  let publicChange = null;
  el.addEventListener('change', (e) => { publicChange = e.detail; });
  const input = el.querySelector('input');
  input.value = '#aabbcc';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(publicChange, null);
});

test('ColorPicker compact hex change does NOT bubble as public change', () => {
  setup();
  const engine = makeEngine();
  const el = mount(engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ])));
  let publicChange = null;
  el.addEventListener('change', (e) => { publicChange = e.detail; });
  const inner = el.querySelector('.tf-color-picker__hex input');
  inner.value = '#ddccbb';
  inner.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(publicChange, null);
});

test('ColorPicker without label requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
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
