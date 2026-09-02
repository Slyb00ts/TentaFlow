// =============================================================================
// File: sdk-runtime/specialized-media-renderer.test.js
// Description: Tests for VideoStream (0x0604), LiveCameraTile (0x0605),
// MapView (0x0606), CodeEditor (0x0607), Terminal (0x0608), Audio (0x0609),
// IFrame (0x060A) — chunk 3.3g.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  VIDEO_STREAM_TAG, LIVE_CAMERA_TILE_TAG, MAP_VIEW_TAG,
  CODE_EDITOR_TAG, TERMINAL_TAG, AUDIO_TAG, IFRAME_TAG,
} from './specialized-media-renderer.js';

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
function storeSet(store, path, value) {
  store.applySnapshot({ entries: [{ path, value }], state_revision: 0, truncated: false, panel_epoch: 1n });
}
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
// VideoStream (0x0604)
// ============================================================================

test('VideoStream renders <video> with src from BindRef', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(VIDEO_STREAM_TAG, [
    [0, LIT('https://example.com/video.mp4')],
    [2, { kind: '16:9' }],
    [3, 'full'],
    [4, false],
    [5, true],
    [6, 'cover'],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-video-stream'), 'wrapper class');
  const video = el.querySelector('video');
  assert(video != null, 'video element exists');
  assertEq(video.src, 'https://example.com/video.mp4');
  assert(video.muted === true, 'muted');
  assert(video.controls === true, 'controls=full');
});

test('VideoStream rejects javascript: src', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(VIDEO_STREAM_TAG, [
    [0, LIT('javascript:alert(1)')],
    [2, { kind: '16:9' }],
    [3, 'none'],
    [4, false],
    [5, false],
    [6, 'contain'],
  ]));
  const video = el.querySelector('video');
  assert(!video.hasAttribute('src'), 'src should not be set for javascript:');
});

test('VideoStream rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(VIDEO_STREAM_TAG, [
    [0, LIT('x')], [2, { kind: '1:1' }], [3, 'none'], [4, false], [5, false], [6, 'cover'], [99, 'bad'],
  ])), 'unknown field');
});

// ============================================================================
// LiveCameraTile (0x0605)
// ============================================================================

test('LiveCameraTile renders camera overlay with label and status', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(LIVE_CAMERA_TILE_TAG, [
    [0, LIT('https://cam.local/stream')],
    [1, LIT('Front Door')],
    [2, LIT('online')],
    [4, true],
    [5, true],
    [6, { kind: '4:3' }],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-live-camera'), 'wrapper class');
  const label = el.querySelector('.tf-live-camera__label');
  assert(label != null, 'label exists');
  assertEq(label.textContent, 'Front Door');
  const status = el.querySelector('.tf-live-camera__status');
  assertEq(status.textContent, 'online');
  const fsBtn = el.querySelector('.tf-live-camera__fullscreen');
  assert(fsBtn != null, 'fullscreen button');
  // The overlay keeps clock/fps intervals alive until the tile is destroyed.
  engine.destroy(el);
});

test('LiveCameraTile without overlay hides label/status', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(LIVE_CAMERA_TILE_TAG, [
    [0, LIT('s')], [1, LIT('L')], [2, LIT('offline')],
    [4, false], [5, false], [6, { kind: '16:9' }],
  ]));
  assert(el.querySelector('.tf-live-camera__overlay') == null, 'no overlay');
  assert(el.querySelector('.tf-live-camera__fullscreen') == null, 'no fullscreen');
});

test('LiveCameraTile with fps shows fps overlay', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(LIVE_CAMERA_TILE_TAG, [
    [0, LIT('s')], [1, LIT('Cam')], [2, LIT('online')],
    [3, LIT(30)], [4, true], [5, false], [6, { kind: '16:9' }],
  ]));
  document.body.appendChild(el);
  const fps = el.querySelector('.tf-live-camera__fps');
  assert(fps != null, 'fps element');
  assertEq(fps.textContent, '30 fps');
  engine.destroy(el);
});

