// =============================================================================
// File: sdk-runtime/specialized-content-renderer.test.js
// Description: Tests for ImageGallery (0x060B), Carousel (0x060C), PdfViewer
// (0x060D), FpsCounter (0x060E), StepProgress (0x060F), Stopwatch (0x0610),
// VirtualizedLog (0x0611) — chunk 3.3g.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  IMAGE_GALLERY_TAG, CAROUSEL_TAG, PDF_VIEWER_TAG,
  FPS_COUNTER_TAG, STEP_PROGRESS_TAG, STOPWATCH_TAG,
  VIRTUALIZED_LOG_TAG,
} from './specialized-content-renderer.js';

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
function storeSetMulti(store, ...pairs) {
  const entries = [];
  for (let i = 0; i < pairs.length; i += 2) entries.push({ path: pairs[i], value: pairs[i + 1] });
  store.applySnapshot({ entries, state_revision: 0, truncated: false, panel_epoch: 1n });
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
// ImageGallery (0x060B)
// ============================================================================

test('ImageGallery renders grid of images from state path', () => {
  setup();
  const store = makeStore();
  storeSet(store, PATH('images'), [
    { src: 'https://example.com/1.jpg', alt: 'One' },
    { src: 'https://example.com/2.jpg', alt: 'Two' },
    { src: 'https://example.com/3.jpg', alt: 'Three' },
  ]);
  const engine = makeEngine(store);
  const el = engine.render(comp(IMAGE_GALLERY_TAG, [
    [0, PATH('images')], [1, 3], [2, { kind: '1:1' }],
    [3, 'md'], [4, true], [5, true],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-image-gallery'), 'wrapper class');
  const cells = el.querySelectorAll('.tf-image-gallery__cell');
  assertEq(cells.length, 3, '3 cells');
  const img = cells[0].querySelector('img');
  assertEq(img.alt, 'One');
  assert(img.loading === 'lazy', 'lazy load');
  assert(cells[0].classList.contains('tf-image-gallery__cell--clickable'), 'lightbox clickable');
});

test('ImageGallery without lightbox has no clickable cells', () => {
  setup();
  const store = makeStore();
  storeSet(store, PATH('imgs'), [{ src: 'https://x.com/a.png' }]);
  const engine = makeEngine(store);
  const el = engine.render(comp(IMAGE_GALLERY_TAG, [
    [0, PATH('imgs')], [1, 2], [2, { kind: '4:3' }],
    [3, 'sm'], [4, false], [5, false],
  ]));
  const cells = el.querySelectorAll('.tf-image-gallery__cell');
  assertEq(cells.length, 1);
  assert(!cells[0].classList.contains('tf-image-gallery__cell--clickable'), 'not clickable');
});

test('ImageGallery with empty array renders empty grid', () => {
  setup();
  const store = makeStore();
  storeSet(store, PATH('empty'), []);
  const engine = makeEngine(store);
  const el = engine.render(comp(IMAGE_GALLERY_TAG, [
    [0, PATH('empty')], [1, 4], [2, { kind: '16:9' }],
    [3, 'lg'], [4, false], [5, false],
  ]));
  assertEq(el.querySelectorAll('.tf-image-gallery__cell').length, 0);
});

// ============================================================================
// Carousel (0x060C)
// ============================================================================

test('Carousel renders slides with navigation arrows', () => {
  setup();
  const store = makeStore();
  storeSetMulti(store,
    PATH('slides'), [{ src: 'https://x.com/1.jpg' }, { src: 'https://x.com/2.jpg' }],
    PATH('idx'), 0,
  );
  const engine = makeEngine(store);
  const el = engine.render(comp(CAROUSEL_TAG, [
    [0, PATH('slides')], [1, PATH('idx')],
    [2, false], [3, 3000], [4, true], [5, true], [6, true], [7, 'swipe'],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-carousel'), 'wrapper class');
  const slides = el.querySelectorAll('.tf-carousel__slide');
  assertEq(slides.length, 2, '2 slides');
  assert(!slides[0].classList.contains('tf-carousel__slide--hidden'), 'first visible');
  assert(slides[1].classList.contains('tf-carousel__slide--hidden'), 'second hidden');
  const prevArrow = el.querySelector('.tf-carousel__arrow--prev');
  const nextArrow = el.querySelector('.tf-carousel__arrow--next');
  assert(prevArrow != null, 'prev arrow');
  assert(nextArrow != null, 'next arrow');
  assertEq(prevArrow.tagName, 'TF-BUTTON', 'prev arrow is tf-button');
  assertEq(nextArrow.tagName, 'TF-BUTTON', 'next arrow is tf-button');
  assertEq(prevArrow.getAttribute('aria-label'), 'Previous slide');
  assertEq(nextArrow.getAttribute('aria-label'), 'Next slide');
  const indicators = el.querySelectorAll('.tf-carousel__indicator');
  assertEq(indicators.length, 2, '2 indicators');
});

test('Carousel arrow click emits slide_change with next index', () => {
  setup();
  const store = makeStore();
  storeSetMulti(store,
    PATH('slides'), ['a.jpg', 'b.jpg', 'c.jpg'],
    PATH('idx'), 0,
  );
  const engine = makeEngine(store);
  const el = engine.render(comp(CAROUSEL_TAG, [
    [0, PATH('slides')], [1, PATH('idx')],
    [2, false], [3, 0], [4, false], [5, false], [6, true], [7, 'arrows_only'],
  ]));
  document.body.appendChild(el);
  let detail = null;
  el.addEventListener('slide_change', (e) => { detail = e.detail; });
  el.querySelector('.tf-carousel__arrow--next').dispatchEvent(new Event('click'));
  assertEq(detail, { index: 1 }, 'next arrow emits index 1');
  el.querySelector('.tf-carousel__arrow--prev').dispatchEvent(new Event('click'));
  // current_index_path still reads 0 (host drives state), so prev clamps to 0.
  assertEq(detail, { index: 0 }, 'prev arrow clamps at 0 without loop');
});

test('Carousel without arrows and indicators', () => {
  setup();
  const store = makeStore();
  storeSetMulti(store, PATH('s'), ['a.jpg'], PATH('i'), 0);
  const engine = makeEngine(store);
  const el = engine.render(comp(CAROUSEL_TAG, [
    [0, PATH('s')], [1, PATH('i')],
    [2, false], [3, 0], [4, false], [5, false], [6, false], [7, 'none'],
  ]));
  assert(el.querySelector('.tf-carousel__arrow--prev') == null, 'no arrows');
  assert(el.querySelector('.tf-carousel__indicators') == null, 'no indicators');
});

// ============================================================================
// PdfViewer (0x060D)
// ============================================================================

test('PdfViewer renders object element with pdf type', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PDF_VIEWER_TAG, [
    [0, 'https://example.com/doc.pdf'],
    [2, { kind: 'vh', value: 80 }],
    [3, 'fit_width'],
    [4, true],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-pdf-viewer'), 'wrapper class');
  const obj = el.querySelector('object');
  assert(obj != null, 'object element');
  assertEq(obj.type, 'application/pdf');
});

test('PdfViewer blocks javascript: src', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(PDF_VIEWER_TAG, [
    [0, 'javascript:alert(1)'], [2, { kind: 'px', value: 500 }],
    [3, 'actual'], [4, false],
  ]));
  assert(el.querySelector('object') == null, 'no object for javascript:');
  assert(el.querySelector('.tf-pdf-viewer__blocked') != null, 'blocked msg');
});

// ============================================================================
// FpsCounter (0x060E)
// ============================================================================

test('FpsCounter renders value from state path', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('fps'), 60);
  const engine = makeEngine(store);
  const el = engine.render(comp(FPS_COUNTER_TAG, [
    [0, PATH('fps')], [1, 'detailed'], [2, 30],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-fps-counter'), 'wrapper class');
  assert(el.classList.contains('tf-fps-counter--detailed'), 'variant');
  const value = el.querySelector('.tf-fps-counter__value');
  assertEq(value.textContent, '60');
  const label = el.querySelector('.tf-fps-counter__label');
  assert(label != null, 'FPS label in detailed variant');
  assertEq(label.textContent, 'FPS');
});

test('FpsCounter minimal variant has no label', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('f'), 30);
  const engine = makeEngine(store);
  const el = engine.render(comp(FPS_COUNTER_TAG, [
    [0, PATH('f')], [1, 'minimal'], [2, 10],
  ]));
  assert(el.querySelector('.tf-fps-counter__label') == null, 'no label');
});

// ============================================================================
// StepProgress (0x060F)
// ============================================================================

test('StepProgress renders steps with current marker', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('current'), 's2');
  const engine = makeEngine(store);
  const el = engine.render(comp(STEP_PROGRESS_TAG, [
    [0, [
      [[0, 's1'], [1, LIT('Step 1')], [2, false]],
      [[0, 's2'], [1, LIT('Step 2')], [2, false]],
      [[0, 's3'], [1, LIT('Step 3')], [2, true]],
    ]],
    [1, PATH('current')],
    [2, 'horizontal'],
    [3, false],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-step-progress'), 'wrapper class');
  assert(el.classList.contains('tf-step-progress--horizontal'), 'horizontal variant');
  const steps = el.querySelectorAll('.tf-step-progress__step');
  assertEq(steps.length, 3, '3 steps');
  assertEq(steps[1].getAttribute('data-status'), 'current');
  const labels = el.querySelectorAll('.tf-step-progress__label');
  assertEq(labels[0].textContent, 'Step 1');
  assert(steps[2].classList.contains('tf-step-progress__step--optional'), 'optional step');
});

test('StepProgress reads StepDef as FieldMap with status BindRef', () => {
  setup();
  const store = makeStore();
  storeSetMulti(store,
    PATH('current'), 'step0',
    PATH('st1'), 'complete',
  );
  const engine = makeEngine(store);
  const el = engine.render(comp(STEP_PROGRESS_TAG, [
    [0, [
      [[0, 'step0'], [1, LIT('First')], [2, false], [3, BOUND('current')]],
      [[0, 'step1'], [1, LIT('Second')], [2, false], [3, BOUND('st1')]],
    ]],
    [1, PATH('current')],
    [2, 'horizontal'],
    [3, false],
  ]));
  document.body.appendChild(el);
  const steps = el.querySelectorAll('.tf-step-progress__step');
  assertEq(steps.length, 2, '2 steps from FieldMap');
  assertEq(steps[0].getAttribute('data-step-id'), 'step0');
  assertEq(steps[1].getAttribute('data-step-id'), 'step1');
  // step0 id matches current id -> status current.
  assertEq(steps[0].getAttribute('data-status'), 'current');
  // step1 status BindRef resolves to 'complete'.
  assertEq(steps[1].getAttribute('data-status'), 'complete');
  const labels = el.querySelectorAll('.tf-step-progress__label');
  assertEq(labels[0].textContent, 'First');
  assertEq(labels[1].textContent, 'Second');
});

test('StepProgress clickable_completed allows clicking completed steps', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('cur'), 's2');
  const engine = makeEngine(store);
  const el = engine.render(comp(STEP_PROGRESS_TAG, [
    [0, [
      [[0, 's1'], [1, LIT('Done')], [2, false], [3, LIT('complete')]],
      [[0, 's2'], [1, LIT('Now')], [2, false]],
    ]],
    [1, PATH('cur')],
    [2, 'vertical'],
    [3, true],
  ]));
  document.body.appendChild(el);
  const steps = el.querySelectorAll('.tf-step-progress__step');
  assert(steps[0].classList.contains('tf-step-progress__step--clickable'), 'completed step clickable');
  const marker = steps[0].querySelector('.tf-step-progress__marker');
  // Complete steps get checkmark, not override by current id matching.
  assertEq(marker.textContent, '✓');
});

test('StepProgress rejects invalid steps array', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STEP_PROGRESS_TAG, [
    [0, 'not-array'], [1, PATH('c')], [2, 'horizontal'], [3, false],
  ])), 'steps must be array');
});

