// =============================================================================
// File: components/tf-state-timeline.test.js
// Description: The timeline owns a continuous coordinate over recorded steps
// and no clock, so what is tested is the coordinate: where the playhead sits,
// which step a fraction is inside, how the strip is laid out, and that moving
// the playhead does not rebuild the strip (which is what keeps the animation
// smooth).
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  playheadX, positionParts, stripLayout, SPEEDS, TfStateTimeline,
} = await import('./tf-state-timeline.js');

const STEPS = [
  { step: 1, name: 'h', qubits: [0] },
  { step: 2, name: 'cx', qubits: [0, 1] },
  { step: 3, name: 'measure', qubits: [0], collapsing: true },
];

test('the strip is one column per recorded step and one row per qubit', () => {
  const layout = stripLayout(STEPS, 2, { column: 40, row: 30 });
  assert.equal(layout.columns.length, 3);
  assert.equal(layout.wires, 2);
  assert.equal(layout.height, 60);
  assert.deepEqual(layout.columns.map((c) => c.name), ['h', 'cx', 'measure']);
});

test('the first operand of a two-qubit gate is its control, the rest are targets', () => {
  const [, cx] = stripLayout(STEPS, 2).columns;
  assert.deepEqual(cx.qubits.map((q) => q.role), ['control', 'target']);
  assert.ok(cx.bottom > cx.top, 'the link spans both wires');
});

test('a one-qubit gate is drawn as a box and never as a control dot', () => {
  const [h] = stripLayout(STEPS, 2).columns;
  assert.deepEqual(h.qubits.map((q) => q.role), ['target']);
});

test('the register width is taken from the steps when the host states none', () => {
  assert.equal(stripLayout(STEPS, 0).wires, 2);
});

test('one step is one column wide, so the head crosses the gate at half a step', () => {
  const layout = stripLayout(STEPS, 2, { column: 40 });
  const start = playheadX(layout, 0);
  const middle = playheadX(layout, 0.5);
  const end = playheadX(layout, 1);
  assert.equal(middle - start, 20);
  assert.equal(end - start, 40);
  assert.equal(middle, layout.columns[0].x, 'halfway through step 1 is where its gate is drawn');
});

test('the playhead is clamped to the recording', () => {
  const layout = stripLayout(STEPS, 2, { column: 40 });
  assert.equal(playheadX(layout, -5), playheadX(layout, 0));
  assert.equal(playheadX(layout, 99), playheadX(layout, 3));
});

test('a position names the step it is inside and how far through it is', () => {
  assert.deepEqual(positionParts(0, 3), { step: 0, fraction: 0 });
  assert.deepEqual(positionParts(1, 3), { step: 1, fraction: 1 });
  const inside = positionParts(1.5, 3);
  assert.equal(inside.step, 2);
  assert.ok(Math.abs(inside.fraction - 0.5) < 1e-9);
  assert.deepEqual(positionParts(9, 3), { step: 3, fraction: 1 });
});

test('the element draws the strip once and moves the playhead without rebuilding it', () => {
  const el = new TfStateTimeline();
  document.body.appendChild(el);
  el.numQubits = 2;
  el.steps = STEPS;
  const columns = el.querySelectorAll('[data-column]');
  assert.equal(columns.length, 3);
  el.position = 1.5;
  assert.equal(el.querySelectorAll('[data-column]')[0], columns[0], 'the same nodes are still there');
  assert.ok(columns[0].classList.contains('is-done'));
  assert.ok(columns[1].classList.contains('is-now'));
  assert.ok(!columns[2].classList.contains('is-done'));
  el.remove();
});

test('the transport reports every button, and play flips its own state', () => {
  const el = new TfStateTimeline();
  document.body.appendChild(el);
  el.steps = STEPS;
  const seen = [];
  el.addEventListener('transport', (event) => seen.push(event.detail.action));
  el.querySelector('[data-transport="prev"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  el.querySelector('[data-transport="play"]').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(seen, ['prev', 'play']);
  assert.equal(el.playing, true);
  el.remove();
});

test('clicking a column seeks to the end of that step', () => {
  const el = new TfStateTimeline();
  document.body.appendChild(el);
  el.steps = STEPS;
  let seeked = null;
  el.addEventListener('seek', (event) => { seeked = event.detail.position; });
  el.querySelectorAll('.tf-timeline__hit')[1].dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(seeked, 2);
  assert.equal(el.position, 2);
  el.remove();
});

test('the speed control offers the three speeds of the plan', () => {
  assert.deepEqual(SPEEDS, [0.5, 1, 2]);
  const el = new TfStateTimeline();
  document.body.appendChild(el);
  el.steps = STEPS;
  assert.equal(el.speed, 1);
  el.speed = 2;
  assert.equal(el.querySelector('[data-speed]').getAttribute('value'), '2');
  el.remove();
});