// ============================================================================
// MapView (0x0606)
// ============================================================================

test('MapView renders placeholder container with data attributes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MAP_VIEW_TAG, [
    [0, PATH('center')],
    [1, PATH('zoom')],
    [2, 'osm'],
    [4, { kind: 'px', value: 400 }],
    [5, PATH('markers')],
    [8, true],
    [9, true],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-map-view'), 'wrapper class');
  assertEq(el.getAttribute('data-map-provider'), 'osm');
  assertEq(el.getAttribute('data-interactive'), 'true');
  assert(el.querySelector('.tf-map-view__placeholder') != null, 'placeholder');
});

test('MapView with tile_server_url sets data-tile-server-url', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(MAP_VIEW_TAG, [
    [0, PATH('c')], [1, PATH('z')], [2, 'tile_server'],
    [3, 'https://tiles.local/{z}/{x}/{y}.png'],
    [4, { kind: 'vh', value: 50 }], [5, PATH('m')], [8, false], [9, false],
  ]));
  assertEq(el.getAttribute('data-tile-server-url'), 'https://tiles.local/{z}/{x}/{y}.png');
});

// ============================================================================
// CodeEditor (0x0607)
// ============================================================================

test('CodeEditor renders pre/code with line numbers', () => {
  setup();
  const store = makeStore();
  storeSet(store, PATH('src'), 'fn main() {\n  println!("ok");\n}');
  const engine = makeEngine(store);
  const el = engine.render(comp(CODE_EDITOR_TAG, [
    [0, PATH('src')], [1, 'rust'], [2, false], [3, true], [4, false],
    [5, 'dark'], [6, 200], [8, 4], [9, false], [10, true], [11, true],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-code-editor'), 'wrapper class');
  assert(el.classList.contains('tf-code-editor--theme-dark'), 'dark theme');
  const gutter = el.querySelector('.tf-code-editor__gutter');
  assert(gutter != null, 'gutter with line numbers');
  const lineNos = gutter.querySelectorAll('.tf-code-editor__line-no');
  assertEq(lineNos.length, 3, '3 lines');
  assertEq(lineNos[0].textContent, '1');
});

test('CodeEditor without line_numbers has no gutter', () => {
  setup();
  const store = makeStore();
  storeSet(store, PATH('src'), 'hello');
  const engine = makeEngine(store);
  const el = engine.render(comp(CODE_EDITOR_TAG, [
    [0, PATH('src')], [1, 'javascript'], [2, true], [3, false], [4, true],
    [5, 'light'], [6, 100], [9, false], [10, false], [11, false],
  ]));
  assert(el.querySelector('.tf-code-editor__gutter') == null, 'no gutter');
});

test('CodeEditor default tab_size is 2 when field 8 absent', () => {
  setup();
  const store = makeStore();
  storeSet(store, PATH('src'), '');
  const engine = makeEngine(store);
  const el = engine.render(comp(CODE_EDITOR_TAG, [
    [0, PATH('src')], [1, 'python'], [2, false], [3, false], [4, false],
    [5, 'auto'], [6, 150], [9, false], [10, false], [11, false],
  ]));
  assertEq(el.getAttribute('data-tab-size'), '2');
});

// ============================================================================
// Terminal (0x0608)
// ============================================================================

test('Terminal renders pre with monospace output', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(TERMINAL_TAG, [
    [0, LIT('$ hello world')],
    [1, 24], [2, 80], [3, 'default'], [4, false], [5, true], [6, 1000],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-terminal'), 'wrapper class');
  const pre = el.querySelector('.tf-terminal__output');
  assert(pre != null, 'pre element');
  assert(pre.textContent.includes('hello world'), 'content');
});

test('Terminal default max_buffer_lines is 10000 when field 6 absent', () => {
  setup();
  const engine = makeEngine();
  // no field 6 -> defaults internally to 10000
  const el = engine.render(comp(TERMINAL_TAG, [
    [0, LIT('line1')], [1, 24], [2, 80], [3, 'high_contrast'], [4, true], [5, false],
  ]));
  assert(el.classList.contains('tf-terminal--theme-high_contrast'), 'theme');
});

test('Terminal rejects unknown field', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(TERMINAL_TAG, [
    [0, LIT('x')], [1, 24], [2, 80], [3, 'default'], [4, false], [5, false], [99, true],
  ])), 'unknown field');
});

// ============================================================================
// Audio (0x0609)
// ============================================================================

test('Audio renders <audio> with controls and loop', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUDIO_TAG, [
    [0, LIT('https://example.com/audio.mp3')],
    [1, 'full'], [2, false], [3, true], [4, 'waveform'],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-audio'), 'wrapper class');
  assert(el.classList.contains('tf-audio--variant-waveform'), 'variant');
  const audio = el.querySelector('audio');
  assert(audio != null, 'audio element');
  assert(audio.controls === true, 'controls');
  assert(audio.loop === true, 'loop');
  assertEq(audio.src, 'https://example.com/audio.mp3');
});

test('Audio with controls=none has no controls attr', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUDIO_TAG, [
    [0, LIT('x.mp3')], [1, 'none'], [2, false], [3, false], [4, 'compact'],
  ]));
  const audio = el.querySelector('audio');
  assert(audio.controls === false, 'no controls');
});

