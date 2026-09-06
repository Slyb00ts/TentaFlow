// ===== File: modules/tentaquant/explain.js — the "Wyjaśnij" sentences of §13.6 =====
//
// Every state view of the run view (Q15) carries a "Wyjaśnij" toggle, and the
// sentence behind it is generated DETERMINISTICALLY from the features of the
// state — superposition, relative phase, entanglement out of purity and
// concurrence, the heaviest basis states, and what the LAST gate changed. No
// model is called: §13.6 draws the line explicitly, and the assistant of §12.4
// is a separate, explicit action.
//
// The functions here answer DESCRIPTORS (`{key, params}`), never finished text.
// Three things follow from that: the choice of sentence is testable without a
// locale, the same feature set renders in all five languages, and the plural
// forms go through the i18n selector instead of being pasted together here
// (rule 8). `explainText` is the one place a descriptor list becomes a string.
//
// Everything is pure — a frame in, sentences out — so the state panel, the
// evolution animation and a test all get the same words for the same state.

import { T } from '/js/modules/tentaquant/format.js';
import { vectorLength } from '/js/components/tf-bloch-sphere.js';

/// Below this purity a qubit is not in a state of its own any more: it is
/// entangled with something else. The same threshold `tf-bloch-sphere` draws
/// its "splątany" chip at, so the sentence and the picture never disagree.
export const ENTANGLED_PURITY = 0.99;

/// Concurrence under which a pair's correlation is not called entanglement.
/// Two qubits can be correlated with concurrence 0 (a classical mixture), and
/// the sentence has to be able to say so.
export const MIN_CONCURRENCE = 0.02;

/// A Bloch vector this far from a pole is a superposition worth naming; closer
/// than that the qubit is "basically |0⟩" and saying otherwise is noise.
export const MIN_EQUATOR = 0.12;

/// Probability under which a basis state is not mentioned at all.
export const MIN_PROBABILITY = 5e-3;

/// Phase difference (in units of π) under which two amplitudes are called
/// phase-aligned.
export const MIN_PHASE = 0.02;

const round = (value, digits = 2) => Number(Number(value).toFixed(digits));

// ---------------------------------------------------------------------------
// Features
// ---------------------------------------------------------------------------

/// Everything the templates below choose between, read off ONE frame as
/// `keyframe-math` produces it. Nothing is inferred that the frame does not
/// carry: a frame without pair matrices reports entanglement from purity only,
/// and says nothing about concurrence.
export function stateFeatures(frame) {
  const bloch = (frame && frame.bloch) || [];
  const purity = (frame && frame.purity) || [];
  const qubits = bloch.map((vector, index) => {
    const length = vectorLength(vector);
    const value = Number(purity[index]);
    return {
      qubit: index,
      vector,
      length,
      purity: Number.isFinite(value) ? value : (1 + length * length) / 2,
      // A qubit whose vector is off both poles is in a superposition of |0⟩
      // and |1⟩ — and only a PURE one is, a mixed vector is short for a
      // different reason entirely.
      superposed: length > MIN_EQUATOR && Math.abs(Number(vector[2]) || 0) < 1 - MIN_EQUATOR,
    };
  });
  const pairs = ((frame && frame.pairs) || []).map((pair) => {
    const list = Array.from(pair.qubits || [], Number);
    return {
      a: list[0],
      b: list[1],
      concurrence: Number(pair.concurrence ?? 0) || 0,
      mutualInformation: Number(pair.mutualInformation ?? pair.mutual_information ?? 0) || 0,
    };
  });
  const amplitudes = [];
  for (const [index, value] of (frame && frame.amplitudes) || []) {
    const probability = value[0] * value[0] + value[1] * value[1];
    if (probability < MIN_PROBABILITY) continue;
    amplitudes.push({
      index,
      probability,
      phase: Math.atan2(value[1], value[0]),
      key: bitLabel(index, bloch.length),
    });
  }
  amplitudes.sort((x, y) => y.probability - x.probability || x.index - y.index);
  return {
    numQubits: bloch.length,
    qubits,
    pairs,
    amplitudes,
    entangled: qubits.filter((q) => q.purity < ENTANGLED_PURITY).map((q) => q.qubit),
    entangledPairs: pairs.filter((p) => p.concurrence > MIN_CONCURRENCE),
    collapsing: Boolean(frame && frame.collapsing),
    gate: (frame && frame.gate) || null,
  };
}

function bitLabel(index, numQubits) {
  const width = Math.max(1, Number(numQubits) || 1);
  let out = '';
  for (let bit = width - 1; bit >= 0; bit -= 1) out += ((Number(index) >> bit) & 1) ? '1' : '0';
  return out;
}

const qubitList = (list) => list.map((q) => `q${q}`).join(', ');

// ---------------------------------------------------------------------------
// Sentences
// ---------------------------------------------------------------------------

