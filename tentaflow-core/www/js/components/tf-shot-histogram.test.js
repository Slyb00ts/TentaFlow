// =============================================================================
// File: components/tf-shot-histogram.test.js
// Description: A histogram is a picture of a distribution, so what is tested is
// the distribution maths behind it — the Wilson interval that draws the
// whiskers, the shared axis two series are aligned on, the log scale, and the
// two agreement numbers (TVD, Hellinger fidelity) the run view prints.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  barHeight, hellingerFidelity, histogramAxis, histogramLayout, seriesProbabilities,
  seriesShots, totalVariationDistance, wilsonInterval, TfShotHistogram,
} = await import('./tf-shot-histogram.js');

const close = (a, b, tolerance = 1e-9) => Math.abs(a - b) < tolerance;

// ---- distributions ---------------------------------------------------------

test('counts are normalised by the series own shot total', () => {
  const p = seriesProbabilities({ counts: { '00': 300, '11': 100 }, shots: 400 });
  assert.ok(close(p.get('00'), 0.75));
  assert.ok(close(p.get('11'), 0.25));
});

test('a series that states no shot total is normalised by its own sum', () => {
  const p = seriesProbabilities({ counts: { a: 1, b: 3 } });
  assert.ok(close(p.get('a'), 0.25));
  assert.equal(seriesShots({ counts: { a: 1, b: 3 } }), 4);
});

test('a series of exact probabilities has no shots, and therefore no whiskers', () => {
  const series = { probabilities: { '00': 0.5, '11': 0.5 } };
  assert.equal(seriesShots(series), 0);
  const [group] = histogramLayout([series]).groups;
  assert.equal(group.bars[0].whisker, null);
  assert.equal(group.bars[0].count, null);
});

test('the axis is the union of the series, heaviest first and then in order', () => {
  const { bitstrings, hidden } = histogramAxis([
    { counts: { '00': 10, '01': 1 } },
    { counts: { '11': 20 } },
  ]);
  assert.deepEqual(bitstrings, ['00', '01', '11']);
  assert.equal(hidden, 0);
});

test('the axis is capped, and says how many states it left out', () => {
  const counts = Object.fromEntries(Array.from({ length: 20 }, (_, i) => [String(i), 20 - i]));
  const { bitstrings, hidden } = histogramAxis([{ counts }], 4);
  // The four heaviest are 0..3; the axis then sorts them lexically.
  assert.deepEqual(bitstrings, ['0', '1', '2', '3']);
  assert.equal(hidden, 16);
});

// ---- the Wilson interval ---------------------------------------------------

test('the Wilson interval of half of a thousand shots is centred and narrow', () => {
  const { low, high, center } = wilsonInterval(512, 1024);
  assert.ok(close(center, 0.5, 1e-3), String(center));
  assert.ok(low > 0.46 && low < 0.5, String(low));
  assert.ok(high > 0.5 && high < 0.54, String(high));
});

test('the Wilson interval of zero successes is not zero-width, which is the point', () => {
  const { low, high } = wilsonInterval(0, 100);
  assert.ok(close(low, 0, 1e-12), String(low));
  assert.ok(high > 0.02 && high < 0.05, `p=0 out of 100 still has an upper bound: ${high}`);
});

test('the Wilson interval of every shot stops at one', () => {
  const { low, high } = wilsonInterval(100, 100);
  assert.equal(high, 1);
  assert.ok(low > 0.95 && low < 1);
});

test('an interval over no shots at all is the whole range', () => {
  assert.deepEqual(wilsonInterval(0, 0), { low: 0, high: 1, center: 0 });
});

// ---- distances -------------------------------------------------------------

test('TVD is zero for one distribution against itself and one for disjoint ones', () => {
  const p = { '00': 0.5, '11': 0.5 };
  assert.ok(close(totalVariationDistance(p, p), 0));
  assert.ok(close(totalVariationDistance(p, { '01': 0.5, '10': 0.5 }), 1));
});

test('TVD counts the states only one distribution has', () => {
  assert.ok(close(totalVariationDistance({ a: 1 }, { a: 0.5, b: 0.5 }), 0.5));
});

test('Hellinger fidelity is one for identical distributions and zero for disjoint ones', () => {
  const p = { '00': 0.5, '11': 0.5 };
  assert.ok(close(hellingerFidelity(p, p), 1, 1e-12));
  assert.ok(close(hellingerFidelity(p, { '01': 1 }), 0));
});

test('Hellinger fidelity of a half-overlap is the square of the overlap sum', () => {
  // (sqrt(0.5*1))^2 = 0.5.
  assert.ok(close(hellingerFidelity({ a: 0.5, b: 0.5 }, { a: 1 }), 0.5, 1e-12));
});

// ---- the scale -------------------------------------------------------------

test('a linear bar is its share of the peak, and a present-but-tiny one stays visible', () => {
  assert.ok(close(barHeight(0.5, 0.5), 100));
  assert.ok(close(barHeight(0.25, 0.5), 50));
  assert.ok(barHeight(1e-6, 0.5) >= 1.2, 'a tiny probability keeps a sliver');
  assert.equal(barHeight(0, 0.5), 0, 'a state that is absent has no bar at all');
});

test('the log scale separates a tail a linear axis flattens', () => {
  const linear = barHeight(0.001, 1);
  const log = barHeight(0.001, 1, { log: true, floor: 0.001 / 10 });
  assert.ok(log > linear * 10, `${log} vs ${linear}`);
});

// ---- the element -----------------------------------------------------------

test('the element draws one group per bitstring and one bar per series', () => {
  const el = new TfShotHistogram();
  document.body.appendChild(el);
  el.series = [
    { id: 'measured', label: 'QPU', tone: 'measured', counts: { '00': 500, '11': 500 }, shots: 1000 },
    { id: 'ideal', label: 'ideal', tone: 'ideal', probabilities: { '00': 0.5, '11': 0.5 } },
  ];
  assert.equal(el.querySelectorAll('.tf-hist__group').length, 2);
  assert.equal(el.querySelectorAll('.tf-hist__bar').length, 4);
  // Only the sampled series carries whiskers.
  assert.equal(el.querySelectorAll('.tf-hist__whisker').length, 2);
  assert.equal(el.querySelectorAll('.tf-hist__key').length, 2);
  assert.ok(el.getAttribute('aria-label').includes('00'));
  el.remove();
});

test('an empty histogram says so instead of drawing an empty axis', () => {
  const el = new TfShotHistogram();
  document.body.appendChild(el);
  el.labels = { empty: 'nothing here' };
  el.series = [];
  assert.equal(el.querySelectorAll('.tf-hist__group').length, 0);
  assert.match(el.textContent, /nothing here/);
  el.remove();
});