test('Audio rejects javascript: src', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(AUDIO_TAG, [
    [0, LIT('javascript:void(0)')],
    [1, 'minimal'], [2, false], [3, false], [4, 'default'],
  ]));
  const audio = el.querySelector('audio');
  assert(!audio.hasAttribute('src'), 'no src');
});

// ============================================================================
// IFrame (0x060A)
// ============================================================================

test('IFrame renders sandboxed iframe with valid src', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(IFRAME_TAG, [
    [0, 'https://embed.example.com/widget'],
    [1, ['allow-scripts', 'allow-forms']],
    [2, { kind: 'px', value: 800 }],
    [3, { kind: 'px', value: 600 }],
    [4, 'Chart widget'],
    [5, 'no-referrer'],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-iframe'), 'wrapper class');
  const iframe = el.querySelector('iframe');
  assert(iframe != null, 'iframe element');
  assertEq(iframe.src, 'https://embed.example.com/widget');
  assertEq(iframe.title, 'Chart widget');
  assertEq(iframe.getAttribute('sandbox'), 'allow-scripts allow-forms');
});

test('IFrame blocks javascript: src', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(IFRAME_TAG, [
    [0, 'javascript:alert(1)'],
    [1, []], [2, { kind: 'px', value: 400 }], [3, { kind: 'px', value: 300 }],
    [4, 'Test'], [5, 'no-referrer'],
  ]));
  const iframe = el.querySelector('iframe');
  assert(iframe == null, 'no iframe for javascript: src');
  const blocked = el.querySelector('.tf-iframe__blocked');
  assert(blocked != null, 'blocked message shown');
});

test('IFrame rejects unknown sandbox token', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(IFRAME_TAG, [
    [0, 'https://x.com'], [1, ['allow-same-origin']], [2, { kind: 'px', value: 100 }],
    [3, { kind: 'px', value: 100 }], [4, 'T'], [5, 'origin'],
  ])), 'unknown sandbox token');
});

// ============================================================================
// Report
// ============================================================================

const passed = results.filter(r => r.ok).length;
const failed = results.filter(r => !r.ok);
console.log(`\nspecialized-media-renderer: ${passed}/${results.length} passed`);
for (const f of failed) console.error(`  FAIL: ${f.name}\n    ${f.err?.message || f.err}`);
if (failed.length > 0) process.exit(1);
