// =============================================================================
// Plik: sdk-runtime/data-bar-chart-renderer.test.js
// Opis: Testy BarChart (0x0217) + StackedBar (0x021A) — chunk 3.3d-10.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { BAR_CHART_TAG, STACKED_BAR_TAG } from './data-bar-chart-renderer.js';
import '../components/tf-bar-chart.js';

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

function chartSeries(id, dataPath, opts = {}) {
  return [
    [0, id], [1, { kind: 'literal', value: opts.name || id }],
    [2, dataPath], [4, opts.style || 'solid'], [5, opts.showInLegend !== false],
    ...(opts.tone ? [[3, opts.tone]] : []),
  ];
}
function chartAxis({ scale = 'category' } = {}) { return [[5, scale]]; }
function chartLegend({ position = 'bottom', alignment = 'center' } = {}) { return [[0, position], [1, alignment]]; }
function barFields({
  series = [], xAxis = chartAxis(), yAxis = [[5, 'linear']],
  orientation = 'vertical', stacking = 'none',
  legend = chartLegend(), heightPx = 200,
} = {}) {
  return [
    [0, series], [1, xAxis], [2, yAxis],
    [3, orientation], [4, stacking], [5, legend], [6, heightPx],
  ];
}

function stackSeg(id, valuePath, tone, opts = {}) {
  const f = [[0, id], [1, { kind: 'bound', path: valuePath }], [3, tone]];
  if (opts.label) f.push([2, opts.label]);
  return f;
}
function stackedBarFields({
  segments = [], total = { kind: 'literal', value: 100 },
  showLegend = false, showPercentages = false, heightPx = 24,
} = {}) {
  return [
    [0, segments], [1, total], [2, showLegend], [3, showPercentages], [4, heightPx],
  ];
}

// ============================================================================
// BarChart
// ============================================================================

test('BarChart vertical stacking=none renderuje grouped bar per series per category', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 'Q1', y: 10 }, { x: 'Q2', y: 20 }] },
      { path: PATH('b'), value: [{ x: 'Q1', y: 5 }, { x: 'Q2', y: 15 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('a', PATH('a'), { tone: 'success' }), chartSeries('b', PATH('b'), { tone: 'critical' })],
  })));
  document.body.appendChild(el);
  // 2 categories × 2 series = 4 bars.
  assertEq(el.querySelectorAll('.tf-chart__bar').length, 4);
  assert(el.querySelector('.tf-chart__bar--tone-success') != null);
  assert(el.querySelector('.tf-chart__bar--tone-critical') != null);
});

test('BarChart vertical stacking=stacked renderuje per category stack', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 'Q1', y: 30 }, { x: 'Q2', y: 50 }] },
      { path: PATH('b'), value: [{ x: 'Q1', y: 20 }, { x: 'Q2', y: 10 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('a', PATH('a')), chartSeries('b', PATH('b'))],
    stacking: 'stacked',
  })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-chart--stacking-stacked'));
  assertEq(el.querySelectorAll('.tf-chart__bar').length, 4);
});

test('BarChart vertical stacking=percent normalizuje do 100%', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 'Q1', y: 25 }] },
      { path: PATH('b'), value: [{ x: 'Q1', y: 75 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('a', PATH('a')), chartSeries('b', PATH('b'))],
    stacking: 'percent',
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-chart__bar').length, 2);
});

test('BarChart horizontal renderuje category labels po lewej', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 'A', y: 10 }, { x: 'B', y: 20 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    orientation: 'horizontal',
  })));
  document.body.appendChild(el);
  assert(el.classList.contains('tf-chart--orientation-horizontal'));
  const yLabels = el.querySelectorAll('.tf-chart__axis--y .tf-chart__axis-label');
  const texts = Array.from(yLabels).map((l) => l.textContent);
  assert(texts.includes('A') && texts.includes('B'));
});

test('BarChart stacking=stacked + log y_axis throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'stacked',
    yAxis: [[5, 'log']],
  }))));
});

test('BarChart stacking=percent + category y_axis throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'percent',
    yAxis: [[5, 'category']],
  }))));
});

