// =============================================================================
// Plik: sdk-runtime/form-file-color-renderer.test.js
// Opis: Testy FileInput (0x0318) + ColorPicker (0x0319) — chunk 3.3c-6.
// =============================================================================

import './_dom-test-harness.js';
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

// ============================================================================
// FileInput
// ============================================================================

function fakeFile({ name = 'a.txt', type = 'text/plain', size = 100 } = {}) {
  // happy-dom File konstruktor jest dostępny ale ma quirks z size;
  // używamy plain obiektu z minimalnym interface'm który walidator czyta.
  return { name, type, size, lastModified: 0 };
}

test('FileInput renderuje <input type=file> + dropzone', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('files')], [1, ['*/*']], [2, 1024 * 1024], [3, 1],
    [4, false], [5, false], [7, 'do_upload'],
    [8, { kind: 'literal', value: 'Pliki' }],
  ]));
  const input = el.querySelector('input[type=file]');
  assertEq(input.getAttribute('accept'), '*/*');
  assert(el.querySelector('.tf-file-input__dropzone') != null);
});

test('FileInput multiple=true ustawia multiple attr + label tekst', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('files')], [1, []], [2, 1024], [3, 5],
    [4, true], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'Pliki' }],
  ]));
  assertEq(el.querySelector('input').hasAttribute('multiple'), true);
  assertEq(el.querySelector('.tf-file-input__trigger').textContent, 'Wybierz pliki');
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

test('FileInput drag_and_drop=true ustawia klasę dropzone--dnd', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  assert(el.querySelector('.tf-file-input__dropzone--dnd') != null);
  assert(el.querySelector('.tf-file-input__dnd-hint') != null);
});

