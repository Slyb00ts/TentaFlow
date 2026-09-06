// ===== File: modules/tentaquant/run-stream.js — one run's live timeline =====
//
// A T1 run answers with its row and then TALKS: `RunSubscribe` opens a stream
// of `RunEvent{seq, kind}` frames — outputs, state keyframes, metrics and the
// final row — with a monotonic `seq` and a 512-frame replay buffer on the node
// that runs it (plan §11.2). Everything a screen shows while a run is in
// flight is folded out of those frames here, in one pure reducer, so the
// ordering rules can be tested without a socket:
//
//   * a frame whose `seq` is not newer than what we hold is a REPLAY, not new
//     work — `after_seq` resumption deliberately re-sends the tail — so it is
//     dropped rather than appended twice. The one exception is the synthetic
//     `done` frame a LATE subscriber gets for a run whose stream this process
//     no longer holds: it carries `seq: 0` and is still the answer.
//   * outputs are keyed by (cell, artifact seq) and keyframes by `step`, so a
//     replayed frame overwrites its own entry instead of duplicating a bar.
//   * `gap` is not a hole we can paper over. The server says it when our
//     cursor fell out of its buffer, and it is not repairable from here: a
//     `RunInfo` carries no stream cursor, so resubscribing with the cursor we
//     hold fails the very same comparison again. The session below therefore
//     gives the stream up and follows the ROW instead, saying so on screen,
//     instead of pretending the frames it never saw were empty.
//
// The session class drives that through the SCREEN (`screen.tqSubscribe`,
// `screen.tq`, `screen.onTransport`) rather than through `ApiBinary` directly,
// which is what keeps every view of this app testable with a fake screen.

import { COUNTS_MIME, STATE_MIME } from '/js/modules/tentaquant/quantum-view.js';

/// Statuses a run no longer moves out of.
const TERMINAL = new Set(['succeeded', 'failed', 'cancelled']);

/// Reasons `RunStreamEnd` carries that the browser acts on (`tentaquant/runs.rs`).
/// A `cancelled` end is not one of them: it arrives with the final `done` row,
/// so the panel reads the outcome off the run like any other finished run.
export const END_COMPLETED = 'completed';
export const END_GAP = 'gap';
export const END_NOT_FOUND = 'not_found';

/// How long the row poll waits between reads once a gap ended the stream.
/// A gap is `after_seq + 1 < oldest` on the node (`tentaquant/runs.rs`), and
/// nothing on the wire hands the browser a fresh cursor — resubscribing with
/// the one we hold would gap again, forever. So the run is followed by its row
/// until it reaches a terminal status, slowly: the row is the whole answer for
/// a finished run and there is no frame left to miss.
const GAP_POLL_MS = 3000;

export function runIsTerminal(status) {
  return TERMINAL.has(String(status || ''));
}

export function runStreamState(patch = {}) {
  return {
    /// Highest frame seq folded in; the cursor a resubscribe resumes from.
    seq: 0,
    outputs: [],
    keyframes: [],
    metrics: null,
    run: null,
    /// True once the server reported that frames were dropped: the evolution
    /// on screen is incomplete and says so.
    gap: false,
    end: '',
    error: '',
    ...patch,
  };
}

const outputKey = (output) => `${output.cellId ?? output.cell_id ?? ''}#${Number(output.seq) || 0}`;

/// Folds one `RunEvent` into the state. Pure: the caller keeps the result.
export function applyRunEvent(state, event) {
  if (!event || typeof event !== 'object') return state;
  const kind = String(event.kind || '');
  const seq = Number(event.seq) || 0;
  // A `done` frame with no seq is the synthetic answer for a run whose stream
  // is gone; every other frame with a stale seq is a replay we already hold.
  if (seq > 0 && seq <= state.seq) return state;
  if (seq === 0 && kind !== 'done') return state;
  const next = { ...state, seq: Math.max(state.seq, seq) };
  switch (kind) {
    case 'output': {
      const output = event.output;
      if (!output) break;
      const key = outputKey(output);
      const outputs = state.outputs.filter((o) => outputKey(o) !== key);
      outputs.push(output);
      outputs.sort((a, b) => (Number(a.seq) || 0) - (Number(b.seq) || 0));
      next.outputs = outputs;
      break;
    }
    case 'state_keyframe': {
      const keyframe = event.keyframe;
      if (!keyframe) break;
      const step = Number(keyframe.step) || 0;
      const keyframes = state.keyframes.filter((k) => (Number(k.step) || 0) !== step);
      keyframes.push(keyframe);
      keyframes.sort((a, b) => (Number(a.step) || 0) - (Number(b.step) || 0));
      next.keyframes = keyframes;
      break;
    }
    case 'metrics':
      if (event.metrics) next.metrics = event.metrics;
      break;
    case 'done':
      if (event.run) {
        next.run = event.run;
        if (event.run.metrics) next.metrics = event.run.metrics;
        if (Array.isArray(event.run.artifacts) && event.run.artifacts.length) {
          next.outputs = mergeOutputs(next.outputs, event.run.artifacts);
        }
      }
      break;
    default:
      // A kind this build does not know still advances the cursor: dropping it
      // would make a resubscribe replay it forever.
      break;
  }
  return next;
}

