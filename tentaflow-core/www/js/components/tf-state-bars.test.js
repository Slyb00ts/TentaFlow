// =============================================================================
// File: components/tf-state-bars.test.js
// Description: The amplitude bars morph while the animation runs, so the axis
// has to stay put: what is tested is the selection of bars (the heaviest ones,
// put BACK in basis order), the height rule, and the phase colour the bar and
// the amplitude table share.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { barHeight, barsFor, TfStateBars } = await import('./tf-state-bars.js');
const { amplitudeRows, phaseColor } = await import('./tf-mime-output.js');

const R2 = Math.SQRT1_2;
const rows = (state, numQubits) => amplitudeRows(state, numQubits);

test('the heaviest bars are kept but drawn in basis order, so the axis is monotonic', () => {
  const state = { numQubits: 2, top: [
    { index: 3, amplitude: [0.9, 0] },
    { index: 0, amplitude: [0.3, 0] },
    { index: 1, amplitude: [0.2, 0] },
  ] };
  const { bars, hidden } = barsFor(rows(state, 2), 2);
  assert.deepEqual(bars.map((b) => b.index), [0, 3]);
  assert.equal(hidden, 1);
});

test('the height is the share of the peak, and a tiny amplitude keeps a sliver', () => {
  assert.equal(barHeight(0.5, 0.5), 100);
  assert.equal(barHeight(0.25, 0.5), 50);
  assert.ok(barHeight(1e-9, 0.5) >= 1.5);
  assert.equal(barHeight(0.5, 0), 0, 'a peak of zero has no bars at all');
});

test('the element draws one column per bar with its phase colour', () => {
  const el = new TfStateBars();
  document.body.appendChild(el);
  el.state = { numQubits: 2, top: [
    { index: 0, amplitude: [R2, 0] },
    { index: 3, amplitude: [0, R2] },
  ] };
  const columns = el.querySelectorAll('.tf-bars__col');
  assert.equal(columns.length, 2);
  const bars = el.querySelectorAll('.tf-bars__bar');
  assert.equal(bars[0].style.getPropertyValue('--tf-bar-phase'), phaseColor(0));
  assert.equal(bars[1].style.getPropertyValue('--tf-bar-phase'), phaseColor(Math.PI / 2));
  assert.deepEqual(Array.from(el.querySelectorAll('.tf-bars__axis span'), (s) => s.textContent), ['|00⟩', '|11⟩']);
  assert.match(el.getAttribute('aria-label'), /50\.0%/);
  el.remove();
});

test('a state with no amplitudes says so', () => {
  const el = new TfStateBars();
  document.body.appendChild(el);
  el.labels = { empty: 'nothing' };
  el.state = { numQubits: 2, top: [] };
  assert.equal(el.querySelectorAll('.tf-bars__bar').length, 0);
  assert.match(el.textContent, /nothing/);
  el.remove();
});

test('the hidden-state footer states how many bars were left out', () => {
  const el = new TfStateBars();
  document.body.appendChild(el);
  el.setAttribute('max-bars', '2');
  el.labels = { more: 'left out: {n}' };
  el.state = { numQubits: 2, top: [0, 1, 2, 3].map((index) => ({ index, amplitude: [0.5, 0] })) };
  assert.match(el.textContent, /left out: 2/);
  el.remove();
});
