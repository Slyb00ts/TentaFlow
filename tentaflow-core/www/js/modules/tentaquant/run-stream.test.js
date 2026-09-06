// =============================================================================
// File: modules/tentaquant/run-stream.test.js
// Description: The fold of one run's stream. Frame ordering is the whole point
// of `seq` + `after_seq` (plan §11.2), so the reducer is exercised the way a
// real subscription exercises it: frames out of order, frames replayed after a
// resume, a synthetic `done` with no seq, and the `gap` the server sends when
// the cursor fell out of its buffer. The session is driven against a fake
// screen so the three recoveries — socket, gap, finish — are checked without a
// socket.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  END_COMPLETED, END_GAP, END_NOT_FOUND, RunStream, applyRunEnd, applyRunEvent,
  countsBundleOf, keyframeBloch, keyframeGateLabel, keyframeProbsBundle, keyframeStateBundle,
  mergeOutputs, outputBundle, outputOfMime, runIsTerminal, runStreamState,
} = await import('./run-stream.js');
const { COUNTS_MIME, STATE_MIME } = await import('./quantum-view.js');

const counts = (seq, shots) => ({
  cellId: 'c1', seq, mime: COUNTS_MIME, sizeBytes: 40,
  sha256: null, inlineJson: JSON.stringify({ counts: { '00': shots / 2, '11': shots / 2 }, shots }),
});

const keyframe = (step, gate) => ({
  step,
  gate: gate ? { name: gate, qubits: [0], matrix: [] } : null,
  bloch: [[0, 0, 1], [0, 0, -1]],
  purity: [1, 0.5],
  pairs: [],
  top: [{ index: 0, amplitude: [0.7071, 0], partners: [] }],
  probsTop: [{ bitstring: '00', probability: 0.5 }, { bitstring: '11', probability: 0.5 }],
});

const frame = (seq, kind, extra = {}) => ({ seq, kind, ...extra });

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

test('frames fold in order and move the cursor a resubscribe resumes from', () => {
  let state = runStreamState();
  state = applyRunEvent(state, frame(1, 'state_keyframe', { keyframe: keyframe(1, 'H') }));
  state = applyRunEvent(state, frame(2, 'output', { output: counts(0, 1024) }));
  assert.equal(state.seq, 2);
  assert.equal(state.keyframes.length, 1);
  assert.equal(state.outputs.length, 1);
});

test('a replayed frame changes nothing — `after_seq` deliberately re-sends the tail', () => {
  let state = runStreamState();
  state = applyRunEvent(state, frame(3, 'output', { output: counts(0, 1024) }));
  const after = applyRunEvent(state, frame(3, 'output', { output: counts(0, 1024) }));
  assert.equal(after, state, 'the same state object, so nothing repainted');
  const older = applyRunEvent(state, frame(2, 'state_keyframe', { keyframe: keyframe(1, 'H') }));
  assert.equal(older, state, 'a frame older than the cursor is not new work');
});

test('keyframes are keyed by step, so a resumed stream never doubles the timeline', () => {
  let state = runStreamState();
  state = applyRunEvent(state, frame(1, 'state_keyframe', { keyframe: keyframe(2, 'Cx') }));
  state = applyRunEvent(state, frame(2, 'state_keyframe', { keyframe: keyframe(1, 'H') }));
  assert.deepEqual(state.keyframes.map((k) => k.step), [1, 2], 'sorted by step, not by arrival');
  state = applyRunEvent(state, frame(3, 'state_keyframe', { keyframe: keyframe(1, 'X') }));
  assert.equal(state.keyframes.length, 2, 'the same step replaces itself');
  assert.equal(state.keyframes[0].gate.name, 'X');
});

test('outputs are keyed by cell and artifact seq', () => {
  let state = runStreamState();
  state = applyRunEvent(state, frame(1, 'output', { output: counts(0, 512) }));
  state = applyRunEvent(state, frame(2, 'output', { output: { ...counts(0, 1024) } }));
  assert.equal(state.outputs.length, 1, 'the same output rewrote itself');
  assert.equal(JSON.parse(state.outputs[0].inlineJson).shots, 1024);
});

test('the synthetic `done` of a late subscriber carries no seq and is still the answer', () => {
  const state = applyRunEvent(runStreamState({ seq: 12 }), {
    seq: 0,
    kind: 'done',
    run: { runId: 'r1', status: 'succeeded', metrics: { durationMs: 42 }, artifacts: [counts(0, 1024)] },
  });
  assert.equal(state.run.status, 'succeeded');
  assert.equal(state.metrics.durationMs, 42);
  assert.equal(state.outputs.length, 1, 'the row brings the outputs the stream never sent');
  assert.equal(state.seq, 12, 'and it does not move the cursor backwards');
});