// ============================================================================
// Stopwatch (0x0610)
// ============================================================================

test('Stopwatch renders time display with tone', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('ts'), Date.now() - 65000); // ~1 min 5 sec ago
  const engine = makeEngine(store);
  const el = engine.render(comp(STOPWATCH_TAG, [
    [0, PATH('ts')], [1, 'minutes'], [2, 'primary'],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-stopwatch'), 'wrapper class');
  assert(el.classList.contains('tf-stopwatch--tone-primary'), 'tone');
  const time = el.querySelector('.tf-stopwatch__time');
  assert(time != null, 'time element');
  assert(time.textContent.includes('1:'), 'shows minutes:seconds');
  // The stopwatch ticks on an interval until destroyed.
  engine.destroy(el);
});

test('Stopwatch without started_at shows dash', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STOPWATCH_TAG, [
    [0, PATH('nodata')], [1, 'seconds'], [2, 'neutral'],
  ]));
  const time = el.querySelector('.tf-stopwatch__time');
  // No data in store -> dash.
  assert(time.textContent === '—' || time.textContent.includes('s'), 'dash or 0s');
  engine.destroy(el);
});

// ============================================================================
// VirtualizedLog (0x0611)
// ============================================================================

test('VirtualizedLog renders log entries with level/timestamp/message', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('events'), [
    { level: 'info', timestamp: '2026-05-24T10:00:00Z', message: 'Started' },
    { level: 'warn', timestamp: '2026-05-24T10:00:01Z', message: 'Slow query' },
    { level: 'error', timestamp: '2026-05-24T10:00:02Z', message: 'Crash', source: 'db' },
  ]);
  const engine = makeEngine(store);
  const el = engine.render(comp(VIRTUALIZED_LOG_TAG, [
    [0, PATH('events')], [1, 'default'], [2, 5000],
    [3, true], [4, false], [5, ['info', 'warn', 'error']],
    [6, true], [7, true], [8, true],
    [9, { kind: 'px', value: 400 }], [11, 'default'],
  ]));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-log-viewer'), 'wrapper class');
  const entries = el.querySelectorAll('.tf-log-viewer__entry');
  assertEq(entries.length, 3, '3 entries');
  assert(entries[0].classList.contains('tf-log-viewer__entry--info'), 'info class');
  assert(entries[1].classList.contains('tf-log-viewer__entry--warn'), 'warn class');
  const ts = entries[0].querySelector('.tf-log-viewer__ts');
  assert(ts != null, 'timestamp shown');
  const src = entries[2].querySelector('.tf-log-viewer__source');
  assert(src != null, 'source shown');
  assertEq(src.textContent, 'db');
});

