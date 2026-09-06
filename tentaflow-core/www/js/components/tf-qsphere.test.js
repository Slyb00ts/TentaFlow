// =============================================================================
// File: components/tf-qsphere.test.js
// Description: The Q-sphere is a LAYOUT — every basis state on the latitude of
// its Hamming weight — so the layout is what is tested: the poles, the rings,
// the stability of a ring while amplitudes change, and the area rule for the
// size of a mark.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  hammingWeight, markRadius, qspherePoint, qsphereLayout, TfQsphere,
} = await import('./tf-qsphere.js');

const R2 = Math.SQRT1_2;
const close = (a, b, tolerance = 1e-9) => Math.abs(a - b) < tolerance;

const bellState = () => ({ top: [
  { index: 0, amplitude: [R2, 0] },
  { index: 3, amplitude: [R2, 0] },
], numQubits: 2 });

test('the Hamming weight counts the ones of an index', () => {
  assert.equal(hammingWeight(0), 0);
  assert.equal(hammingWeight(0b1011), 3);
});

test('|0...0> sits on the north pole and |1...1> on the south', () => {
  assert.deepEqual(qspherePoint(0, 3, 0, 1).map((v) => Math.round(v * 1e9) / 1e9), [0, 0, 1]);
  assert.deepEqual(qspherePoint(0b111, 3, 0, 1).map((v) => Math.round(v * 1e9) / 1e9), [0, 0, -1]);
});

test('a weight ring sits on its own latitude and spreads around it', () => {
  // Weight 1 of three qubits: z = 1 - 2/3.
  const [x, y, z] = qspherePoint(0b001, 3, 0, 3);
  assert.ok(close(z, 1 / 3));
  assert.ok(close(Math.hypot(x, y, z), 1), 'every state is on the unit sphere');
  const second = qspherePoint(0b010, 3, 1, 3);
  assert.ok(close(second[2], 1 / 3), 'the same weight is the same latitude');
  assert.ok(Math.hypot(x - second[0], y - second[1]) > 0.5, 'and a different longitude');
});

test('the Bell state puts one mark on each pole', () => {
  const { points, numQubits } = qsphereLayout(bellState());
  assert.equal(numQubits, 2);
  assert.equal(points.length, 2);
  const [zero, three] = points.sort((a, b) => a.index - b.index);
  assert.deepEqual(zero.vector.map(Math.round), [0, 0, 1]);
  assert.deepEqual(three.vector.map(Math.round), [0, 0, -1]);
  assert.ok(close(zero.probability, 0.5));
});

test('dropping a faint state does not rotate the ring the heavy ones are on', () => {
  const state = { numQubits: 3, top: [
    { index: 0b001, amplitude: [0.8, 0] },
    { index: 0b010, amplitude: [0.5, 0] },
    { index: 0b100, amplitude: [0.05, 0] },
  ] };
  const whole = qsphereLayout(state);
  const capped = qsphereLayout(state, 2);
  assert.equal(capped.hidden, 1);
  for (const point of capped.points) {
    const same = whole.points.find((p) => p.index === point.index);
    assert.deepEqual(point.vector, same.vector);
  }
});

test('a mark maps probability to area, so twice the probability is twice the ink', () => {
  const small = markRadius(0.25, 200);
  const big = markRadius(0.5, 200);
  assert.ok(close(big / small, Math.SQRT2, 1e-9));
  assert.ok(markRadius(0, 200) >= 1.5, 'a state that is present but tiny stays visible');
});

test('the element draws a dot and a spoke per state, and an aria label with them', () => {
  const el = new TfQsphere();
  document.body.appendChild(el);
  el.state = bellState();
  assert.equal(el.querySelectorAll('.tf-qsphere__dot').length, 2);
  assert.equal(el.querySelectorAll('.tf-qsphere__spoke').length, 2);
  assert.match(el.querySelector('svg').getAttribute('aria-label'), /\|00⟩/);
  el.remove();
});

test('an orbit keypress turns the sphere and reports it', () => {
  const el = new TfQsphere();
  document.body.appendChild(el);
  el.state = bellState();
  let turned = null;
  el.addEventListener('orbit', (event) => { turned = event.detail; });
  el.querySelector('svg').dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  assert.ok(turned && Number.isFinite(turned.yaw), 'the orbit event carries the new camera');
  el.remove();
});

test('an empty state says so instead of drawing an empty sphere', () => {
  const el = new TfQsphere();
  document.body.appendChild(el);
  el.labels = { empty: 'nothing' };
  el.state = { numQubits: 2, top: [] };
  assert.equal(el.querySelectorAll('.tf-qsphere__dot').length, 0);
  assert.match(el.textContent, /nothing/);
  el.remove();
});

test('the poles are labelled, so the latitude scale is readable', () => {
  const el = new TfQsphere();
  document.body.appendChild(el);
  el.labels = { north: 'weight 0', south: 'all ones' };
  el.state = bellState();
  const poles = [...el.querySelectorAll('.tf-qsphere__pole')].map((p) => p.textContent);
  assert.deepEqual(poles, ['weight 0', 'all ones']);
  el.remove();
});
