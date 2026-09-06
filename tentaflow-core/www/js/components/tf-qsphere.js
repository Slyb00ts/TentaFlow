// =============================================================================
// File: components/tf-qsphere.js
// Description: <tf-qsphere> — the Q-sphere of plan §13.6 (mockup Q15 "Stan"):
//              every basis state of the register is ONE point on a sphere,
//              placed by its Hamming weight (|0…0⟩ at the north pole, |1…1⟩ at
//              the south), sized by its probability and coloured by its phase.
//
//              It answers a question the amplitude bars cannot: which states
//              carry weight, and how their phases relate. A bar chart of 2ⁿ
//              columns is unreadable past four qubits; the sphere stays legible
//              because the empty states simply are not drawn.
//
//              Rendered as SVG rather than on a canvas — unlike a Bloch sphere
//              this picture has one MARK PER STATE, and a mark is a thing a
//              pointer hovers, a title describes and a screen reader reaches.
//              Drag to orbit; the projection is `tf-bloch-sphere`'s own camera,
//              so the two pictures on the same screen turn the same way.
//
//  Properties: state — {amplitudes|top, numQubits}, labels — i18n dict.
//  Attributes: size (px, default 180), max-states (default 64), yaw, pitch.
//  Events    : "orbit" detail {yaw, pitch} while the user turns the sphere.
//
// Example: qs.state = { top: frame.top, numQubits: 4 };
// =============================================================================

import { amplitudeRows, phaseColor } from './tf-mime-output.js';
import { projectVector } from './tf-bloch-sphere.js';

const SVG_NS = 'http://www.w3.org/2000/svg';
const DEFAULT_SIZE = 180;
const DEFAULT_MAX_STATES = 64;
const DEFAULT_YAW = -0.45;
const DEFAULT_PITCH = 0.32;

const DEFAULT_LABELS = {
  qsphere: 'Q-sphere',
  empty: 'no amplitudes',
  north: 'weight 0',
  south: 'all ones',
  probability: 'probability',
  phase: 'phase',
  more: '+{n} more states',
};

// ---------------------------------------------------------------------------
// Layout — pure
// ---------------------------------------------------------------------------

export function hammingWeight(index) {
  let n = Math.max(0, Number(index) || 0);
  let bits = 0;
  while (n) { bits += n & 1; n >>>= 1; }
  return bits;
}

/// Where one basis state sits on the sphere. Latitude is the Hamming weight
/// (z runs +1 → −1 as the weight runs 0 → n) and longitude spreads the states
/// of one weight evenly around that circle of latitude, in index order, so the
/// picture is a deterministic function of the register and not of the order
/// the amplitudes happened to arrive in.
export function qspherePoint(index, numQubits, rank, ringSize) {
  const n = Math.max(1, Number(numQubits) || 1);
  const weight = hammingWeight(index);
  const z = 1 - (2 * weight) / n;
  const radius = Math.sqrt(Math.max(0, 1 - z * z));
  // A pole holds exactly one state, so it needs no angle at all.
  const angle = ringSize > 1 ? (2 * Math.PI * rank) / ringSize : 0;
  return [radius * Math.cos(angle), radius * Math.sin(angle), z];
}

/// Every drawn state, heaviest first and capped: `{index, key, weight, vector,
/// probability, magnitude, phase}`. States of equal weight keep index order
/// around their ring, which is what makes the ring stable while an animation
/// changes the amplitudes on it.
export function qsphereLayout(state, limit = DEFAULT_MAX_STATES) {
  const numQubits = Math.max(1, Number(state && state.numQubits) || 1);
  const rows = amplitudeRows(state || {}, numQubits);
  const max = Math.max(1, Number(limit) || DEFAULT_MAX_STATES);
  const kept = rows.slice(0, max);
  // The ring a state lands on is decided by the WHOLE register, not by the
  // subset that survived the cap: dropping a faint state must not rotate the
  // heavy ones next to it.
  const rings = new Map();
  for (const row of rows) {
    const weight = hammingWeight(row.index);
    if (!rings.has(weight)) rings.set(weight, []);
    rings.get(weight).push(row.index);
  }
  for (const list of rings.values()) list.sort((a, b) => a - b);
  const points = kept.map((row) => {
    const weight = hammingWeight(row.index);
    const ring = rings.get(weight) || [row.index];
    return {
      index: row.index,
      key: row.key,
      weight,
      probability: row.probability,
      magnitude: row.magnitude,
      phase: row.phase,
      vector: qspherePoint(row.index, numQubits, ring.indexOf(row.index), ring.length),
    };
  });
  return { numQubits, points, hidden: Math.max(0, rows.length - kept.length) };
}