/// What this state IS, in at most four sentences, ordered from the fact a
/// beginner reads first (where the probability sits) to the one that needs the
/// picture (the relative phase).
export function explainState(frame) {
  const features = stateFeatures(frame);
  if (!features.numQubits) return [];
  const out = [];
  const [top, second] = features.amplitudes;

  if (features.amplitudes.length === 1 && top) {
    out.push({ key: 'explain.single', params: { state: top.key } });
  } else if (top) {
    out.push({
      key: 'explain.spread',
      params: { n: features.amplitudes.length, state: top.key, p: round(top.probability) },
    });
  }

  const superposed = features.qubits.filter((q) => q.superposed && q.purity >= ENTANGLED_PURITY);
  if (superposed.length) {
    out.push({
      key: 'explain.superposition',
      params: { n: superposed.length, qubits: qubitList(superposed.map((q) => q.qubit)) },
    });
  }

  if (features.entangledPairs.length) {
    const strongest = features.entangledPairs
      .slice().sort((x, y) => y.concurrence - x.concurrence)[0];
    out.push({
      key: 'explain.entangled_pair',
      params: { a: `q${strongest.a}`, b: `q${strongest.b}`, c: round(strongest.concurrence) },
    });
  } else if (features.entangled.length >= 2) {
    // Purity says the qubits are not on their own, but this frame carries no
    // pair matrix to name the partner — so the sentence says exactly that much.
    out.push({
      key: 'explain.entangled_purity',
      params: { n: features.entangled.length, qubits: qubitList(features.entangled) },
    });
  }

  if (top && second) {
    const delta = Math.abs(top.phase - second.phase) / Math.PI;
    if (delta > MIN_PHASE) {
      out.push({
        key: 'explain.relative_phase',
        params: { a: top.key, b: second.key, phase: round(delta) },
      });
    }
  }
  return out;
}

/// What the last gate CHANGED, by comparing the frame with the one before it.
/// The plan asks for a highlight, not a diff dump, so this answers one leading
/// sentence and at most two changes — the ones the eye can find in the picture.
export function explainGate(frame, previous) {
  const gate = frame && frame.gate;
  if (!gate) return [];
  const name = String(gate.name || '').toUpperCase();
  const qubits = qubitList(Array.from(gate.qubits || [], Number));
  if (frame.collapsing) return [{ key: 'explain.gate_measure', params: { qubits } }];
  // At fraction 0 the playhead sits BEFORE this gate: it has not run, so it
  // has changed nothing — and saying "it changed nothing" would read as a
  // verdict on a gate that is still to come.
  if (Number(frame.fraction) <= 1e-9) return [{ key: 'explain.gate_pending', params: { gate: name, qubits } }];
  const changes = gateChanges(frame, previous);
  if (!changes.length) return [{ key: 'explain.gate_plain', params: { gate: name, qubits } }];
  return [{ key: 'explain.gate_changed', params: { gate: name, qubits } }, ...changes];
}

/// The two differences worth pointing at: the qubit whose Bloch vector moved
/// the most, and the basis state that gained the most probability.
export function gateChanges(frame, previous) {
  if (!previous) return [];
  const out = [];
  const before = stateFeatures(previous);
  const after = stateFeatures(frame);
  let moved = null;
  for (const qubit of after.qubits) {
    const was = before.qubits[qubit.qubit];
    if (!was) continue;
    const distance = Math.hypot(
      (qubit.vector[0] || 0) - (was.vector[0] || 0),
      (qubit.vector[1] || 0) - (was.vector[1] || 0),
      (qubit.vector[2] || 0) - (was.vector[2] || 0),
    );
    if (!moved || distance > moved.distance) moved = { qubit, was, distance };
  }
  if (moved && moved.distance > 0.05) {
    if (moved.was.purity >= ENTANGLED_PURITY && moved.qubit.purity < ENTANGLED_PURITY) {
      out.push({ key: 'explain.change_entangled', params: { qubit: `q${moved.qubit.qubit}` } });
    } else if (!moved.was.superposed && moved.qubit.superposed) {
      out.push({ key: 'explain.change_superposed', params: { qubit: `q${moved.qubit.qubit}` } });
    } else {
      out.push({ key: 'explain.change_turned', params: { qubit: `q${moved.qubit.qubit}` } });
    }
  }
  const wasProbability = new Map(before.amplitudes.map((a) => [a.index, a.probability]));
  let gained = null;
  for (const amplitude of after.amplitudes) {
    const delta = amplitude.probability - (wasProbability.get(amplitude.index) || 0);
    if (!gained || delta > gained.delta) gained = { amplitude, delta };
  }
  if (gained && gained.delta > 0.05) {
    out.push({ key: 'explain.change_amplitude', params: { state: gained.amplitude.key, p: round(gained.amplitude.probability) } });
  }
  return out;
}

/// The histogram's own sentence: how many shots landed where, and how far the
/// measured distribution is from the exact one the same run computed.
export function explainHistogram({ shots = 0, topState = '', topCount = 0, tvd = null, fidelity = null } = {}) {
  const out = [];
  if (shots && topState) {
    out.push({ key: 'explain.hist_top', params: { state: topState, n: topCount, shots } });
  }
  if (tvd !== null && Number.isFinite(tvd)) {
    out.push({
      key: tvd < 0.02 ? 'explain.hist_close' : 'explain.hist_far',
      params: { tvd: round(tvd, 3), fidelity: round(Number(fidelity) || 0, 3) },
    });
  }
  return out;
}

/// Descriptors → one paragraph. `translate` defaults to the app's own i18n, so
/// a caller passes one only in a test.
export function explainText(sentences, translate = T) {
  return (sentences || []).map(({ key, params }) => translate(key, params)).join(' ');
}