test('FileInput capture=user ustawia capture attr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, ['image/*']], [2, 1024 * 1024], [3, 1],
    [4, false], [5, false], [6, 'user'], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  assertEq(el.querySelector('input').getAttribute('capture'), 'user');
});

test('FileInput store push renderuje listę plików', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('files'), value: [{ name: 'a.txt', size: 1234 }, { name: 'b.pdf', size: 5_500_000 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('files')], [1, []], [2, 10 * 1024 * 1024], [3, 5],
    [4, true], [5, false], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  const items = el.querySelectorAll('.tf-file-input__item');
  assertEq(items.length, 2);
  assertEq(items[0].querySelector('.tf-file-input__item-name').textContent, 'a.txt');
});

test('FileInput drag-drop nieakceptowany typ → reject', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, ['image/*']], [2, 1024 * 1024], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  let rej = null;
  let chg = null;
  el.addEventListener('reject', (e) => { rej = e.detail; });
  el.addEventListener('files_selected', (e) => { chg = e.detail; });
  const dz = el.querySelector('.tf-file-input__dropzone');
  const file = fakeFile({ name: 'doc.pdf', type: 'application/pdf', size: 100 });
  const dragEvent = new (globalThis.Event || function () {})('drop', { bubbles: false, cancelable: true });
  dragEvent.dataTransfer = { files: [file] };
  dragEvent.preventDefault = () => {};
  dz.dispatchEvent(dragEvent);
  assertEq(rej != null && rej.reason, 'accept');
  assertEq(chg, null);
});

test('FileInput drag-drop size > max → reject', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1000], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  let rej = null;
  el.addEventListener('reject', (e) => { rej = e.detail; });
  const dz = el.querySelector('.tf-file-input__dropzone');
  const file = fakeFile({ size: 5000 });
  const ev = new (globalThis.Event)('drop', { bubbles: false, cancelable: true });
  ev.dataTransfer = { files: [file] };
  ev.preventDefault = () => {};
  dz.dispatchEvent(ev);
  assertEq(rej != null && rej.reason, 'max_size');
});

test('FileInput drag-drop valid file → files_selected z metadata + upload_action_id', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, ['text/*']], [2, 10_000], [3, 1],
    [4, false], [5, true], [7, 'do_upload'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  let chg = null;
  el.addEventListener('files_selected', (e) => { chg = e.detail; });
  const dz = el.querySelector('.tf-file-input__dropzone');
  const file = fakeFile({ name: 'note.txt', type: 'text/plain', size: 42 });
  const ev = new (globalThis.Event)('drop', { bubbles: false, cancelable: true });
  ev.dataTransfer = { files: [file] };
  ev.preventDefault = () => {};
  dz.dispatchEvent(ev);
  assertEq(chg.kind, 'files');
  assertEq(chg.upload_action_id, 'do_upload');
  assertEq(chg.value, [{ name: 'note.txt', size: 42, type: 'text/plain', last_modified: 0 }]);
});

test('FileInput drag-drop > max_files → reject', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 10_000], [3, 1],
    [4, false], [5, true], [7, 'up'],
    [8, { kind: 'literal', value: 'P' }],
  ]));
  let rej = null;
  el.addEventListener('reject', (e) => { rej = e.detail; });
  const dz = el.querySelector('.tf-file-input__dropzone');
  const ev = new (globalThis.Event)('drop', { bubbles: false, cancelable: true });
  ev.dataTransfer = { files: [fakeFile({ name: '1.txt' }), fakeFile({ name: '2.txt' })] };
  ev.preventDefault = () => {};
  dz.dispatchEvent(ev);
  assertEq(rej != null && rej.reason, 'max_files');
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

test('FileInput bez label wymaga a11y.label', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(FILE_INPUT_TAG, [
    [0, PATH('f')], [1, []], [2, 1024], [3, 1],
    [4, false], [5, false], [7, 'up'],
  ])));
});

// ============================================================================
// ColorPicker
// ============================================================================

test('ColorPicker variant=wheel renderuje <input type=color>', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  assertEq(el.querySelector('input').getAttribute('type'), 'color');
});

test('ColorPicker wheel sync z #rrggbb wartością ze store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('c'), value: '#ff8800' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  assertEq(el.querySelector('input').value, '#ff8800');
});

test('ColorPicker wheel change emituje hex value', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  const input = el.querySelector('input');
  input.value = '#00ff00';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, { value: '#00ff00', kind: 'hex' });
});

test('ColorPicker variant=swatch + default palette renderuje 16 swatches', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  assertEq(el.querySelectorAll('.tf-color-picker__swatch').length, 16);
});

test('ColorPicker variant=swatch z allowed_tokens używa tokens (NIE palette)', () => {
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

test('ColorPicker variant=tokens_only bez allowed_tokens throws', () => {
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

test('ColorPicker swatch click emituje change.kind=token gdy token', () => {
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

test('ColorPicker swatch click bez tokenów emituje change.kind=hex', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'swatch'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  const firstSwatch = el.querySelector('.tf-color-picker__swatch');
  firstSwatch.click();
  assertEq(got.kind, 'hex');
  assert(got.value.startsWith('#'));
});

test('ColorPicker compact dodaje hex input', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  assert(el.querySelector('.tf-color-picker__hex') != null);
});

test('ColorPicker compact hex input invalid value → revert', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [{ path: PATH('c'), value: '#abcdef' }], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  const hexI = el.querySelector('.tf-color-picker__hex');
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  hexI.value = 'not_a_hex';
  hexI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
  assertEq(got, null);
  assertEq(hexI.value, '#abcdef');
});

test('ColorPicker compact show_alpha=false odrzuca 8-hex z alfą', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  const hexI = el.querySelector('.tf-color-picker__hex');
  let got = null;
  el.addEventListener('tf-bind-write', (e) => { got = e.detail; });
  hexI.value = '#aabbccdd';
  hexI.dispatchEvent(new (globalThis.Event)('change', { bubbles: false }));
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

test('ColorPicker NIE emituje publicznego "change" — bind-write przez tf-bind-write', () => {
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

test('ColorPicker wheel input.change NIE bubble jako public change na wrapper', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'wheel'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let publicChange = null;
  el.addEventListener('change', (e) => { publicChange = e.detail; });
  const input = el.querySelector('input');
  input.value = '#aabbcc';
  input.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(publicChange, null);
});

test('ColorPicker compact hex.change NIE bubble jako public change', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(COLOR_PICKER_TAG, [
    [0, PATH('c')], [1, 'compact'], [3, false],
    [4, { kind: 'literal', value: 'C' }],
  ]));
  let publicChange = null;
  el.addEventListener('change', (e) => { publicChange = e.detail; });
  const hexI = el.querySelector('.tf-color-picker__hex');
  hexI.value = '#ddccbb';
  hexI.dispatchEvent(new (globalThis.Event)('change', { bubbles: true }));
  assertEq(publicChange, null);
});

test('ColorPicker bez label wymaga a11y.label', () => {
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