test('a frame kind this build does not know still advances the cursor', () => {
  const state = applyRunEvent(runStreamState(), frame(4, 'state_frame'));
  assert.equal(state.seq, 4, 'or a resubscribe would replay it forever');
});

test('a gap is remembered, because the timeline on screen is then incomplete', () => {
  const state = applyRunEnd(runStreamState({ seq: 9 }), END_GAP);
  assert.equal(state.gap, true);
  assert.equal(state.end, END_GAP);
  assert.equal(applyRunEnd(runStreamState(), END_COMPLETED).gap, false);
});

test('the stored row wins over the stream when the two carry the same output', () => {
  const merged = mergeOutputs([counts(0, 512)], [counts(0, 1024)]);
  assert.equal(merged.length, 1);
  assert.equal(JSON.parse(merged[0].inlineJson).shots, 1024);
});

test('terminal statuses are the three a run stops at', () => {
  assert.deepEqual(
    ['created', 'queued', 'running', 'succeeded', 'failed', 'cancelled'].map(runIsTerminal),
    [false, false, false, true, true, true],
  );
});

// ---------------------------------------------------------------------------
// Selectors
// ---------------------------------------------------------------------------

test('an inline output becomes the mime bundle its renderer reads', () => {
  let state = runStreamState();
  state = applyRunEvent(state, frame(1, 'output', { output: counts(0, 1024) }));
  const bundle = countsBundleOf(state);
  assert.equal(bundle[COUNTS_MIME].shots, 1024);
  assert.equal(outputOfMime(state, STATE_MIME), null);
});

test('an output that stayed in the content store draws nothing here', () => {
  const stored = { cellId: 'c1', seq: 1, mime: STATE_MIME, sizeBytes: 9e6, sha256: 'a'.repeat(64), inlineJson: null };
  assert.equal(outputBundle(stored), null, 'it is a download, not a picture');
  assert.equal(outputBundle(null), null);
  assert.equal(outputBundle({ mime: COUNTS_MIME, inlineJson: '{oops' }), null, 'unparsable is "no output", never a throw');
});

test('a keyframe becomes exactly what the state panel draws', () => {
  const bundle = keyframeStateBundle(keyframe(3, 'Cx'), 2);
  assert.deepEqual(bundle[STATE_MIME].bloch, [[0, 0, 1], [0, 0, -1]]);
  assert.deepEqual(bundle[STATE_MIME].purity, [1, 0.5]);
  assert.equal(bundle[STATE_MIME].numQubits, 2);
  // The sparse `top` list travels verbatim: tf-mime-output reads that shape.
  assert.equal(bundle[STATE_MIME].top.length, 1);
  assert.deepEqual(keyframeBloch(keyframe(1, 'H')), [0, 0, 1, 0, 0, -1]);
  assert.equal(keyframeGateLabel(keyframe(1, 'H')), 'H q0');
  assert.equal(keyframeGateLabel(keyframe(0, null)), '');
});

test('the step distribution of a keyframe carries no shot count', () => {
  const bundle = keyframeProbsBundle(keyframe(1, 'H'));
  assert.deepEqual(bundle[COUNTS_MIME].counts, { '00': 0.5, '11': 0.5 });
  assert.equal(bundle[COUNTS_MIME].shots, undefined, 'these are probabilities, not draws');
  assert.equal(keyframeProbsBundle({ step: 0, probsTop: [] }), null);
});

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

