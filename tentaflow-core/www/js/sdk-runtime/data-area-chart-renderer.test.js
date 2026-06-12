// =============================================================================
// Plik: sdk-runtime/data-area-chart-renderer.test.js
// Opis: Testy AreaChart (0x0218) — chunk 3.3d-9.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { AREA_CHART_TAG } from './data-area-chart-renderer.js';
import '../components/tf-area-chart.js';

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
function chartAxis({ scale = 'linear' } = {}) { return [[5, scale]]; }
function chartLegend({ position = 'bottom', alignment = 'center' } = {}) { return [[0, position], [1, alignment]]; }
function chartTooltip({ enabled = true } = {}) { return [[0, enabled]]; }

function areaFields({
  series = [], xAxis = chartAxis(), yAxis = chartAxis(),
  legend = chartLegend(), tooltip = chartTooltip(),
  zoom = 'none', brush = false, heightPx = 200,
  stacking = 'none', opacity = 0.4,
} = {}) {
  return [
    [0, series], [1, xAxis], [2, yAxis], [3, legend],
    [4, tooltip], [5, zoom], [6, brush], [7, heightPx],
    [8, stacking], [9, opacity],
  ];
}

// ============================================================================

test('AreaChart renderuje SVG z <polygon> per series', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 3 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assert(el.querySelector('polygon.tf-chart__area') != null);
  // Plus polyline top edge.
  assert(el.querySelector('polyline.tf-chart__series-line') != null);
});

test('AreaChart opacity per spec default=0.4 ustawia fill-opacity', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  const area = el.querySelector('polygon.tf-chart__area');
  assertEq(area.getAttribute('fill-opacity'), '0.4');
});

test('AreaChart opacity > 1 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    opacity: 1.5,
  }))));
});

test('AreaChart opacity < 0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    opacity: -0.1,
  }))));
});

test('AreaChart stacking=stacked baseline na poprzedniej serii', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] },
      { path: PATH('b'), value: [{ x: 0, y: 3 }, { x: 1, y: 4 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [
      chartSeries('a', PATH('a'), { tone: 'success' }),
      chartSeries('b', PATH('b'), { tone: 'critical' }),
    ],
    stacking: 'stacked',
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('polygon.tf-chart__area').length, 2);
  assert(el.classList.contains('tf-chart--stacking-stacked'));
});

test('AreaChart stacking=percent normalizuje do 100%', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 0, y: 25 }, { x: 1, y: 50 }] },
      { path: PATH('b'), value: [{ x: 0, y: 75 }, { x: 1, y: 50 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [
      chartSeries('a', PATH('a')),
      chartSeries('b', PATH('b')),
    ],
    stacking: 'percent',
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('polygon.tf-chart__area').length, 2);
});

test('AreaChart stacking=percent + log scale throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'percent',
    yAxis: chartAxis({ scale: 'log' }),
  }))));
});

test('AreaChart stacking=stacked + log scale throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'stacked',
    yAxis: chartAxis({ scale: 'log' }),
  }))));
});

test('AreaChart stacking=percent z y_axis.scale=category throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'percent',
    yAxis: chartAxis({ scale: 'category' }),
  }))));
});

test('AreaChart invalid stacking mode throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'super',
  }))));
});

test('AreaChart reuse chart-shared: axes, legend, tooltip jak LineChart', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-chart__axis--x') != null);
  assert(el.querySelector('.tf-chart__axis--y') != null);
  assert(el.querySelector('.tf-chart__legend') != null);
  assert(el.querySelector('.tf-chart__tooltip') != null);
});

test('AreaChart series.points overlay (circles)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 3 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 3);
});

test('AreaChart legend toggle ukrywa area + emit series_toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('series_toggle', (e) => { got = e.detail; });
  el.querySelector('.tf-chart__legend-item').click();
  assertEq(got.hidden, true);
  assertEq(el.querySelectorAll('polygon.tf-chart__area').length, 0);
});

test('AreaChart reactive store update rebuilds polygon points', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 2);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('s'), op: { kind: 'set', value: [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 3 }, { x: 3, y: 4 }] } }],
  });
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 4);
});

test('AreaChart unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, [
    ...areaFields({ series: [chartSeries('s', PATH('s'))] }), [99, 'x'],
  ])));
});

test('AreaChart pusta series throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({ series: [] }))));
});

test('AreaChart stacking=none akceptuje negative Y (area pod baseline)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: -5 }, { x: 1, y: 3 }, { x: 2, y: -2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    stacking: 'none',
  })));
  document.body.appendChild(el);
  // Wszystkie 3 punkty widoczne (incl negative).
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 3);
});

test('AreaChart stacking=stacked pomija v<=0 (zero NIE kontrybuuje)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 0, y: 5 }, { x: 1, y: 0 }, { x: 2, y: -3 }] },
      { path: PATH('b'), value: [{ x: 0, y: 2 }, { x: 1, y: 4 }, { x: 2, y: 1 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('a', PATH('a')), chartSeries('b', PATH('b'))],
    stacking: 'stacked',
  })));
  document.body.appendChild(el);
  // Series 'a' ma punkty tylko dla X=0 (5 > 0); X=1 (0) i X=2 (-3) pomijane.
  // Series 'b' ma wszystkie 3 punkty (positive).
  const groups = el.querySelectorAll('.tf-chart__series-points');
  assertEq(groups[0].querySelectorAll('circle').length, 1);
  assertEq(groups[1].querySelectorAll('circle').length, 3);
});

test('AreaChart NaN/non-numeric Y są filtrowane', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: NaN }, { x: 2, y: 'foo' }, { x: 3, y: 4 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 2);
});

test('AreaChart wszystkie 7 tonów renderowane z odpowiednią klasą', () => {
  setup();
  const tones = ['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted'];
  const store = makeStore();
  const entries = tones.map((t, i) => ({ path: PATH(`s${i}`), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }));
  store.applySnapshot({ entries, state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: tones.map((t, i) => chartSeries(`s${i}`, PATH(`s${i}`), { tone: t })),
  })));
  document.body.appendChild(el);
  for (const t of tones) {
    assert(el.querySelector(`.tf-chart__area--tone-${t}`) != null, `expected area--tone-${t}`);
  }
});

test('AreaChart height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
    heightPx: 0,
  }))));
});

test('AreaChart SVG ma role=img + aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(AREA_CHART_TAG, areaFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  const svg = el.querySelector('svg');
  assertEq(svg.getAttribute('role'), 'img');
  assert(svg.hasAttribute('aria-label'));
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