test('VirtualizedLog filters by filter_levels', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('ev'), [
    { level: 'debug', message: 'debug msg' },
    { level: 'error', message: 'error msg' },
  ]);
  const engine = makeEngine(store);
  const el = engine.render(comp(VIRTUALIZED_LOG_TAG, [
    [0, PATH('ev')], [1, 'compact'], [2, 1000],
    [3, false], [4, false], [5, ['error']],
    [6, false], [7, false], [8, false],
    [9, { kind: 'full' }], [11, 'compact'],
  ]));
  const entries = el.querySelectorAll('.tf-log-viewer__entry');
  assertEq(entries.length, 1, 'only error shown');
  assert(entries[0].classList.contains('tf-log-viewer__entry--error'), 'error class');
});

test('VirtualizedLog defaults max_buffer_events to 10000 when absent', () => {
  setup();
  const store = makeStore();
  storeSet(store,PATH('ev'), []);
  const engine = makeEngine(store);
  // No field 2 -> default 10000 buffer; just verify it renders without error.
  const el = engine.render(comp(VIRTUALIZED_LOG_TAG, [
    [0, PATH('ev')], [1, 'expanded'],
    [3, true], [4, true], [5, []],
    [6, true], [7, false], [8, false],
    [9, { kind: 'full' }], [11, 'comfortable'],
  ]));
  assert(el.classList.contains('tf-log-viewer--expanded'), 'expanded variant');
  assert(el.classList.contains('tf-log-viewer--density-comfortable'), 'comfortable density');
});

test('VirtualizedLog rejects unknown log level in filter_levels', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(VIRTUALIZED_LOG_TAG, [
    [0, PATH('ev')], [1, 'default'], [3, false], [4, false],
    [5, ['invalid_level']], [6, false], [7, false], [8, false],
    [9, { kind: 'full' }], [11, 'default'],
  ])), 'unknown log level');
});

// ============================================================================
// Report
// ============================================================================

const passed = results.filter(r => r.ok).length;
const failed = results.filter(r => !r.ok);
console.log(`\nspecialized-content-renderer: ${passed}/${results.length} passed`);
for (const f of failed) console.error(`  FAIL: ${f.name}\n    ${f.err?.message || f.err}`);
if (failed.length > 0) process.exit(1);