function fakeScreen(over = {}) {
  const screen = {
    subscriptions: [],
    requests: [],
    transport: null,
    unsubscribed: 0,
    run: { runId: 'r1', status: 'running', artifacts: [] },
    tqSubscribe(kind, payload, handlers) {
      this.subscriptions.push({ kind, payload, handlers });
      return Promise.resolve(() => { this.unsubscribed += 1; });
    },
    tq(kind, payload) {
      this.requests.push([kind, payload]);
      return Promise.resolve({ run: this.run });
    },
    onTransport(cb) { this.transport = cb; return () => { this.transport = null; }; },
    ...over,
  };
  return screen;
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

test('a subscription resumes from the cursor it folded', async () => {
  const screen = fakeScreen();
  const stream = new RunStream(screen, 'r1', {});
  await stream.start();
  assert.equal(screen.subscriptions[0].payload.afterSeq, 0);
  screen.subscriptions[0].handlers.onChunk({ event: frame(7, 'output', { output: counts(0, 1024) }) });
  assert.equal(stream.state.seq, 7);

  // The socket died: the server-side subscription went with it, so the next
  // `open` opens a NEW one — from the cursor, not from the beginning.
  screen.transport({ type: 'disconnected' });
  assert.equal(screen.unsubscribed, 1);
  screen.transport({ type: 'open' });
  await settle();
  assert.equal(screen.subscriptions.length, 2);
  assert.equal(screen.subscriptions[1].payload.afterSeq, 7);
  stream.stop();
});

test('a finished run ends the session and is not resubscribed', async () => {
  const ends = [];
  const screen = fakeScreen();
  const stream = new RunStream(screen, 'r1', { onEnd: (reason) => ends.push(reason) });
  await stream.start();
  // 'cancelled' is a reason the session passes on untouched: the outcome is
  // read off the final row, not off the end of the stream.
  screen.subscriptions[0].handlers.onEnd({ reason: 'cancelled' });
  assert.deepEqual(ends, ['cancelled']);
  screen.transport({ type: 'disconnected' });
  screen.transport({ type: 'open' });
  await settle();
  assert.equal(screen.subscriptions.length, 1, 'a run that ended has nothing to resume');
  stream.stop();
});

test('a gap re-reads the row, and a run that already finished needs no new stream', async () => {
  const ends = [];
  const screen = fakeScreen({ run: { runId: 'r1', status: 'succeeded', artifacts: [counts(0, 1024)] } });
  const stream = new RunStream(screen, 'r1', { onEnd: (reason) => ends.push(reason) });
  await stream.start();
  screen.subscriptions[0].handlers.onEnd({ reason: END_GAP });
  await settle();
  assert.deepEqual(screen.requests.map(([kind]) => kind), ['tentaQuantRunGetRequest']);
  assert.equal(stream.state.gap, true, 'the view has to say the evolution is incomplete');
  assert.equal(stream.state.outputs.length, 1, 'the row carried what the stream dropped');
  assert.deepEqual(ends, [END_COMPLETED]);
  assert.equal(screen.subscriptions.length, 1);
  assert.equal(stream.gapTimer, 0, 'nothing is armed for a run that is over');
  stream.stop();
});

test('a gap on a run still going follows the row and never resubscribes', async () => {
  const updates = [];
  const screen = fakeScreen();
  const stream = new RunStream(screen, 'r1', { onUpdate: () => updates.push(1) });
  await stream.start();
  screen.subscriptions[0].handlers.onChunk({ event: frame(600, 'metrics', { metrics: { qubits: 4 } }) });
  screen.subscriptions[0].handlers.onEnd({ reason: END_GAP });
  await settle();
  assert.equal(stream.finished, false);
  // Resubscribing is what MUST NOT happen: the cursor we hold is the one the
  // node already called a gap, so the same request would gap again forever.
  assert.equal(screen.subscriptions.length, 1, 'the stream is given up, not retried');
  assert.equal(stream.state.gap, true);
  assert.notEqual(stream.gapTimer, 0, 'the row is re-read on a timer instead');

  // Nor does a reconnect resurrect the stream: the cursor is the same one the
  // node called a gap.
  screen.transport({ type: 'disconnected' });
  screen.transport({ type: 'open' });
  await settle();
  assert.equal(screen.subscriptions.length, 1, 'a reconnect cannot repair the cursor either');

  // A poll that reads the same row again repaints nothing — a repaint replaces
  // the whole panel under the reader.
  const quiet = updates.length;
  await stream.pollRow();
  assert.equal(updates.length, quiet, 'an unchanged row is not news');
  assert.equal(screen.requests.length, 2, 'both polls read the row, nothing else');

  // And the poll ends the session the moment the row says the run is over.
  screen.run = { runId: 'r1', status: 'succeeded', artifacts: [counts(0, 512)] };
  const ends = [];
  stream.onEnd = (reason) => ends.push(reason);
  await stream.pollRow();
  assert.deepEqual(ends, [END_COMPLETED]);
  assert.equal(stream.finished, true);
  assert.equal(stream.state.outputs.length, 1, 'the row carried what the stream dropped');
  stream.stop();
  assert.equal(stream.gapTimer, 0, 'and stopping the session disarms the timer');
});

test('a stream error ends the session with the reason on the state', async () => {
  const ends = [];
  const screen = fakeScreen();
  const stream = new RunStream(screen, 'r1', { onEnd: (reason) => ends.push(reason) });
  await stream.start();
  screen.subscriptions[0].handlers.onError({ message: 'run not found' });
  assert.equal(stream.state.error, 'run not found');
  assert.deepEqual(ends, [END_NOT_FOUND]);
  stream.stop();
});