/// The stored outputs of a run merged over what the stream delivered. The row
/// is authoritative — an output the stream lost is in it — so it wins on a key
/// collision.
export function mergeOutputs(streamed, stored) {
  const byKey = new Map((streamed || []).map((o) => [outputKey(o), o]));
  for (const output of stored || []) byKey.set(outputKey(output), output);
  return Array.from(byKey.values()).sort((a, b) => (Number(a.seq) || 0) - (Number(b.seq) || 0));
}

export function applyRunEnd(state, reason) {
  return { ...state, end: String(reason || ''), gap: state.gap || reason === END_GAP };
}

// ---------------------------------------------------------------------------
// Selectors — what a panel draws out of the folded state
// ---------------------------------------------------------------------------

/// The newest output of one mime type, or null. A run writes at most one of
/// each, but a resumed stream can carry the same one twice.
export function outputOfMime(state, mime) {
  const matches = (state.outputs || []).filter((o) => String(o.mime) === mime);
  return matches.length ? matches[matches.length - 1] : null;
}

/// The mime bundle of one output, or null when the payload did not travel
/// inline (a large output is a content-store reference and is downloaded, not
/// drawn).
export function outputBundle(output) {
  if (!output) return null;
  const inline = output.inlineJson ?? output.inline_json;
  if (!inline) return null;
  try {
    return { [String(output.mime)]: JSON.parse(inline) };
  } catch {
    // A payload the server wrote and we cannot parse is a bug worth seeing as
    // "no output", never as a crash inside a repaint.
    return null;
  }
}

export function countsBundleOf(state) {
  return outputBundle(outputOfMime(state, COUNTS_MIME));
}

export function stateBundleOf(state) {
  return outputBundle(outputOfMime(state, STATE_MIME));
}

/// The Bloch row of one keyframe, flattened the way `paintBloch` and
/// `tf-bloch-sphere` both read it.
export function keyframeBloch(keyframe) {
  const flat = [];
  for (const vector of (keyframe && keyframe.bloch) || []) {
    const parts = Array.from(vector || []);
    flat.push(Number(parts[0]) || 0, Number(parts[1]) || 0, Number(parts[2]) || 0);
  }
  return flat;
}

/// A keyframe as the state card reads it: the spheres, their purity and the
/// heaviest amplitudes. `tf-mime-output` already understands the sparse `top`
/// list, so the keyframe travels almost verbatim (§13.6).
export function keyframeStateBundle(keyframe, numQubits) {
  if (!keyframe) return null;
  const bloch = (keyframe.bloch || []).map((v) => Array.from(v || [], Number));
  return {
    [STATE_MIME]: {
      numQubits: Number(numQubits) || bloch.length,
      bloch,
      purity: (keyframe.purity || []).map(Number),
      top: keyframe.top || [],
    },
  };
}

/// The step distribution a keyframe carries, as the histogram renderer reads
/// it. These are PROBABILITIES, not draws, so the bundle carries no shot
/// count — a footer saying "0 shots" would be a lie about a measured run.
export function keyframeProbsBundle(keyframe) {
  const rows = (keyframe && (keyframe.probsTop ?? keyframe.probs_top)) || [];
  if (!rows.length) return null;
  const counts = {};
  for (const row of rows) counts[String(row.bitstring)] = Number(row.probability) || 0;
  return { [COUNTS_MIME]: { counts } };
}

