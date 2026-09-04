// =============================================================================
// File: components/tf-bloch-sphere.test.js
// Description: <tf-bloch-sphere> is a picture, so what is tested is the maths
// behind it — the slerp that makes a gate look like a turn, purity, the camera
// projection — plus the parts a screen reader and a keyboard actually reach.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');

const {
  slerpVector, purityFromVector, projectVector, vectorLength, ketFor, blochVectorList,
  TfBlochSphere,
} = await import('./tf-bloch-sphere.js');

const close = (a, b, tolerance = 1e-9) => Math.abs(a - b) < tolerance;

// ---- geometry --------------------------------------------------------------

test('slerp turns along the great circle instead of cutting through the sphere', () => {
  const half = slerpVector([0, 0, 1], [1, 0, 0], 0.5);
  assert.ok(close(vectorLength(half), 1), 'a pure state stays on the surface');
  const expected = Math.SQRT1_2;
  assert.ok(close(half[0], expected, 1e-12));
  assert.ok(close(half[2], expected, 1e-12));
  assert.ok(close(half[1], 0, 1e-12));
});

test('slerp pins both ends of the interval', () => {
  assert.deepEqual(slerpVector([0, 0, 1], [1, 0, 0], 0).map(Math.round), [0, 0, 1]);
  assert.deepEqual(slerpVector([0, 0, 1], [1, 0, 0], 1).map(Math.round), [1, 0, 0]);
  // Out-of-range fractions clamp rather than extrapolating off the sphere.
  assert.deepEqual(slerpVector([0, 0, 1], [1, 0, 0], 2).map(Math.round), [1, 0, 0]);
});

test('slerp interpolates the length, so a collapsing state shrinks on the way', () => {
  const half = slerpVector([0, 0, 1], [0, 0, 0.5], 0.5);
  assert.ok(close(vectorLength(half), 0.75, 1e-12));
});

test('antipodal states still travel over the surface, not through the origin', () => {
  const half = slerpVector([0, 0, 1], [0, 0, -1], 0.5);
  assert.ok(close(vectorLength(half), 1, 1e-9), `|r| was ${vectorLength(half)}`);
  assert.ok(close(half[2], 0, 1e-9), 'the halfway point sits on the equator');
});

test('a vector that starts at the origin is interpolated linearly', () => {
  const half = slerpVector([0, 0, 0], [0, 0, 1], 0.5);
  assert.ok(close(half[2], 0.5, 1e-12));
});

test('purity follows the Bloch length: pure on the surface, half at the centre', () => {
  assert.ok(close(purityFromVector([0, 0, 1]), 1));
  assert.ok(close(purityFromVector([0, 0, 0]), 0.5));
  assert.ok(close(purityFromVector([0, 0, 0.5]), 0.625));
});

test('the camera puts |0> at the top and |1> at the bottom', () => {
  const radius = 40;
  const top = projectVector([0, 0, 1], 0, 0, radius);
  const bottom = projectVector([0, 0, -1], 0, 0, radius);
  assert.ok(close(top.y, -radius, 1e-12));
  assert.ok(close(bottom.y, radius, 1e-12));
  assert.ok(close(top.x, 0, 1e-12));
});

test('yaw turns the equator and depth says which half is facing the viewer', () => {
  const front = projectVector([0, -1, 0], 0, 0, 10);
  const back = projectVector([0, 1, 0], 0, 0, 10);
  assert.ok(front.depth < 0 !== back.depth < 0, 'the two sides differ in sign');
  const turned = projectVector([1, 0, 0], Math.PI / 2, 0, 10);
  assert.ok(close(turned.x, 0, 1e-12), 'a quarter turn takes +x off the screen axis');
});

test('ketFor recognises the six cardinal states and nothing else', () => {
  assert.equal(ketFor([0, 0, 1]), '|0⟩');
  assert.equal(ketFor([0, 0, -1]), '|1⟩');
  assert.equal(ketFor([1, 0, 0]), '|+⟩');
  assert.equal(ketFor([-1, 0, 0]), '|−⟩');
  assert.equal(ketFor([0, 1, 0]), '|+i⟩');
  assert.equal(ketFor([0.6, 0, 0.8]), null);
  assert.equal(ketFor([0, 0, 0.5]), null, 'a mixed state has no ket');
});

// ---- the element -----------------------------------------------------------

function mount(attributes = {}) {
  const el = new TfBlochSphere();
  for (const [name, value] of Object.entries(attributes)) el.setAttribute(name, value);
  el.setAttribute('animate', 'off');
  document.body.appendChild(el);
  return el;
}

test('the sphere describes itself for a screen reader', () => {
  const el = mount({ label: 'q0' });
  el.vector = [0, 0, 1];
  const label = el.querySelector('canvas').getAttribute('aria-label');
  assert.match(label, /q0/);
  assert.match(label, /\|0⟩/);
  assert.match(label, /pure/);
  el.remove();
});

test('a mixed qubit is chipped as entangled and says so out loud', () => {
  const el = mount({ label: 'q1' });
  el.vector = [0, 0, 0.2];
  assert.equal(el.entangled, true);
  assert.equal(el.querySelector('.tf-bloch__chip').textContent, 'entangled');
  assert.match(el.querySelector('canvas').getAttribute('aria-label'), /entangled/);
  el.vector = [0, 0, 1];
  assert.equal(el.querySelector('.tf-bloch__chip'), null);
  el.remove();
});

test('labels replace the English fallbacks without touching the markup', () => {
  const el = mount();
  el.labels = { entangled: 'splątany' };
  el.vector = [0, 0, 0];
  assert.equal(el.querySelector('.tf-bloch__chip').textContent, 'splątany');
  el.remove();
});

