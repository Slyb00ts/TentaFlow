// =============================================================================
// File: components/tf-density-plot.test.js
// Description: The density plot reads a matrix two ways (a heat grid and an
// isometric city), so what is tested is the reading: both wire shapes of a
// complex matrix, the diverging scale that keeps a negative entry negative, the
// painter's order of the city, and the refusal to draw something that is not a
// square matrix.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  basisLabels, cityLayout, complexEntries, densityCells, densityColor, TfDensityPlot,
} = await import('./tf-density-plot.js');

/// The Bell state's ρ: 0.5 on the four corners, zero everywhere else.
const BELL = [
  [0.5, 0], [0, 0], [0, 0], [0.5, 0],
  [0, 0], [0, 0], [0, 0], [0, 0],
  [0, 0], [0, 0], [0, 0], [0, 0],
  [0.5, 0], [0, 0], [0, 0], [0.5, 0],
];

test('both wire shapes of a complex matrix read the same', () => {
  const nested = complexEntries([[1, 2], [3, 4]]);
  const flat = complexEntries([1, 2, 3, 4]);
  assert.deepEqual(nested, flat);
});

test('the cells of the Bell matrix are the four corners', () => {
  const { dim, cells, peak } = densityCells(BELL, 're');
  assert.equal(dim, 4);
  assert.equal(peak, 0.5);
  const heavy = cells.filter((c) => c.value > 0).map((c) => `${c.row}${c.col}`);
  assert.deepEqual(heavy, ['00', '03', '30', '33']);
});

test('the imaginary part of a real matrix is empty, which is a fact and not a failure', () => {
  const { cells, peak } = densityCells(BELL, 'im');
  assert.equal(peak, 0);
  assert.ok(cells.every((c) => c.value === 0));
});

test('a matrix that is not square is refused rather than padded', () => {
  assert.equal(densityCells([[1, 0], [0, 0], [0, 0]], 're').dim, 0);
  assert.equal(densityCells([], 're').dim, 0);
});

test('the scale diverges: a negative entry is a different colour, not a smaller one', () => {
  const positive = densityColor(0.5, 0.5);
  const negative = densityColor(-0.5, 0.5);
  assert.notEqual(positive, negative);
  assert.match(positive, /^rgba\(167, 139, 250/);
  assert.match(negative, /^rgba\(244, 114, 182/);
  assert.equal(densityColor(0, 0.5), 'transparent');
});

test('the basis labels are the register the matrix is indexed by', () => {
  assert.deepEqual(basisLabels(4), ['00', '01', '10', '11']);
  assert.deepEqual(basisLabels(2), ['0', '1']);
});

test('the city plot grows a negative bar downward and paints from the back', () => {
  const cells = [
    { row: 0, col: 0, value: 1 },
    { row: 1, col: 1, value: -1 },
  ];
  const bars = cityLayout(cells, 2, { size: 200, peak: 1 });
  assert.deepEqual(bars.map((b) => `${b.row}${b.col}`), ['00', '11'], 'far corner first');
  assert.ok(bars[0].apex.y < bars[0].base.y, 'a positive bar rises');
  assert.ok(bars[1].apex.y > bars[1].base.y, 'a negative bar sinks');
});

test('the element draws a heat grid with its labels, and switches to the city', () => {
  const el = new TfDensityPlot();
  document.body.appendChild(el);
  el.matrix = { dim: 4, rho: BELL };
  assert.equal(el.querySelectorAll('.tf-density__cell').length, 16);
  assert.equal(el.querySelectorAll('.tf-density__col-label').length, 4);
  el.setAttribute('mode', 'city');
  assert.equal(el.querySelectorAll('.tf-density__cell').length, 0);
  assert.equal(el.querySelectorAll('.tf-density__bar').length, 4, 'only the non-zero entries');
  el.remove();
});

test('an empty matrix says so', () => {
  const el = new TfDensityPlot();
  document.body.appendChild(el);
  el.labels = { empty: 'no matrix here' };
  el.matrix = { dim: 0, rho: [] };
  assert.match(el.textContent, /no matrix here/);
  el.remove();
});