/// The gate a keyframe was taken after, as a one-line label.
export function keyframeGateLabel(keyframe) {
  const gate = keyframe && keyframe.gate;
  if (!gate) return '';
  const qubits = (gate.qubits || []).map((q) => `q${Number(q)}`).join(', ');
  return qubits ? `${gate.name} ${qubits}` : String(gate.name || '');
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

/// Drives one run's stream: subscribe, fold, resume.
///
/// It owns three recoveries, and each is a different fact:
///   * the SOCKET dropped — the server's subscription died with it, so the
///     next `open` resubscribes from the cursor we hold;
///   * the server reported a GAP — our cursor is older than its buffer and
///     nothing can move it forward, so the stream is abandoned and the row is
///     re-read on a slow timer until the run is terminal;
///   * the run FINISHED — nothing to resume, and the handle is released.
export class RunStream {
  constructor(screen, runId, { onUpdate = () => {}, onEnd = () => {} } = {}) {
    this.screen = screen;
    this.runId = runId;
    this.onUpdate = onUpdate;
    this.onEnd = onEnd;
    this.state = runStreamState();
    this.unsubscribe = null;
    this.unwatchTransport = null;
    this.gapTimer = 0;
    this.finished = false;
    this.stopped = false;
    // The socket went away while we were subscribed: the server-side
    // subscription went with it, so the next `open` has to open a new one.
    this.dropped = false;
  }

  async start() {
    this.unwatchTransport = this.screen.onTransport((event) => this.onTransportEvent(event));
    await this.subscribe();
  }

  stop() {
    this.stopped = true;
    if (this.gapTimer) clearTimeout(this.gapTimer);
    this.gapTimer = 0;
    this.release();
    if (this.unwatchTransport) this.unwatchTransport();
    this.unwatchTransport = null;
  }

  release() {
    const unsubscribe = this.unsubscribe;
    this.unsubscribe = null;
    if (unsubscribe) unsubscribe();
  }

  async subscribe() {
    if (this.stopped || this.finished) return;
    this.release();
    try {
      this.unsubscribe = await this.screen.tqSubscribe(
        'tentaQuantRunSubscribeRequest',
        { runId: this.runId, afterSeq: this.state.seq },
        {
          onChunk: (body) => this.chunk(body),
          onEnd: (body) => this.end(body),
          onError: (body) => this.fail(body),
        },
      );
      this.dropped = false;
      if (this.stopped) this.release();
    } catch (e) {
      this.fail(e);
    }
  }

  chunk(body) {
    if (this.stopped) return;
    const before = this.state;
    this.state = applyRunEvent(this.state, body && body.event);
    if (this.state !== before) this.onUpdate(this.state, body && body.event);
  }

  end(body) {
    if (this.stopped) return;
    const reason = String((body && body.reason) || END_COMPLETED);
    this.release();
    this.state = applyRunEnd(this.state, reason);
    if (reason === END_GAP) { this.onUpdate(this.state, null); this.pollRow(); return; }
    this.finished = true;
    this.onUpdate(this.state, null);
    this.onEnd(reason, this.state);
  }

  fail(error) {
    if (this.stopped) return;
    this.release();
    this.finished = true;
    this.state = { ...this.state, error: errorText(error) };
    this.onUpdate(this.state, null);
    this.onEnd(END_NOT_FOUND, this.state);
  }

  /// The row is the authority after a gap: it carries the outputs the stream
  /// dropped and says whether the run is still going. The stream itself is over
  /// — the cursor cannot be repaired from the browser — so a run still going is
  /// followed by re-reading that row until it stops, and the panel keeps the
  /// `gap` flag that tells the user the evolution on screen is incomplete.
  async pollRow() {
    if (this.stopped || this.finished) return;
    let run = null;
    try {
      const res = await this.screen.tq('tentaQuantRunGetRequest', { runId: this.runId });
      run = res && res.run;
    } catch (e) {
      this.fail(e);
      return;
    }
    if (this.stopped || this.finished) return;
    if (run) {
      const outputs = mergeOutputs(this.state.outputs, run.artifacts || []);
      // A repaint replaces the whole panel, so a poll that read the same row
      // again stays silent rather than dropping the reader's scroll position.
      const changed = run.status !== (this.state.run && this.state.run.status)
        || outputs.length !== this.state.outputs.length;
      this.state = {
        ...this.state,
        run,
        metrics: run.metrics || this.state.metrics,
        outputs,
      };
      if (changed) this.onUpdate(this.state, null);
    }
    if (run && runIsTerminal(run.status)) {
      this.finished = true;
      this.onEnd(END_COMPLETED, this.state);
      return;
    }
    // One poll in flight at a time: two overlapping timers would double the
    // read rate of a run that never terminates.
    if (this.gapTimer) clearTimeout(this.gapTimer);
    this.gapTimer = setTimeout(() => {
      this.gapTimer = 0;
      this.pollRow();
    }, GAP_POLL_MS);
  }

  onTransportEvent(event) {
    const type = event && event.type;
    if (type === 'disconnected' || type === 'close') {
      this.dropped = true;
      this.release();
      return;
    }
    // A session that already gapped holds a cursor the node rejects, so a
    // reconnect changes nothing for it: it keeps following the row instead.
    if (type === 'open' && this.dropped && !this.finished && !this.stopped && !this.state.gap) this.subscribe();
  }
}

function errorText(error) {
  if (!error) return '';
  if (typeof error === 'string') return error;
  return String(error.message || error.error || error.reason || '');
}