/// Radius of a state's mark in pixels. Probability maps to AREA, not to the
/// radius: doubling a probability has to look like twice as much ink.
export function markRadius(probability, size) {
  const p = Math.max(0, Math.min(1, Number(probability) || 0));
  const biggest = Math.max(3, size * 0.055);
  return Math.max(1.5, biggest * Math.sqrt(p));
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfQsphere extends HTMLElement {
  static get observedAttributes() {
    return ['size', 'max-states', 'yaw', 'pitch'];
  }

  constructor() {
    super();
    this._state = null;
    this._labels = { ...DEFAULT_LABELS };
    this._built = false;
    this._pointer = null;
    this._onPointerDown = this._onPointerDown.bind(this);
    this._onPointerMove = this._onPointerMove.bind(this);
    this._onPointerUp = this._onPointerUp.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._render();
  }

  disconnectedCallback() {
    this._onPointerUp();
  }

  attributeChangedCallback() {
    if (this._built) this._render();
  }

  get state() { return this._state; }

  set state(value) {
    this._state = value || null;
    if (!this._built) this._build();
    this._render();
  }

  get labels() { return { ...this._labels }; }

  set labels(value) {
    this._labels = { ...DEFAULT_LABELS, ...(value || {}) };
    if (this._built) this._render();
  }

  _build() {
    this._built = true;
    this.classList.add('tf-qsphere');
    this._svg = document.createElementNS(SVG_NS, 'svg');
    this._svg.setAttribute('class', 'tf-qsphere__svg');
    this._svg.setAttribute('role', 'img');
    this._svg.setAttribute('tabindex', '0');
    this._svg.addEventListener('pointerdown', this._onPointerDown);
    this._svg.addEventListener('keydown', this._onKeyDown);
    this._foot = document.createElement('div');
    this._foot.className = 'tf-qsphere__foot';
    this.replaceChildren(this._svg, this._foot);
  }

  _size() {
    const value = Number(this.getAttribute('size'));
    return Number.isFinite(value) && value > 60 ? value : DEFAULT_SIZE;
  }

  _angle(name, fallback) {
    const value = Number(this.getAttribute(name));
    return Number.isFinite(value) ? value : fallback;
  }

  _orbit(dYaw, dPitch) {
    const yaw = this._angle('yaw', DEFAULT_YAW) + dYaw;
    const pitch = Math.max(-1.4, Math.min(1.4, this._angle('pitch', DEFAULT_PITCH) + dPitch));
    this.setAttribute('yaw', yaw.toFixed(4));
    this.setAttribute('pitch', pitch.toFixed(4));
    this.dispatchEvent(new CustomEvent('orbit', { detail: { yaw, pitch } }));
  }

  _onPointerDown(event) {
    this._pointer = { x: event.clientX, y: event.clientY };
    if (typeof window === 'undefined') return;
    window.addEventListener('pointermove', this._onPointerMove);
    window.addEventListener('pointerup', this._onPointerUp);
  }

  _onPointerMove(event) {
    if (!this._pointer) return;
    this._orbit((event.clientX - this._pointer.x) * 0.01, (event.clientY - this._pointer.y) * 0.01);
    this._pointer = { x: event.clientX, y: event.clientY };
  }

  _onPointerUp() {
    this._pointer = null;
    if (typeof window === 'undefined') return;
    window.removeEventListener('pointermove', this._onPointerMove);
    window.removeEventListener('pointerup', this._onPointerUp);
  }

  _onKeyDown(event) {
    const step = 0.18;
    const moves = {
      ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step],
    };
    const move = moves[event.key];
    if (!move) return;
    event.preventDefault();
    this._orbit(move[0], move[1]);
  }

  _render() {
    const size = this._size();
    const half = size / 2;
    const radius = half - 12;
    const yaw = this._angle('yaw', DEFAULT_YAW);
    const pitch = this._angle('pitch', DEFAULT_PITCH);
    const layout = qsphereLayout(this._state || {}, Number(this.getAttribute('max-states')) || DEFAULT_MAX_STATES);
    this._svg.setAttribute('width', String(size));
    this._svg.setAttribute('height', String(size));
    this._svg.setAttribute('viewBox', `0 0 ${size} ${size}`);
    this._svg.replaceChildren();

    const body = document.createElementNS(SVG_NS, 'circle');
    body.setAttribute('class', 'tf-qsphere__body');
    body.setAttribute('cx', String(half));
    body.setAttribute('cy', String(half));
    body.setAttribute('r', String(radius));
    this._svg.appendChild(body);
    this._svg.appendChild(this._ring(half, radius, yaw, pitch));
    // The two poles are what makes the latitude readable: |0…0⟩ at the top and
    // the all-ones state at the bottom, whatever the camera is doing.
    for (const [vector, text] of [[[0, 0, 1], this._labels.north], [[0, 0, -1], this._labels.south]]) {
      const projected = projectVector(vector, yaw, pitch, radius);
      const pole = document.createElementNS(SVG_NS, 'text');
      pole.setAttribute('class', 'tf-qsphere__pole');
      pole.setAttribute('x', (half + projected.x).toFixed(2));
      pole.setAttribute('y', (half + projected.y + (vector[2] > 0 ? -8 : 14)).toFixed(2));
      pole.setAttribute('text-anchor', 'middle');
      pole.textContent = text;
      this._svg.appendChild(pole);
    }

    if (!layout.points.length) {
      this._foot.textContent = this._labels.empty;
      this._svg.setAttribute('aria-label', `${this._labels.qsphere}: ${this._labels.empty}`);
      return;
    }

    // Painter's order: the far hemisphere first, so a heavy state in front is
    // never hidden behind a faint one behind it.
    const marks = layout.points
      .map((point) => ({ point, projected: projectVector(point.vector, yaw, pitch, radius) }))
      .sort((a, b) => a.projected.depth - b.projected.depth);
    for (const { point, projected } of marks) {
      const x = half + projected.x;
      const y = half + projected.y;
      const spoke = document.createElementNS(SVG_NS, 'line');
      spoke.setAttribute('class', 'tf-qsphere__spoke');
      spoke.setAttribute('x1', String(half));
      spoke.setAttribute('y1', String(half));
      spoke.setAttribute('x2', x.toFixed(2));
      spoke.setAttribute('y2', y.toFixed(2));
      spoke.setAttribute('stroke', phaseColor(point.phase));
      this._svg.appendChild(spoke);
      const dot = document.createElementNS(SVG_NS, 'circle');
      dot.setAttribute('class', `tf-qsphere__dot${projected.depth < 0 ? ' is-back' : ''}`);
      dot.setAttribute('cx', x.toFixed(2));
      dot.setAttribute('cy', y.toFixed(2));
      dot.setAttribute('r', markRadius(point.probability, size).toFixed(2));
      dot.setAttribute('fill', phaseColor(point.phase));
      const title = document.createElementNS(SVG_NS, 'title');
      title.textContent = `|${point.key}⟩ · ${this._labels.probability} ${point.probability.toFixed(4)}`
        + ` · ${this._labels.phase} ${(point.phase / Math.PI).toFixed(2)}π`;
      dot.appendChild(title);
      this._svg.appendChild(dot);
    }
    this._foot.textContent = layout.hidden
      ? this._labels.more.replace('{n}', String(layout.hidden))
      : '';
    this._svg.setAttribute('aria-label', `${this._labels.qsphere}: ${layout.points
      .map((p) => `|${p.key}⟩ ${(p.probability * 100).toFixed(1)}%`).join(', ')}`);
  }

  /// The equator, drawn as the ellipse the camera makes of it — the horizon
  /// line that tells the eye which way the sphere is turned.
  _ring(half, radius, yaw, pitch) {
    const path = document.createElementNS(SVG_NS, 'path');
    path.setAttribute('class', 'tf-qsphere__ring');
    const steps = 64;
    let d = '';
    for (let i = 0; i <= steps; i += 1) {
      const angle = (2 * Math.PI * i) / steps;
      const projected = projectVector([Math.cos(angle), Math.sin(angle), 0], yaw, pitch, radius);
      d += `${i ? 'L' : 'M'}${(half + projected.x).toFixed(2)} ${(half + projected.y).toFixed(2)}`;
    }
    path.setAttribute('d', d);
    return path;
  }
}

if (!customElements.get('tf-qsphere')) {
  customElements.define('tf-qsphere', TfQsphere);
}

export { TfQsphere, DEFAULT_LABELS as QSPHERE_LABELS };