test('BarChart horizontal stacking=none + log scale skaluje bar.width logarytmicznie', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    // value=31.62 to ~połowa drogi log między 10..100.
    entries: [{ path: PATH('s'), value: [{ x: 'A', y: 31.62 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    orientation: 'horizontal',
    yAxis: [[5, 'log'], [2, 10], [3, 100]],
  })));
  document.body.appendChild(el);
  const rect = el.querySelector('rect.tf-chart__bar');
  // Container szerokość 200*1.5 fallback = 300; x1-x0 = 300-48-16 = 236.
  // log(31.62)~1.5, log10=1, log100=2, ratio=0.5 → bar width ~118.
  const w = parseFloat(rect.getAttribute('width'));
  assert(w > 100 && w < 140, `expected log-scaled width ~118, got ${w}`);
});

test('BarChart legend toggle ukrywa bars + emit series_toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 'A', y: 5 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('series_toggle', (e) => { got = e.detail; });
  el.querySelector('.tf-chart__legend-item').click();
  assertEq(got.hidden, true);
  assertEq(el.querySelectorAll('.tf-chart__bar').length, 0);
});

test('BarChart reactive: update series rebuilds bars', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 'A', y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('.tf-chart__bar').length, 1);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('s'), op: { kind: 'set', value: [{ x: 'A', y: 1 }, { x: 'B', y: 2 }, { x: 'C', y: 3 }] } }],
  });
  assertEq(el.querySelectorAll('.tf-chart__bar').length, 3);
});

test('BarChart pusta series throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, barFields({ series: [] }))));
});

test('BarChart invalid orientation throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    orientation: 'diagonal',
  }))));
});

test('BarChart invalid stacking throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'crazy',
  }))));
});

test('BarChart unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, [
    ...barFields({ series: [chartSeries('s', PATH('s'))] }), [99, 'x'],
  ])));
});

test('BarChart height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
    heightPx: 0,
  }))));
});

test('BarChart SVG ma role=img + aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 'A', y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(BAR_CHART_TAG, barFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  const svg = el.querySelector('svg');
  assertEq(svg.getAttribute('role'), 'img');
  assert(svg.hasAttribute('aria-label'));
});

// ============================================================================
// StackedBar
// ============================================================================

test('StackedBar renderuje pojedynczy <div> bar z segmentami', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: 30 },
      { path: PATH('b'), value: 50 },
      { path: PATH('c'), value: 20 },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [
      stackSeg('a', PATH('a'), 'success'),
      stackSeg('b', PATH('b'), 'warning'),
      stackSeg('c', PATH('c'), 'critical'),
    ],
    total: { kind: 'literal', value: 100 },
  })));
  assertEq(el.querySelectorAll('.tf-stacked-bar__segment').length, 3);
});

test('StackedBar segment width proportional do value/total', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: 25 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('a', PATH('a'), 'primary')],
    total: { kind: 'literal', value: 100 },
  })));
  const seg = el.querySelector('.tf-stacked-bar__segment');
  assertEq(seg.style.width, '25%');
});

test('StackedBar sum > total clamps overflow', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: 60 },
      { path: PATH('b'), value: 60 },  // overflow 20
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [
      stackSeg('a', PATH('a'), 'success'),
      stackSeg('b', PATH('b'), 'critical'),
    ],
    total: { kind: 'literal', value: 100 },
  })));
  const segs = el.querySelectorAll('.tf-stacked-bar__segment');
  assertEq(segs[0].style.width, '60%');
  // Second segment clamped do remaining 40% (NIE 60%).
  assertEq(segs[1].style.width, '40%');
});

test('StackedBar show_percentages renderuje % w segmentach', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: 50 }, { path: PATH('b'), value: 50 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [
      stackSeg('a', PATH('a'), 'success'),
      stackSeg('b', PATH('b'), 'critical'),
    ],
    total: { kind: 'literal', value: 100 },
    showPercentages: true,
  })));
  const percentages = el.querySelectorAll('.tf-stacked-bar__segment-percent');
  assertEq(percentages.length, 2);
  assertEq(percentages[0].textContent, '50%');
});

