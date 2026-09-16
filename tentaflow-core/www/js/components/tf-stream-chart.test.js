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

test('the slide is paced by the measured interval, not by a fixed sprint', async () => {
  const el = makeChart();
  const layer = el.querySelector('.tf-chart__stream-layer');
  // One 5 s poll: the layer is placed one sample to the right and eased back.
  el.push(T0 + 5_000, { read: 25, write: 2 });
  assert.equal(el.querySelector('.tf-chart__stream-layer') === layer, true, 'the layer is re-projected, not rebuilt');
  assert.match(layer.style.transform, /^translateX\([\d.]+px\)$/, 'starts one sample to the right');
  await new Promise((r) => requestAnimationFrame(r));
  // Nearly the whole measured interval, not a sprint followed by a freeze —
  // only the wait for this animation frame is deducted (see below).
  const ms = Number(/transform ([\d.]+)ms linear/.exec(layer.style.transition)[1]);
  assert.equal(ms > 4_000 && ms <= 5_000, true, `paced by the 5 s interval, got ${ms}ms`);
  assert.equal(layer.style.transform, 'translateX(0)');
  el.remove();
});

// The transition cannot begin until the next animation frame, and that frame
// is up to a refresh period away. Charging the full interval to a slide that
// starts late overruns the next sample by exactly that much on every cycle, so
// the wait is measured and deducted from both the duration and the distance
// still to travel.
test('the wait for the animation frame is deducted from the slide, not added to it', async () => {
  const el = makeChart();
  const layer = el.querySelector('.tf-chart__stream-layer');
  const raf = globalThis.requestAnimationFrame;
  // A frame that arrives 40 ms late, which is what a busy tab really does.
  globalThis.requestAnimationFrame = (cb) => setTimeout(cb, 40);
  try {
    el.push(T0 + 5_000, { read: 25, write: 2 });
    const startOffset = Number(/translateX\(([\d.]+)px\)/.exec(layer.style.transform)[1]);
    await new Promise((r) => setTimeout(r, 80));
    assert.equal(startOffset > 0, true, 'the layer is placed one sample to the right');
    const ms = Number(/transform ([\d.]+)ms linear/.exec(layer.style.transition)[1]);
    assert.equal(ms < 5_000 - 30, true, `the lost frame is subtracted, got ${ms}ms`);
    assert.equal(ms > 4_000, true, `but only the lost frame, got ${ms}ms`);
    assert.equal(layer.style.transform, 'translateX(0)');
  } finally {
    globalThis.requestAnimationFrame = raf;
    el.remove();
  }
});

test('a gap wider than the window places the samples instead of crawling across the plot', () => {
  const el = makeChart({ windowSecs: 60 });
  const layer = el.querySelector('.tf-chart__stream-layer');
  // Two windows without a sample (a hidden tab): nothing on screen carries
  // over, so there is no continuous motion to show.
  el.push(T0 + 120_000, { read: 25, write: 2 });
  assert.equal(el.querySelector('.tf-chart__stream-layer') === layer, true, 'still the same layer');
  assert.equal(layer.style.transform, '', 'no slide at all');
  el.remove();
});

// The wide-gap return used to fire BEFORE the pending frame was cancelled, so
// a slide queued by the previous push stayed armed and still applied a
// transform computed from the OLD delta — in exactly the hidden-tab case the
// return exists for. The frame is now dropped and the layer put back at rest
// before any decision not to slide.
test('a wide gap drops the frame a previous push queued instead of sliding on it', async () => {
  const el = makeChart({ windowSecs: 60 });
  const layer = el.querySelector('.tf-chart__stream-layer');
  // A normal 5 s sample arms a slide…
  el.push(T0 + 5_000, { read: 25, write: 2 });
  assert.match(layer.style.transform, /^translateX\([\d.]+px\)$/, 'the slide is armed');

  // …and the tab is hidden for two windows before the next one arrives, so the
  // queued frame never ran.
  el.push(T0 + 200_000, { read: 26, write: 2 });
  assert.equal(el.querySelector('.tf-chart__stream-layer') === layer, true, 'still the same layer');
  assert.equal(layer.style.transform, '', 'the pending slide is dropped, not applied');

  await new Promise((r) => requestAnimationFrame(r));
  await new Promise((r) => requestAnimationFrame(r));
  assert.equal(layer.style.transform, '', 'and no stale frame moves the layer afterwards');
  assert.equal(/transform [\d.]+ms/.test(layer.style.transition || ''), false, 'no transition was started');
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
