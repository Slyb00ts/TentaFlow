// =============================================================================
// Plik: sdk-runtime/data-line-chart-renderer.test.js
// Opis: Testy LineChart (0x0216) — chunk 3.3d-8.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { LINE_CHART_TAG } from './data-line-chart-renderer.js';
import '../components/tf-line-chart.js';

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
function chartAxis({ scale = 'linear', label, ticks, min, max } = {}) {
  const f = [[5, scale]];
  if (label) f.push([0, label]);
  if (ticks != null) f.push([4, ticks]);
  if (min != null) f.push([2, min]);
  if (max != null) f.push([3, max]);
  return f;
}
function chartLegend({ position = 'bottom', alignment = 'center' } = {}) {
  return [[0, position], [1, alignment]];
}
function chartTooltip({ enabled = true, format } = {}) {
  const f = [[0, enabled]];
  if (format) f.push([1, format]);
  return f;
}

function lineChartFields({
  series = [], xAxis = chartAxis(), yAxis = chartAxis(), legend = chartLegend(),
  tooltip = chartTooltip(), zoom = 'none', brush = false, heightPx = 200,
} = {}) {
  return [
    [0, series], [1, xAxis], [2, yAxis], [3, legend],
    [4, tooltip], [5, zoom], [6, brush], [7, heightPx],
  ];
}

// ============================================================================

test('LineChart renderuje <svg> z polyline per series', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s1'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 1.5 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s1', PATH('s1'))],
  })));
  document.body.appendChild(el);
  const svg = el.querySelector('svg');
  assertEq(svg.tagName.toLowerCase(), 'svg');
  assert(svg.querySelector('polyline.tf-chart__series-line') != null);
});

test('LineChart wiele serii renderuje osobne polylines z osobnymi tone', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] },
      { path: PATH('b'), value: [{ x: 0, y: 3 }, { x: 1, y: 1 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [
      chartSeries('a', PATH('a'), { tone: 'success' }),
      chartSeries('b', PATH('b'), { tone: 'critical' }),
    ],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('polyline.tf-chart__series-line').length, 2);
  assert(el.querySelector('.tf-chart__series-line--tone-success') != null);
  assert(el.querySelector('.tf-chart__series-line--tone-critical') != null);
});

test('LineChart series style=dashed → klasa --style-dashed', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'), { style: 'dashed' })],
  })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-chart__series-line--style-dashed') != null);
});

test('LineChart axes — X i Y rendered', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 0 }, { x: 10, y: 100 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assert(el.querySelector('.tf-chart__axis--x') != null);
  assert(el.querySelector('.tf-chart__axis--y') != null);
});

test('LineChart axis labels — tick text wygenerowany', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 0 }, { x: 10, y: 100 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  const labels = el.querySelectorAll('.tf-chart__axis-label');
  assert(labels.length > 0, 'expected at least one axis tick label');
});

test('LineChart legend renderowany z entries per series', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 0, y: 1 }] },
      { path: PATH('b'), value: [{ x: 0, y: 2 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [
      chartSeries('a', PATH('a'), { name: 'Apple' }),
      chartSeries('b', PATH('b'), { name: 'Banana' }),
    ],
  })));
  const items = el.querySelectorAll('.tf-chart__legend-item');
  assertEq(items.length, 2);
  assertEq(items[0].querySelector('.tf-chart__legend-label').textContent, 'Apple');
});

test('LineChart legend item click toggle series visibility + emit series_toggle', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('series_toggle', (e) => { got = e.detail; });
  const item = el.querySelector('.tf-chart__legend-item');
  item.click();
  assertEq(got, { series_id: 's', hidden: true });
  assert(item.classList.contains('tf-chart__legend-item--hidden'));
  // Series powinna zniknąć z SVG.
  assertEq(el.querySelectorAll('polyline.tf-chart__series-line').length, 0);
});

test('LineChart show_in_legend=false ukrywa entry', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: PATH('a'), value: [{ x: 0, y: 1 }] },
      { path: PATH('b'), value: [{ x: 0, y: 2 }] },
    ],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [
      chartSeries('a', PATH('a')),
      chartSeries('b', PATH('b'), { showInLegend: false }),
    ],
  })));
  assertEq(el.querySelectorAll('.tf-chart__legend-item').length, 1);
});

test('LineChart legend position=none nie renderuje legend', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    legend: chartLegend({ position: 'none' }),
  })));
  assertEq(el.querySelector('.tf-chart__legend'), null);
});