test('StackedBar show_legend renderuje legend list z value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: 40 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('a', PATH('a'), 'primary', { label: { kind: 'literal', value: 'Done' } })],
    total: { kind: 'literal', value: 100 },
    showLegend: true,
  })));
  const legend = el.querySelector('.tf-stacked-bar__legend');
  assert(legend != null);
  assertEq(legend.querySelector('.tf-stacked-bar__legend-label').textContent, 'Done');
});

test('StackedBar reactive: value update rebuilds widths', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: 10 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('a', PATH('a'), 'primary')],
    total: { kind: 'literal', value: 100 },
  })));
  assertEq(el.querySelector('.tf-stacked-bar__segment').style.width, '10%');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 75 } }],
  });
  assertEq(el.querySelector('.tf-stacked-bar__segment').style.width, '75%');
});

test('StackedBar duplicate segment id throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('dup', PATH('a'), 'success'), stackSeg('dup', PATH('b'), 'critical')],
  }))));
});

test('StackedBar pusta segments throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STACKED_BAR_TAG, stackedBarFields({ segments: [] }))));
});

test('StackedBar invalid segment id grammar throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('Bad ID!', PATH('a'), 'primary')],
  }))));
});

test('StackedBar invalid tone throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('s', PATH('a'), 'rainbow')],
  }))));
});

test('StackedBar height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('s', PATH('a'), 'primary')],
    heightPx: 0,
  }))));
});

test('StackedBar unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(STACKED_BAR_TAG, [
    ...stackedBarFields({ segments: [stackSeg('s', PATH('a'), 'primary')] }),
    [99, 'x'],
  ])));
});

test('StackedBar negative segment value treated as 0', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: -50 }, { path: PATH('b'), value: 30 }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(STACKED_BAR_TAG, stackedBarFields({
    segments: [stackSeg('a', PATH('a'), 'critical'), stackSeg('b', PATH('b'), 'success')],
    total: { kind: 'literal', value: 100 },
  })));
  const segs = el.querySelectorAll('.tf-stacked-bar__segment');
  assertEq(segs[0].style.width, '0%');
  assertEq(segs[1].style.width, '30%');
});

// ---- <tf-bar-chart> component contract (analytics charts) ----

function makeBarChart(opts = {}) {
  const el = document.createElement('tf-bar-chart');
  el.height = opts.height ?? 200;
  if (opts.stacking) el.stacking = opts.stacking;
  if (opts.tooltip) el.tooltip = opts.tooltip;
  if (opts.narrow !== undefined) el.narrow = opts.narrow;
  el.series = opts.series;
  document.body.appendChild(el);
  return el;
}
function daySeries(id, tone, n, base) {
  return {
    id, name: id, tone, style: 'solid', showInLegend: true,
    points: Array.from({ length: n }, (_, i) => ({ x: String(i + 1).padStart(2, '0') + '.08', y: base * (i + 1) })),
  };
}

test('tf-bar-chart: tooltip domyślnie włączony (element .tf-chart__tooltip + crosshair)', () => {
  setup();
  const el = makeBarChart({ series: [daySeries('a', 'primary', 3, 10)] });
  assert(el.querySelector('.tf-chart__tooltip') != null, 'tooltip element expected by default');
  assert(el.querySelector('line.tf-chart__crosshair') != null, 'crosshair expected by default');
  const off = makeBarChart({ series: [daySeries('a', 'primary', 3, 10)], tooltip: { enabled: false } });
  assertEq(off.querySelector('.tf-chart__tooltip'), null);
  assertEq(off.querySelector('.tf-chart__crosshair'), null);
});

