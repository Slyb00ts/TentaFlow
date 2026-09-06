// =============================================================================
// File: components/tf-line-chart.test.js
// Description: Tests for <tf-line-chart>'s incremental `updateSeries()` path
// (and the `series` setter that now delegates to it) — the "live poll" case
// a chart must handle without redrawing from zero: same-shape data swaps
// patch the existing <polyline>/<circle> attributes and preserve the SVG
// root, tooltip, legend and hover state; any shape change (series added/
// removed/recolored/relabeled) still falls back to the original full
// `_render()` rebuild, exactly like every consumer that assigns `series`
// once already relies on.
//
// NOTE: requires `happy-dom` (see package.json devDependencies). Not
// installed in this checkout — `node --test` on this file currently fails
// at import with ERR_MODULE_NOT_FOUND for 'happy-dom', same as every other
// *.test.js under js/sdk-runtime and js/components that imports
// _dom-test-harness.js. Written and reviewed against the actual component
// source; not executed in this environment.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}

const { TfLineChart } = await import('./tf-line-chart.js');

// ---- helpers ---------------------------------------------------------------

function makeChart() {
  const chart = new TfLineChart();
  chart.xAxis = { scale: 'category', min: null, max: null, ticks: null, format: null };
  chart.yAxis = { scale: 'linear', min: 0, max: null, ticks: 4, format: null };
  chart.legend = { position: 'bottom', alignment: 'start' };
  document.body.appendChild(chart);
  return chart;
}

function twoSeries(labels, msgsIn, lag) {
  return [
    {
      id: 'msgsIn', name: 'Messages in', tone: 'primary', style: 'solid',
      showInLegend: true, points: labels.map((x, i) => ({ x, y: msgsIn[i] })),
    },
    {
      id: 'lag', name: 'Consumer lag', tone: 'warning', style: 'dashed',
      showInLegend: true, points: labels.map((x, i) => ({ x, y: lag[i] })),
    },
  ];
}

function polylinesById(chart) {
  const map = new Map();
  for (const p of chart.querySelectorAll('polyline.tf-chart__series-line')) {
    map.set(p.getAttribute('data-series-id'), p);
  }
  return map;
}

// ============================================================================

test('first `series` assignment renders fully (svg + gridlines + polylines)', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1', 't2'], [1, 2, 3], [0, 1, 0]);
  assert.ok(chart._svg, 'svg root created');
  assert.equal(chart.querySelectorAll('polyline.tf-chart__series-line').length, 2);
  assert.equal(chart.querySelectorAll('.tf-chart__axis--x').length, 1);
  assert.equal(chart.querySelectorAll('.tf-chart__axis--y').length, 1);
});

test('same-shape series update takes the incremental path: svg/legend/tooltip identity preserved', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1'], [1, 2], [0, 1]);
  const svgBefore = chart._svg;
  const legendBefore = chart._legendEl;
  const tooltipBefore = chart._tooltipEl;
  const linesBefore = polylinesById(chart);
  const msgsInPointsBefore = linesBefore.get('msgsIn').getAttribute('points');

  chart.updateSeries(twoSeries(['t0', 't1', 't2'], [1, 2, 5], [0, 1, 2]));

  assert.equal(chart._svg, svgBefore, 'svg root not recreated');
  assert.equal(chart._legendEl, legendBefore, 'legend not rebuilt');
  assert.equal(chart._tooltipEl, tooltipBefore, 'tooltip element not rebuilt');
  const linesAfter = polylinesById(chart);
  assert.equal(linesAfter.get('msgsIn'), linesBefore.get('msgsIn'), 'polyline element reused, not recreated');
  assert.equal(linesAfter.get('lag'), linesBefore.get('lag'), 'polyline element reused, not recreated');
  // Geometry did update to the new window (same element, new attribute value).
  assert.notEqual(linesAfter.get('msgsIn').getAttribute('points'), msgsInPointsBefore);
});

test('open tooltip/hover state survives an incremental update', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1'], [1, 2], [0, 1]);
  chart._tooltipEl.hidden = false;
  chart.updateSeries(twoSeries(['t0', 't1', 't2'], [1, 2, 3], [0, 1, 2]));
  assert.equal(chart._tooltipEl.hidden, false, 'incremental update must not force-hide an open tooltip');
});

test('series shape change (extra series) falls back to a full rebuild', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1'], [1, 2], [0, 1]);
  const svgBefore = chart._svg;
  chart.updateSeries([
    ...twoSeries(['t0', 't1'], [1, 2], [0, 1]),
    { id: 'extra', name: 'Extra', tone: 'info', style: 'solid', showInLegend: true, points: [{ x: 't0', y: 1 }] },
  ]);
  assert.notEqual(chart._svg, svgBefore, 'a shape change must rebuild the svg root');
  assert.equal(chart.querySelectorAll('polyline.tf-chart__series-line').length, 3);
});

test('series shape change (recolored series) falls back to a full rebuild', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1'], [1, 2], [0, 1]);
  const svgBefore = chart._svg;
  const changed = twoSeries(['t0', 't1'], [1, 2], [0, 1]);
  changed[0].tone = 'critical';
  chart.updateSeries(changed);
  assert.notEqual(chart._svg, svgBefore, 'a tone change must rebuild the svg root');
});

test('the `series` setter itself takes the incremental path for a compatible shape', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1'], [1, 2], [0, 1]);
  const svgBefore = chart._svg;
  chart.series = twoSeries(['t0', 't1', 't2'], [1, 2, 3], [0, 1, 2]);
  assert.equal(chart._svg, svgBefore, '`series =` on a compatible shape must not rebuild the svg root');
});

test('a ring-buffer scroll step plays a translateX transition on the series layer', () => {
  const chart = makeChart();
  // Fill a 3-slot window, then push one more sample past the window (the
  // steady-state case `pushWindowSample`/tentabus.js produces every poll
  // once MAX_CHART_POINTS is reached: same slot count, shifted by one).
  chart.series = twoSeries(['t0', 't1', 't2'], [1, 2, 3], [0, 1, 0]);
  chart.updateSeries(twoSeries(['t1', 't2', 't3'], [2, 3, 4], [1, 0, 2]), { animate: true });
  const transform = chart._seriesLayerEl.style.transform;
  assert.ok(transform.includes('translateX'), `expected a translateX transform, got "${transform}"`);
});

test('prefers-reduced-motion (or animate:false) applies the new geometry without a transition', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0', 't1', 't2'], [1, 2, 3], [0, 1, 0]);
  chart.updateSeries(twoSeries(['t1', 't2', 't3'], [2, 3, 4], [1, 0, 2]), { animate: false });
  assert.equal(chart._seriesLayerEl.style.transform, '', 'no transform should be applied without animation');
  // Geometry still updated in place.
  const line = polylinesById(chart).get('msgsIn');
  assert.ok(line.getAttribute('points').length > 0);
});

test('updateSeries on an unconnected/unrendered chart falls back to a full render (no crash)', () => {
  const chart = new TfLineChart();
  chart.updateSeries(twoSeries(['t0'], [1], [0]));
  assert.ok(chart._svg, 'first updateSeries call must still produce a full render');
});

test('non-array series values are tolerated (empty chart), matching the old setter contract', () => {
  const chart = makeChart();
  chart.series = twoSeries(['t0'], [1], [0]);
  // @ts-expect-error intentional bad input, mirrors the pre-existing guard
  chart.series = null;
  assert.deepEqual(chart._series, []);
});