test('the component records its own trail of the states it has been given', () => {
  const el = mount({ 'trail-length': '2' });
  el.vector = [0, 0, 1];
  el.vector = [1, 0, 0];
  el.vector = [0, 1, 0];
  assert.equal(el.trail.length, 2, 'the trail is capped by trail-length');
  el.clearTrail();
  assert.equal(el.trail.length, 0);
  el.remove();
});

test('an explain sentence appears under the sphere and hides when empty', () => {
  const el = mount();
  assert.equal(el.querySelector('.tf-bloch__explain').hidden, true);
  el.explain = 'H puts q0 on the equator.';
  assert.equal(el.querySelector('.tf-bloch__explain').textContent, 'H puts q0 on the equator.');
  assert.equal(el.querySelector('.tf-bloch__explain').hidden, false);
  el.remove();
});

test('arrow keys orbit the sphere and report the new camera', () => {
  const el = mount();
  let detail = null;
  el.addEventListener('orbit', (event) => { detail = event.detail; });
  el.querySelector('canvas')
    .dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  assert.ok(detail, 'orbit fired');
  assert.equal(typeof detail.yaw, 'number');
  assert.equal(typeof detail.pitch, 'number');
  el.remove();
});

test('an explicit purity overrides the one the vector implies', () => {
  const el = mount();
  el.vector = [0, 0, 1];
  assert.ok(close(el.purity, 1));
  el.purity = 0.5;
  assert.equal(el.purity, 0.5);
  assert.equal(el.entangled, true);
  el.remove();
});

// ---- the state payloads the simulator actually produces ---------------------

test('blochVectorList accepts the flat vector and the nested keyframe alike', () => {
  const flat = blochVectorList(new Float64Array([1, 0, 0, 0, 0, -1]));
  assert.deepEqual(flat, [[1, 0, 0], [0, 0, -1]]);
  // `keyframe()` serialises Vec<[f64; 3]>, so its `bloch` is nested.
  assert.deepEqual(blochVectorList({ bloch: [[0, 1, 0], [0, 0, 1]] }), [[0, 1, 0], [0, 0, 1]]);
  assert.deepEqual(blochVectorList([[0.5, 0, 0]]), [[0.5, 0, 0]]);
  assert.deepEqual(blochVectorList(null), []);
  assert.deepEqual(blochVectorList({ bloch: [] }), []);
  // A trailing partial triple is not a qubit and is dropped rather than padded.
  assert.deepEqual(blochVectorList([1, 0, 0, 0]), [[1, 0, 0]]);
});

// ---- animation -------------------------------------------------------------
// The arrow moving IS the deliverable (§13.6), so the frame loop is driven by
// hand: a fake clock and a fake rAF queue, because a real one makes "does it
// ever reach t = 1" a race.

function withFakeMotion(reduce, body) {
  const realRaf = globalThis.requestAnimationFrame;
  const realCancel = globalThis.cancelAnimationFrame;
  const realPerformance = globalThis.performance;
  const realMatchMedia = window.matchMedia;
  const frames = [];
  const clock = { time: 0 };
  globalThis.requestAnimationFrame = (callback) => frames.push(callback);
  globalThis.cancelAnimationFrame = () => {};
  globalThis.performance = { now: () => clock.time };
  window.matchMedia = (query) => ({
    matches: reduce && query.includes('prefers-reduced-motion'),
    media: query,
    addEventListener() {},
    removeEventListener() {},
  });
  try {
    body({
      frames,
      run(at) {
        clock.time = at;
        const pending = frames.splice(0, frames.length);
        for (const callback of pending) callback(at);
      },
    });
  } finally {
    globalThis.requestAnimationFrame = realRaf;
    globalThis.cancelAnimationFrame = realCancel;
    globalThis.performance = realPerformance;
    window.matchMedia = realMatchMedia;
  }
}

function mountAnimated(attributes = {}) {
  const el = new TfBlochSphere();
  for (const [name, value] of Object.entries(attributes)) el.setAttribute(name, value);
  document.body.appendChild(el);
  return el;
}

test('a new vector slerps over the frames and lands exactly on the target', () => {
  withFakeMotion(false, ({ frames, run }) => {
    // A fresh sphere already sits at |0>, so this is one turn from north pole
    // to the equator and nothing has been scheduled before it.
    const el = mountAnimated({ duration: '200' });
    el.vector = [1, 0, 0];
    assert.equal(frames.length, 1, 'the first frame is scheduled');

    run(100);
    assert.ok(vectorLength(el._drawn) > 0.99, 'the arrow stays on the surface mid-turn');
    assert.ok(el._drawn[0] > 0 && el._drawn[2] > 0, 'it is between the two poles, not at either');
    assert.equal(frames.length, 1, 'and asks for another frame');

    run(200);
    assert.ok(close(el._drawn[0], 1, 1e-9), 'the last frame lands on the target');
    assert.ok(close(el._drawn[2], 0, 1e-9));
    assert.equal(frames.length, 0, 'and stops asking');
    el.remove();
  });
});

test('a reader who asked for reduced motion gets the final state with no frames', () => {
  withFakeMotion(true, ({ frames }) => {
    const el = mountAnimated({ duration: '200' });
    el.vector = [0, 0, 1];
    el.vector = [1, 0, 0];
    assert.equal(frames.length, 0, 'prefers-reduced-motion schedules nothing');
    assert.deepEqual(el._drawn, [1, 0, 0], 'the arrow is already there');
    el.remove();
  });
});

test('animate="off" pins the arrow even when motion is allowed', () => {
  withFakeMotion(false, ({ frames }) => {
    const el = mountAnimated({ duration: '200', animate: 'off' });
    el.vector = [1, 0, 0];
    assert.equal(frames.length, 0);
    assert.deepEqual(el._drawn, [1, 0, 0]);
    el.remove();
  });
});