test('LineChart category scale renderuje per-category ticks', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 'A', y: 1 }, { x: 'B', y: 2 }, { x: 'C', y: 3 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    xAxis: chartAxis({ scale: 'category' }),
  })));
  document.body.appendChild(el);
  const labels = el.querySelectorAll('.tf-chart__axis--x .tf-chart__axis-label');
  const texts = Array.from(labels).map((l) => l.textContent);
  assert(texts.includes('A') && texts.includes('B') && texts.includes('C'));
});

test('LineChart reactive: store update rebuilds SVG z nową liczbą punktów', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 2);
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('s'), op: { kind: 'set', value: [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 3 }, { x: 3, y: 4 }, { x: 4, y: 5 }] } }],
  });
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 5);
});

test('LineChart tooltip jest renderowany gdy enabled=true', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    tooltip: chartTooltip({ enabled: true }),
  })));
  assert(el.querySelector('.tf-chart__tooltip') != null);
});

test('LineChart tooltip NIE renderowany gdy enabled=false', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    tooltip: chartTooltip({ enabled: false }),
  })));
  assertEq(el.querySelector('.tf-chart__tooltip'), null);
});

test('LineChart series.points overlay (circles) per data point', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }, { x: 2, y: 3 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 3);
});

test('LineChart axis label invalid scale throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    xAxis: chartAxis({ scale: 'parabolic' }),
  }))));
});

test('LineChart pusta series throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({ series: [] }))));
});

test('LineChart duplicate series id throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('dup', PATH('a')), chartSeries('dup', PATH('b'))],
  }))));
});

test('LineChart series id invalid grammar throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('Bad ID!', PATH('s'))],
  }))));
});

test('LineChart axis min >= max throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    yAxis: chartAxis({ min: 10, max: 5 }),
  }))));
});

test('LineChart height_px=0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    heightPx: 0,
  }))));
});

test('LineChart invalid zoom mode throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    zoom: 'pinch',
  }))));
});

test('LineChart ValueFormat currency z extra decimals throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    yAxis: chartAxis({ scale: 'linear' }).concat([[1, { kind: 'currency', code: 'USD', decimals: 2 }]]),
  }))));
});

test('LineChart log scale skips zero/negative values', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 1, y: 10 }, { x: 0, y: 100 }, { x: -5, y: 50 }, { x: 100, y: 1000 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    xAxis: chartAxis({ scale: 'log' }),
  })));
  document.body.appendChild(el);
  // Tylko pozytywne X (1, 100) widoczne — 2 punkty.
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 2);
});

test('LineChart log scale z axis.min <= 0 throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    yAxis: chartAxis({ scale: 'log', min: 0 }),
  }))));
});

test('LineChart ChartSeries duplicate FieldMap key throws', () => {
  setup();
  const engine = makeEngine();
  const dupSeries = [
    [0, 'a'], [0, 'a'], [1, { kind: 'literal', value: 'A' }],
    [2, PATH('s')], [4, 'solid'], [5, true],
  ];
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({ series: [dupSeries] }))));
});

test('LineChart ChartAxis unknown field throws', () => {
  setup();
  const engine = makeEngine();
  const badAxis = [[5, 'linear'], [99, 'x']];
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    xAxis: badAxis,
  }))));
});

test('LineChart wszystkie 7 tonów renderowane z odpowiednią klasą', () => {
  setup();
  const tones = ['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted'];
  const store = makeStore();
  const entries = tones.map((t, i) => ({ path: PATH(`s${i}`), value: [{ x: 0, y: 1 }, { x: 1, y: 2 }] }));
  store.applySnapshot({ entries, state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: tones.map((t, i) => chartSeries(`s${i}`, PATH(`s${i}`), { tone: t })),
  })));
  document.body.appendChild(el);
  for (const t of tones) {
    assert(el.querySelector(`.tf-chart__series-line--tone-${t}`) != null, `expected tone-${t}`);
  }
});