test('tf-bar-chart stacked: tylko górny segment to <path> z zaokrąglonym szczytem', () => {
  setup();
  const el = makeBarChart({
    stacking: 'stacked',
    series: [daySeries('completion', 'primary', 2, 100), daySeries('prompt', 'accent', 2, 50)],
  });
  const rects = el.querySelectorAll('rect.tf-chart__bar');
  const paths = el.querySelectorAll('path.tf-chart__bar');
  assertEq(rects.length, 2, 'lower segments are plain rects');
  assertEq(paths.length, 2, 'top segments are paths');
  for (const p of paths) {
    assert(p.classList.contains('tf-chart__bar--tone-accent'), 'top segment belongs to the last series');
    const d = p.getAttribute('d');
    // Two quadratic corners at the top, straight vertical sides down to the base.
    assertEq((d.match(/Q/g) || []).length, 2, `expected two rounded corners in ${d}`);
    assert(/^M[\d.]+,[\d.]+V/.test(d) && d.endsWith('Z'), `path starts at the base and closes: ${d}`);
  }
  // Every bar of a category grows from the axis with the same stagger delay.
  const bars = el.querySelectorAll('.tf-chart__bar--enter');
  assertEq(bars.length, 4);
  assertEq(bars[0].style.animationDelay, bars[1].style.animationDelay);
  assertEq(bars[2].style.animationDelay, '12ms');
  assert(/^0(px)? [\d.]+px$/.test(bars[0].style.transformOrigin), `origin at the axis: ${bars[0].style.transformOrigin}`);
});

test('tf-bar-chart narrow: wąski plot rysuje tylko ostatnie maxPoints kategorii', () => {
  setup();
  // happy-dom fallback box = height*1.5 = 300 px < breakpoint 560.
  const el = makeBarChart({ series: [daySeries('a', 'primary', 19, 1000)] });
  const bars = el.querySelectorAll('.tf-chart__bar');
  assertEq(bars.length, 10);
  const labels = Array.from(el.querySelectorAll('.tf-chart__axis--x .tf-chart__axis-label')).map((l) => l.textContent);
  assertEq(labels[0], '10.08');
  assertEq(labels[labels.length - 1], '19.08');
  const wide = makeBarChart({ series: [daySeries('a', 'primary', 19, 1000)], narrow: null });
  assertEq(wide.querySelectorAll('.tf-chart__bar').length, 19);
});

test('tf-bar-chart: oś Y formatuje ticki przez fmtCompact (mln/tys)', () => {
  setup();
  const el = makeBarChart({ series: [daySeries('a', 'primary', 4, 1_000_000)] });
  el.locale = 'pl';
  const labels = Array.from(el.querySelectorAll('.tf-chart__axis--y .tf-chart__axis-label')).map((l) => l.textContent);
  assert(labels.some((l) => /mln$/.test(l)), `expected "mln" ticks, got ${JSON.stringify(labels)}`);
  assert(!labels.some((l) => /\d{7}/.test(l)), 'no raw 7-digit numbers on the axis');
});

test('tf-bar-chart: tone accent → klasa --tone-accent na słupku i swatchu legendy', () => {
  setup();
  const el = makeBarChart({ series: [daySeries('prompt', 'accent', 2, 10)] });
  el.legend = { position: 'top', alignment: 'start' };
  assert(el.querySelector('.tf-chart__bar--tone-accent') != null);
  assert(el.querySelector('.tf-chart__legend-swatch--tone-accent') != null);
});

test('tf-bar-chart: maxBarWidth ogranicza szerokość słupka (domyślnie 34 px)', () => {
  setup();
  const el = makeBarChart({ stacking: 'stacked', series: [daySeries('a', 'primary', 2, 10)] });
  const d = el.querySelector('path.tf-chart__bar').getAttribute('d');
  const xs = d.match(/[MH]([\d.]+)/g).map((m) => parseFloat(m.slice(1)));
  const width = Math.max(...xs) - Math.min(...xs) + 3;  // H stops short of the 3 px corner radius
  assert(Math.abs(width - 34) < 0.01, `expected 34 px bar, got ${width}`);
  el.maxBarWidth = 20;
  const d2 = el.querySelector('path.tf-chart__bar').getAttribute('d');
  const xs2 = d2.match(/[MH]([\d.]+)/g).map((m) => parseFloat(m.slice(1)));
  assert(Math.abs(Math.max(...xs2) - Math.min(...xs2) + 3 - 20) < 0.01, `expected 20 px bar: ${d2}`);
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
