// =============================================================================
// File: components/tf-stream-chart.test.js
// Description: tf-stream-chart keeps its SVG nodes across push() calls
// (re-projection in place, no rebuild), drops samples that left the window,
// grows the Y scale when a sample exceeds it and labels the X axis with
// relative offsets.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}

await import('./tf-stream-chart.js');

const T0 = 1_700_000_000_000;

function makeChart({ windowSecs = 60, seed = true } = {}) {
  const el = document.createElement('tf-stream-chart');
  el.animate = false;
  el.window = windowSecs;
  el.series = [
    { id: 'read', name: 'Read', tone: 'primary', style: 'solid', showInLegend: true,
      points: seed ? [{ x: T0 - 10_000, y: 10 }, { x: T0 - 5_000, y: 20 }, { x: T0, y: 30 }] : [] },
    { id: 'write', name: 'Write', tone: 'info', style: 'solid', showInLegend: true,
      points: seed ? [{ x: T0 - 10_000, y: 1 }, { x: T0 - 5_000, y: 2 }, { x: T0, y: 3 }] : [] },
  ];
  document.body.appendChild(el);
  return el;
}

function pointCount(line) {
  const raw = (line.getAttribute('points') || '').trim();
  return raw ? raw.split(' ').length : 0;
}

test('seeded series render one polyline and one area per series inside a clipped layer', () => {
  const el = makeChart();
  const layer = el.querySelector('.tf-chart__stream-layer');
  assert.ok(layer, 'stream layer present');
  assert.match(layer.getAttribute('clip-path'), /^url\(#tf-stream-clip-\d+\)$/);
  const lines = el.querySelectorAll('polyline.tf-chart__series-line');
  const areas = el.querySelectorAll('polygon.tf-chart__area');
  assert.equal(lines.length, 2);
  assert.equal(areas.length, 2);
  assert.equal(pointCount(lines[0]), 3);
  assert.ok(el.querySelector('.tf-chart__axis--x'), 'x axis drawn');
  el.remove();
});

test('push() re-projects the existing polylines instead of rebuilding the svg', () => {
  const el = makeChart();
  const lineBefore = el.querySelector('polyline[data-series-id="read"]');
  const svgBefore = el.querySelector('svg');
  el.push(T0 + 5_000, { read: 25, write: 2 });
  const lineAfter = el.querySelector('polyline[data-series-id="read"]');
  assert.strictEqual(lineAfter, lineBefore, 'polyline node reused');
  assert.strictEqual(el.querySelector('svg'), svgBefore, 'svg node reused');
  assert.equal(pointCount(lineAfter), 4);
  // The right edge is the newest sample: its x maps to the plot's right border.
  const last = lineAfter.getAttribute('points').trim().split(' ').pop().split(',').map(Number);
  const axisLine = el.querySelector('.tf-chart__axis--x .tf-chart__axis-line');
  assert.ok(Math.abs(last[0] - Number(axisLine.getAttribute('x2'))) < 0.01, 'newest sample sits at the right edge');
  el.remove();
});

test('samples older than the window are dropped, keeping one point past the left edge', () => {
  const el = makeChart({ windowSecs: 20 });
  // Window is 20 s; after this push the T0-10s sample is the one point kept
  // outside the window and T0-5s onwards stay inside.
  el.push(T0 + 12_000, { read: 5, write: 1 });
  const line = el.querySelector('polyline[data-series-id="read"]');
  // Points: (T0-10s), T0-5s, T0, T0+12s → 4; the T0-10s point remains only
  // because the next one is already inside the window.
  assert.equal(pointCount(line), 4);
  el.push(T0 + 18_000, { read: 5, write: 1 });
  // Now T0-5s is older than the window edge (T0-2s) → the T0-10s point goes,
  // T0-5s stays as the single outside point.
  assert.equal(pointCount(line), 4);
  el.remove();
});

test('a sample above the current scale redraws the axes with a larger domain', () => {
  const el = makeChart();
  const labelsBefore = [...el.querySelectorAll('.tf-chart__axis--y .tf-chart__axis-label')].map((t) => t.textContent);
  const svgBefore = el.querySelector('svg');
  el.push(T0 + 5_000, { read: 900, write: 3 });
  const labelsAfter = [...el.querySelectorAll('.tf-chart__axis--y .tf-chart__axis-label')].map((t) => t.textContent);
  assert.notDeepEqual(labelsAfter, labelsBefore, 'y axis rescaled');
  assert.ok(labelsAfter.some((l) => l === '1K' || l === '1 tys.' || /^1/.test(l)), `top tick covers 900 (${labelsAfter.join(',')})`);
  assert.strictEqual(el.querySelector('svg'), svgBefore, 'the svg element itself survives the rescale');
  el.remove();
});

test('x axis labels are relative offsets ending at 0', () => {
  const el = makeChart({ windowSecs: 300 });
  const labels = [...el.querySelectorAll('.tf-chart__axis--x .tf-chart__axis-label')].map((t) => t.textContent);
  assert.equal(labels[labels.length - 1], '0');
  assert.equal(labels[0], '-5m');
  el.remove();
});

test('push() with a value for one series only extends that series', () => {
  const el = makeChart({ seed: false });
  el.push(T0, { read: 1 });
  el.push(T0 + 1_000, { read: 2 });
  assert.equal(pointCount(el.querySelector('polyline[data-series-id="read"]')), 2);
  assert.equal(pointCount(el.querySelector('polyline[data-series-id="write"]')), 0);
  el.remove();
});
