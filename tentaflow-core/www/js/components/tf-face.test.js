// ============ File: tf-face.test.js - Motion continuity and speech lifecycle regression tests. ============
import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { TfFace } from './tf-face.js';

test('face motion remains continuous across frame rates, modes, blinks and audio gaps', () => {
  function faceAtRest() {
    const face = new TfFace();
    const s = face._state;
    s.nextBlinkAt = s.nextSmileAt = s.nextBrowAsymAt = Infinity;
    s.nextEmphasisAt = Infinity;
    return face;
  }

  function advance(face, seconds, fps, amplitude) {
    const s = face._state;
    for (let i = 0; i < Math.round(seconds * fps); i++) {
      face.setSpeechAmplitude(amplitude, { round: 0.35, wide: 0.25 });
      s.phase += 1 / fps;
      face._tickIdle(1 / fps, performance.now());
      for (const value of Object.values(s.mimicry)) {
        assert(Number.isFinite(value) && value >= -0.001 && value <= 1.001);
      }
      face._applyBlendshapes(s.mimicry);
      assert(face._workVertices.every(Number.isFinite));
    }
  }

  const trajectories = [30, 60, 120].map((fps) => {
    const face = faceAtRest();
    face.setAttribute('mode', 'speak');
    advance(face, 0.5, fps, 0.8);
    const open = face._state.mimicry.mouth_open;
    assert(open > 0.15);
    advance(face, 0.5, fps, 0);
    assert(face._state.mimicry.mouth_open < 0.001, 'Mouth must settle during silence');
    return open;
  });
  assert(Math.max(...trajectories) - Math.min(...trajectories) < 0.005, 'Frame-rate independent motion');

  const face = faceAtRest();
  face.setAttribute('mode', 'speak');
  advance(face, 1, 60, 0.8);
  const before = { ...face._state.mimicry };
  face.setAttribute('mode', 'think');
  assert.deepEqual(face._state.mimicry, before, 'Changing mode must not reset the rendered pose');
  face._state.phase += 1 / 60;
  face._tickIdle(1 / 60, performance.now());
  assert(Math.abs(face._state.mimicry.mouth_open - before.mouth_open) < 0.03);
  advance(face, 1, 60, 0);
  assert(face._state.mimicry.mouth_open < 0.001);

  face.setAttribute('mode', 'speak');
  advance(face, 1, 60, 0.8);
  for (let i = 0; i < 60; i++) {
    face._state.phase += 1 / 60;
    face._tickIdle(1 / 60, face._state.speechUpdatedAt + 1000 + i * 1000 / 60);
  }
  assert(face._state.mimicry.mouth_open < 0.001, 'Stale audio must close the mouth');

  const idle = faceAtRest();
  advance(idle, 30, 60, 0);
  for (const [key, value] of Object.entries(idle._state.mimicry)) {
    if (key.startsWith('vis_') || key === 'mouth_open') assert.equal(value, 0);
  }
  idle._state.nextBlinkAt = idle._state.phase;
  let maxBlink = 0;
  for (let i = 0; i < 60; i++) {
    advance(idle, 1 / 120, 120, 0);
    maxBlink = Math.max(maxBlink, idle._state.mimicry.blink_left);
  }
  assert(maxBlink > 0.9 && idle._state.mimicry.blink_left < 0.001);
});
