// =============================================================================
// File: modules/vision-detections-overlay.pose-sync.test.js
// Purpose: unit tests of how vision-detections-overlay.js picks the gesture-engine
//          (pose) frame for the picture on screen. The methods belong to a class
//          that needs the DOM, so — like the OCR votes test — their source is cut
//          out of the real file and evaluated on a stand-in `this`.
// =============================================================================

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import test from 'node:test';
import assert from 'node:assert/strict';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'vision-detections-overlay.js'), 'utf8');

function extractMethod(src, name) {
  const marker = `\n  ${name}(`;
  const start = src.indexOf(marker);
  if (start < 0) throw new Error(`method not found: ${name}`);
  const bodyStart = src.indexOf('{', start);
  let depth = 0;
  for (let i = bodyStart; i < src.length; i++) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) return src.slice(start + 1, i + 1);
    }
  }
  throw new Error(`unbalanced braces in method: ${name}`);
}

function extractConst(src, name) {
  const m = new RegExp(`const\\s+${name}\\s*=\\s*([^;]+);`).exec(src);
  if (!m) throw new Error(`const not found: ${name}`);
  return Number(eval(m[1])); // eslint-disable-line no-eval
}

const POSE_BUFFER_MS = extractConst(source, 'POSE_BUFFER_MS');
const MAX_WIEK_RAMKI_MS = extractConst(source, 'MAX_WIEK_RAMKI_MS');
const EPS_PRZOD_MS = extractConst(source, 'EPS_PRZOD_MS');
const SYNC_OFFSET_MS = extractConst(source, 'SYNC_OFFSET_MS');

const methodNames = [
  'pushPoseFrame',
  'selectPoseFrame',
  'selectFrame',
  'wybierzNajlepszaRamke',
  'frameCaptureMs',
];
const factory = new Function(
  'POSE_BUFFER_MS',
  'MAX_WIEK_RAMKI_MS',
  'epsPrzodMs',
  'syncOffsetMs',
  `return { ${methodNames.map((n) => extractMethod(source, n)).join(', ')} };`,
);
const methods = factory(
  POSE_BUFFER_MS,
  MAX_WIEK_RAMKI_MS,
  () => EPS_PRZOD_MS,
  () => SYNC_OFFSET_MS,
);

/** Overlay stand-in with a video at `currentTime` and an optional PTS base. */
function makeOverlay({ base = null, video = null, captureTarget = null } = {}) {
  return {
    poseFrames: [],
    frames: [],
    mediaBasePtsNs: () => base,
    videoEl: () => video,
    targetCaptureWallMs: () => captureTarget,
    ...methods,
  };
}

const BASE_NS = 96_000_000_000_000;
const frameAt = (mediaMs, extra = {}) => ({
  at: 0,
  tsMs: 1_700_000_000_000 + mediaMs,
  ptsNs: BASE_NS + mediaMs * 1e6,
  items: [{ klasa: 'hand', mediaMs }],
  ...extra,
});

test('with a PTS axis the frame of the picture on screen wins over the newest', () => {
  const o = makeOverlay({ base: BASE_NS, video: { currentTime: 10.04 } });
  for (const ms of [9_967, 10_000, 10_033, 10_066, 10_100]) o.pushPoseFrame(frameAt(ms));
  const sel = o.selectPoseFrame();
  // The playhead is at 10 040 ms: the analysis of the 10 033 ms frame is drawn,
  // not the newer 10 066 / 10 100 ones the video has not reached yet (beyond the
  // EPS_PRZOD_MS look-ahead).
  assert.ok(10_066 - 10_040 > EPS_PRZOD_MS);
  assert.equal(sel.frame.items[0].mediaMs, 10_033);
  assert.equal(sel.ageMs, 7);
});

test('nothing is drawn when every analysis is older than the age limit', () => {
  const o = makeOverlay({ base: BASE_NS, video: { currentTime: 20 } });
  o.pushPoseFrame(frameAt(10_000));
  assert.equal(o.selectPoseFrame(), null);
});

test('without a PTS axis the capture wall-clock picks the frame', () => {
  const o = makeOverlay({ captureTarget: 1_700_000_010_040 });
  for (const ms of [10_000, 10_033, 10_066]) o.pushPoseFrame(frameAt(ms, { ptsNs: null }));
  const sel = o.selectPoseFrame();
  assert.equal(sel.frame.items[0].mediaMs, 10_033);
  assert.equal(sel.ageMs, 7);
});

test('without any playback timing the newest frame is drawn', () => {
  const o = makeOverlay();
  o.pushPoseFrame(frameAt(10_000, { ptsNs: null }));
  o.pushPoseFrame(frameAt(10_033, { ptsNs: null }));
  assert.equal(o.selectPoseFrame().frame.items[0].mediaMs, 10_033);
});

test('the pose buffer keeps only the last few seconds', () => {
  const o = makeOverlay();
  for (let ms = 0; ms <= 10_000; ms += 100) o.pushPoseFrame(frameAt(ms));
  const span = o.poseFrames.at(-1).tsMs - o.poseFrames[0].tsMs;
  assert.ok(span <= POSE_BUFFER_MS, `span ${span} ms`);
  assert.equal(o.poseFrames.at(-1).items[0].mediaMs, 10_000);
});