test('LineChart brush+zoom mousedown/up emituje range_select', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 10, y: 5 }, { x: 20, y: 3 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    brush: true,
    zoom: 'x',
  })));
  document.body.appendChild(el);
  let got = null;
  el.addEventListener('range_select', (e) => { got = e.detail; });
  const svg = el.querySelector('svg');
  // Stub getBoundingClientRect żeby drag był w plot area.
  svg.getBoundingClientRect = () => ({ left: 0, top: 0, width: 400, height: 200, right: 400, bottom: 200 });
  svg.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mousedown', { bubbles: true, clientX: 60, clientY: 50, cancelable: true }));
  globalThis.document.dispatchEvent(new (globalThis.MouseEvent || globalThis.Event)('mouseup', { bubbles: true, clientX: 200, clientY: 150 }));
  assert(got != null);
  assertEq(got.zoom_mode, 'x');
  assertEq(got.brush, true);
});

test('LineChart unknown field throws', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(LINE_CHART_TAG, [
    ...lineChartFields({ series: [chartSeries('s', PATH('s'))] }),
    [99, 'x'],
  ])));
});

test('LineChart NaN/non-numeric Y values są filtrowane', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }, { x: 1, y: 'foo' }, { x: 2, y: NaN }, { x: 3, y: 4 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  document.body.appendChild(el);
  assertEq(el.querySelectorAll('circle.tf-chart__series-point').length, 2);
});

test('LineChart SVG ma role=img + aria-label', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
  })));
  const svg = el.querySelector('svg');
  assertEq(svg.getAttribute('role'), 'img');
  assert(svg.hasAttribute('aria-label'));
});

test('LineChart legend layout=top na początku, layout=bottom na końcu', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('s'), value: [{ x: 0, y: 1 }] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const elTop = engine.render(comp(LINE_CHART_TAG, lineChartFields({
    series: [chartSeries('s', PATH('s'))],
    legend: chartLegend({ position: 'top' }),
  })));
  // Legend powinien być pierwszym dzieckiem layout'u dla position=top.
  const layout = elTop.querySelector('.tf-chart__layout');
  assert(layout.children[0].classList.contains('tf-chart__legend--position-top'));
});

// ---- <tf-line-chart> component contract (analytics charts) ----

test('tf-line-chart: tooltip domyślnie on, format(x, items) i draw-in linii', () => {
  setup();
  const el = document.createElement('tf-line-chart');
  el.height = 200;
  el.xAxis = { scale: 'category' };
  el.series = [
    { id: 'p50', name: 'p50', tone: 'accent', style: 'solid', showInLegend: true, points: [{ x: 'a', y: 1 }, { x: 'b', y: 2 }] },
    { id: 'p90', name: 'p90', tone: 'primary', style: 'dashed', showInLegend: true, points: [{ x: 'a', y: 3 }, { x: 'b', y: 4 }] },
  ];
  document.body.appendChild(el);
  assert(el.querySelector('.tf-chart__tooltip') != null, 'tooltip on by default');
  assert(el.querySelector('.tf-chart__series-line--tone-accent') != null, 'accent tone class');
  const lines = el.querySelectorAll('.tf-chart__series-line--enter');
  assertEq(lines.length, 2, 'both lines animate on first paint');
  assert(parseFloat(lines[0].style.strokeDashoffset) > 0, 'dashoffset starts at the line length');
  // Custom tooltip format gets the x value and every series at that x.
  let got = null;
  el.tooltip = { format: (x, items) => { got = { x, items }; return `<b>${x}</b>`; } };
  const box = el._lastPlotBox;
  el._svg.getBoundingClientRect = () => ({ left: 0, top: 0 });
  el._svg.dispatchEvent(new MouseEvent('mousemove', { clientX: box.x1 - 1, clientY: (box.y0 + box.y1) / 2 }));
  assertEq(got.x, 'b');
  assertEq(got.items.map((i) => [i.seriesId, i.y, i.tone]), [['p50', 2, 'accent'], ['p90', 4, 'primary']]);
  assertEq(el.querySelector('.tf-chart__tooltip').innerHTML, '<b>b</b>');
  // Default format: title + one row per series with exact values.
  el.tooltip = { enabled: true };
  el._svg.getBoundingClientRect = () => ({ left: 0, top: 0 });
  el._svg.dispatchEvent(new MouseEvent('mousemove', { clientX: box.x0 + 1, clientY: (box.y0 + box.y1) / 2 }));
  const tip = el.querySelector('.tf-chart__tooltip');
  assertEq(tip.querySelector('.tf-chart__tooltip-title').textContent, 'a');
  assertEq(tip.querySelectorAll('.tf-chart__tooltip-row').length, 2);
  assertEq(el.querySelector('.tf-chart__crosshair').getAttribute('x1'), String(box.x0 + (box.x1 - box.x0) / 4));
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
