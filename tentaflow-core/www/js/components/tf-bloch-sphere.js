// =============================================================================
// File: components/tf-bloch-sphere.js
// Description: <tf-bloch-sphere> — ONE qubit's Bloch sphere (plan §13.2, §13.6;
//              mockups Q06/Q07/Q15). A host that shows a register lays a row of
//              these out itself; the component never assumes it is alone.
//
//              A 2D projection of the 3D sphere painted on a plain <canvas>:
//              no WebGL, because a run view can hold a dozen of these and a
//              dozen GL contexts is a context-loss machine. Orbit with the
//              pointer or the arrow keys.
//
//              The input is the Bloch vector (x, y, z) of the reduced density
//              matrix: |r| < 1 is a mixed qubit, drawn as a shorter arrow, and
//              a purity under `purity-threshold` also earns the "entangled"
//              chip of §13.6. Between two states the arrow SLERPs along the
//              rotation that took it there, which is the point of the evolution
//              animation — a gate is a turn, not a jump.
//
//  Properties: vector [x, y, z] | Float64Array — assigning animates,
//              purity  — explicit purity; derived from |r| when absent,
//              trail   — array of past vectors; the component also records its
//                        own last `trail-length` states,
//              label, explain (a sentence under the sphere, §13.6 "Wyjaśnij"),
//              labels  — i18n dict, English fallbacks only.
//              The explain area also accepts light-DOM children marked
//              slot="explain", which win over the `explain` string.
//  Attributes: size, trail-length, purity-threshold, duration, animate="off",
//              label, explain.
//  Events    : "orbit" detail {yaw, pitch} while the user turns the sphere.
//
// Example: const s = document.querySelector('tf-bloch-sphere');
//          s.label = 'q0';
//          s.vector = sim.bloch(0);
// =============================================================================

import { cssToken } from './shared-styles.js';

const DEFAULT_SIZE = 96;
const DEFAULT_TRAIL = 24;
const DEFAULT_DURATION = 420;
const DEFAULT_THRESHOLD = 0.99;

const DEFAULT_LABELS = {
  sphere: 'Bloch sphere',
  entangled: 'entangled',
  mixed: 'mixed',
  pure: 'pure',
  orbit: 'Arrow keys turn the sphere',
};

// ---------------------------------------------------------------------------
// Pure geometry
// ---------------------------------------------------------------------------

export function vectorLength(vector) {
  const [x = 0, y = 0, z = 0] = vector || [];
  return Math.sqrt(x * x + y * y + z * z);
}

/// Normalises the two shapes a Bloch payload really arrives in. The stepping
/// API flattens every qubit into one Float64Array (`blochVectors()` →
/// `[x0,y0,z0,x1,...]`), while a keyframe carries `bloch: [[x,y,z], ...]`
/// because Rust serialises `Vec<[f64; 3]>` nested. Both are live producers of
/// the same picture, so the renderers normalise here instead of forcing the
/// host to reshape a frame it got from the simulator.
export function blochVectorList(source) {
  const raw = source && source.bloch !== undefined ? source.bloch : source;
  if (!raw || typeof raw === 'string') return [];
  const list = Array.from(raw);
  if (!list.length) return [];
  const finite = (value) => (Number.isFinite(Number(value)) ? Number(value) : 0);
  if (list[0] !== null && typeof list[0] === 'object') {
    return list.map((entry) => {
      const parts = Array.from(entry || [], finite).slice(0, 3);
      while (parts.length < 3) parts.push(0);
      return parts;
    });
  }
  const out = [];
  for (let i = 0; i + 2 < list.length; i += 3) {
    out.push([finite(list[i]), finite(list[i + 1]), finite(list[i + 2])]);
  }
  return out;
}

/// Purity of the one-qubit state whose Bloch vector this is: tr(ρ²) = (1+|r|²)/2.
export function purityFromVector(vector) {
  const length = Math.min(1, vectorLength(vector));
  return (1 + length * length) / 2;
}

/// Interpolates BETWEEN two Bloch vectors the way the gate that connects them
/// moves: the direction turns along the great circle (slerp), the length
/// interpolates linearly, so a pure state stays on the surface all the way and
/// a collapsing one shrinks through the inside instead of jumping.
export function slerpVector(from, to, t) {
  const clamped = Math.max(0, Math.min(1, Number(t) || 0));
  const a = Array.from(from || [0, 0, 0], Number);
  const b = Array.from(to || [0, 0, 0], Number);
  const la = vectorLength(a);
  const lb = vectorLength(b);
  const length = la + (lb - la) * clamped;
  if (la < 1e-9 || lb < 1e-9) {
    return [
      a[0] + (b[0] - a[0]) * clamped,
      a[1] + (b[1] - a[1]) * clamped,
      a[2] + (b[2] - a[2]) * clamped,
    ];
  }
  const ua = a.map((v) => v / la);
  const ub = b.map((v) => v / lb);
  const dot = Math.max(-1, Math.min(1, ua[0] * ub[0] + ua[1] * ub[1] + ua[2] * ub[2]));
  const omega = Math.acos(dot);
  if (omega < 1e-6) return ub.map((v) => v * length);
  // Antipodal directions leave the great circle undefined; any plane through
  // both poles is as good as another, so one perpendicular axis is picked.
  if (Math.PI - omega < 1e-6) {
    const helper = Math.abs(ua[2]) < 0.9 ? [0, 0, 1] : [1, 0, 0];
    const axis = normalize(cross(ua, helper));
    const angle = Math.PI * clamped;
    const rotated = rotateAround(ua, axis, angle);
    return rotated.map((v) => v * length);
  }
  const sin = Math.sin(omega);
  const wa = Math.sin((1 - clamped) * omega) / sin;
  const wb = Math.sin(clamped * omega) / sin;
  const mixed = [0, 1, 2].map((i) => ua[i] * wa + ub[i] * wb);
  const mixedLength = vectorLength(mixed) || 1;
  return mixed.map((v) => (v / mixedLength) * length);
}

function cross(a, b) {
  return [
    a[1] * b[2] - a[2] * b[1],
    a[2] * b[0] - a[0] * b[2],
    a[0] * b[1] - a[1] * b[0],
  ];
}

function normalize(v) {
  const length = vectorLength(v) || 1;
  return v.map((component) => component / length);
}

function rotateAround(v, axis, angle) {
  const cos = Math.cos(angle);
  const sin = Math.sin(angle);
  const dot = v[0] * axis[0] + v[1] * axis[1] + v[2] * axis[2];
  const perpendicular = cross(axis, v);
  return [0, 1, 2].map((i) => (
    v[i] * cos + perpendicular[i] * sin + axis[i] * dot * (1 - cos)
  ));
}

/// Camera projection: yaw turns around the z axis, pitch lifts the pole toward
/// the viewer. `depth` is positive when the point is on the near hemisphere,
/// which is all the painter needs to know to draw behind or in front.
export function projectVector(vector, yaw, pitch, radius) {
  const [x = 0, y = 0, z = 0] = vector || [];
  const cy = Math.cos(yaw);
  const sy = Math.sin(yaw);
  const rx = x * cy - y * sy;
  const ry = x * sy + y * cy;
  const cp = Math.cos(pitch);
  const sp = Math.sin(pitch);
  const depth = ry * cp - z * sp;
  const up = ry * sp + z * cp;
  return { x: rx * radius, y: -up * radius, depth };
}

/// The short ket a state is recognisable as, for the caption under the sphere.
export function ketFor(vector) {
  const [x = 0, y = 0, z = 0] = vector || [];
  if (vectorLength(vector) < 0.995) return null;
  const near = (a, b) => Math.abs(a - b) < 0.02;
  if (near(z, 1)) return '|0⟩';
  if (near(z, -1)) return '|1⟩';
  if (near(x, 1)) return '|+⟩';
  if (near(x, -1)) return '|−⟩';
  if (near(y, 1)) return '|+i⟩';
  if (near(y, -1)) return '|−i⟩';
  return null;
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfBlochSphere extends HTMLElement {
  static get observedAttributes() {
    return ['size', 'trail-length', 'purity-threshold', 'duration', 'animate', 'label', 'explain'];
  }

  constructor() {
    super();
    this._vector = [0, 0, 1];
    this._drawn = [0, 0, 1];
    this._from = null;
    this._trail = [];
    this._externalTrail = null;
    this._purity = null;
    this._labels = { ...DEFAULT_LABELS };
    this._yaw = -0.5;
    this._pitch = 0.35;
    this._raf = 0;
    this._animationStart = 0;
    this._pointer = null;
    this._built = false;
    this._onPointerDown = this._onPointerDown.bind(this);
    this._onPointerMove = this._onPointerMove.bind(this);
    this._onPointerUp = this._onPointerUp.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
    this._frame = this._frame.bind(this);
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._render();
  }

  disconnectedCallback() {
    this._stopAnimation();
    if (typeof window !== 'undefined') {
      window.removeEventListener('pointermove', this._onPointerMove);
      window.removeEventListener('pointerup', this._onPointerUp);
    }
  }

  attributeChangedCallback() {
    if (this._built) this._render();
  }

  // -- properties ------------------------------------------------------------

  get vector() { return this._vector.slice(); }

  set vector(value) {
    const next = Array.from(value || [], Number).slice(0, 3);
    while (next.length < 3) next.push(0);
    if (next.every((component, i) => Math.abs(component - this._vector[i]) < 1e-9)) return;
    this._pushTrail(this._vector);
    this._from = this._drawn.slice();
    this._vector = next;
    // The caption and the ARIA label describe the state that was SET, so they
    // are written once here; only the arrow itself moves frame by frame.
    if (this._shouldAnimate()) {
      this._render();
      this._startAnimation();
    } else {
      this._drawn = next.slice();
      this._render();
    }
  }

  get purity() { return this._purity == null ? purityFromVector(this._vector) : this._purity; }

  set purity(value) {
    const number = Number(value);
    this._purity = Number.isFinite(number) ? number : null;
    this._render();
  }

  get trail() { return (this._externalTrail || this._trail).map((v) => v.slice()); }

  set trail(value) {
    this._externalTrail = Array.isArray(value)
      ? value.map((v) => Array.from(v, Number).slice(0, 3))
      : null;
    this._render();
  }

  get labels() { return { ...this._labels }; }

  set labels(value) {
    this._labels = { ...DEFAULT_LABELS, ...(value || {}) };
    this._render();
  }

  get label() { return this.getAttribute('label') || ''; }

  set label(value) { this.setAttribute('label', String(value ?? '')); }

  get explain() { return this.getAttribute('explain') || ''; }

  set explain(value) { this.setAttribute('explain', String(value ?? '')); }

  get entangled() { return this.purity < this._threshold(); }

  /// Clears the recorded trail — a new run starts from an empty history.
  clearTrail() {
    this._trail = [];
    this._render();
  }

  // -- construction ----------------------------------------------------------

  _build() {
    this._built = true;
    this.classList.add('tf-bloch');
    // Light DOM is rebuilt on every render, so anything the host slotted in is
    // taken out of the way first and re-attached under the caption afterwards.
    this._slotted = Array.from(this.querySelectorAll('[slot="explain"]'));
    this.innerHTML = '';

    this._canvas = document.createElement('canvas');
    this._canvas.className = 'tf-bloch__canvas';
    this._canvas.tabIndex = 0;
    this._canvas.setAttribute('role', 'img');
    this._canvas.addEventListener('pointerdown', this._onPointerDown);
    this._canvas.addEventListener('keydown', this._onKeyDown);
    this.appendChild(this._canvas);

    this._caption = document.createElement('div');
    this._caption.className = 'tf-bloch__caption';
    this.appendChild(this._caption);

    // A div, not a p: slotted explain content may legitimately be block-level.
    this._explainEl = document.createElement('div');
    this._explainEl.className = 'tf-bloch__explain';
    this.appendChild(this._explainEl);
  }

  _threshold() {
    const value = Number(this.getAttribute('purity-threshold'));
    return Number.isFinite(value) && value > 0 ? value : DEFAULT_THRESHOLD;
  }

  _size() {
    const value = Number(this.getAttribute('size'));
    return Number.isFinite(value) && value > 24 ? value : DEFAULT_SIZE;
  }

  _trailLimit() {
    const value = Number(this.getAttribute('trail-length'));
    return Number.isFinite(value) && value >= 0 ? value : DEFAULT_TRAIL;
  }

  _pushTrail(vector) {
    const limit = this._trailLimit();
    if (!limit) return;
    this._trail.push(vector.slice());
    while (this._trail.length > limit) this._trail.shift();
  }

  _shouldAnimate() {
    if (this.getAttribute('animate') === 'off') return false;
    if (!this.isConnected || typeof requestAnimationFrame !== 'function') return false;
    return !prefersReducedMotion();
  }

  _duration() {
    const value = Number(this.getAttribute('duration'));
    return Number.isFinite(value) && value >= 0 ? value : DEFAULT_DURATION;
  }

  _startAnimation() {
    this._stopAnimation();
    const duration = this._duration();
    if (!duration) {
      this._drawn = this._vector.slice();
      this._render();
      return;
    }
    this._animationStart = now();
    this._raf = requestAnimationFrame(this._frame);
  }

  _stopAnimation() {
    if (this._raf && typeof cancelAnimationFrame === 'function') cancelAnimationFrame(this._raf);
    this._raf = 0;
  }

  _frame() {
    const duration = this._duration();
    const elapsed = now() - this._animationStart;
    const t = duration ? Math.min(1, elapsed / duration) : 1;
    this._drawn = slerpVector(this._from || this._drawn, this._vector, easeOut(t));
    this._paint(this._size());
    if (t < 1) this._raf = requestAnimationFrame(this._frame);
    else this._raf = 0;
  }

  // -- interaction -----------------------------------------------------------

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
    const step = 0.15;
    const move = {
      ArrowLeft: [-step, 0], ArrowRight: [step, 0],
      ArrowUp: [0, -step], ArrowDown: [0, step],
    }[event.key];
    if (!move) return;
    event.preventDefault();
    this._orbit(move[0], move[1]);
  }

  _orbit(deltaYaw, deltaPitch) {
    this._yaw += deltaYaw;
    this._pitch = Math.max(-1.4, Math.min(1.4, this._pitch + deltaPitch));
    this._render();
    this.dispatchEvent(new CustomEvent('orbit', {
      bubbles: true, composed: true, detail: { yaw: this._yaw, pitch: this._pitch },
    }));
  }

  // -- rendering -------------------------------------------------------------

  _render() {
    if (!this._built) return;
    const size = this._size();
    const purity = this.purity;
    const entangled = purity < this._threshold();
    const ket = ketFor(this._vector);
    const length = vectorLength(this._vector);

    this.classList.toggle('tf-bloch--entangled', entangled);
    this._canvas.setAttribute('aria-label', this._ariaLabel(length, purity, entangled, ket));

    this._caption.innerHTML = '';
    if (this.label) {
      const name = document.createElement('span');
      name.className = 'tf-bloch__label';
      name.textContent = this.label;
      this._caption.appendChild(name);
    }
    const coords = document.createElement('span');
    coords.className = 'tf-bloch__coords';
    coords.textContent = ket || `|r| = ${length.toFixed(2)}`;
    this._caption.appendChild(coords);
    if (entangled) {
      const chip = document.createElement('span');
      chip.className = 'tf-bloch__chip';
      chip.textContent = this._labels.entangled;
      this._caption.appendChild(chip);
    }

    this._explainEl.replaceChildren(...this._slotted);
    if (!this._slotted.length) this._explainEl.textContent = this.explain;
    this._explainEl.hidden = !this._slotted.length && !this.explain;

    this._paint(size);
  }

  _ariaLabel(length, purity, entangled, ket) {
    const [x, y, z] = this._vector;
    const head = `${this.label ? `${this.label} — ` : ''}${this._labels.sphere}`;
    const state = ket ? `, ${ket}` : '';
    const kind = entangled ? this._labels.entangled : (length < 0.995 ? this._labels.mixed : this._labels.pure);
    return `${head}${state}, x ${x.toFixed(2)}, y ${y.toFixed(2)}, z ${z.toFixed(2)}, `
      + `|r| ${length.toFixed(2)}, ${kind} (${purity.toFixed(3)}). ${this._labels.orbit}`;
  }

  _paint(size) {
    const canvas = this._canvas;
    const ratio = (typeof window !== 'undefined' && window.devicePixelRatio) || 1;
    canvas.style.width = `${size}px`;
    canvas.style.height = `${size}px`;
    canvas.width = Math.round(size * ratio);
    canvas.height = Math.round(size * ratio);
    const ctx = typeof canvas.getContext === 'function' ? canvas.getContext('2d') : null;
    if (!ctx) return;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, size, size);

    const colors = {
      grid: cssToken('--tf-border-hover', '#2f3668'),
      faint: cssToken('--tf-border', '#1f2548'),
      text: cssToken('--tf-text-3', '#6a7196'),
      vector: cssToken('--tf-q-vector', '#f472b6'),
      trail: cssToken('--tf-accent-3', '#a78bfa'),
      fill: cssToken('--tf-accent-glow', 'rgba(99,102,241,0.18)'),
    };
    const cx = size / 2;
    const cy = size / 2;
    const radius = size / 2 - 8;
    const at = (vector) => {
      const p = projectVector(vector, this._yaw, this._pitch, radius);
      return { x: cx + p.x, y: cy + p.y, depth: p.depth };
    };

    ctx.fillStyle = colors.fill;
    ctx.beginPath();
    ctx.arc(cx, cy, radius, 0, Math.PI * 2);
    ctx.fill();
    ctx.strokeStyle = colors.grid;
    ctx.lineWidth = 1;
    ctx.stroke();

    // Equator and the prime meridian, sampled rather than drawn as ellipses:
    // an arbitrary camera turns a circle into an arbitrarily rotated ellipse,
    // and sampling is both shorter and exact.
    this._paintCircle(ctx, at, (angle) => [Math.cos(angle), Math.sin(angle), 0], colors);
    this._paintCircle(ctx, at, (angle) => [Math.cos(angle), 0, Math.sin(angle)], colors);

    const poleTop = at([0, 0, 1]);
    const poleBottom = at([0, 0, -1]);
    ctx.strokeStyle = colors.faint;
    ctx.beginPath();
    ctx.moveTo(poleTop.x, poleTop.y);
    ctx.lineTo(poleBottom.x, poleBottom.y);
    ctx.stroke();
    ctx.fillStyle = colors.text;
    ctx.font = `9px ${cssToken('--tf-mono', 'ui-monospace, monospace')}`;
    ctx.textAlign = 'center';
    ctx.fillText('|0⟩', poleTop.x, poleTop.y - 2);
    ctx.fillText('|1⟩', poleBottom.x, poleBottom.y + 9);

    const history = this._externalTrail || this._trail;
    if (history.length > 1) {
      ctx.strokeStyle = colors.trail;
      ctx.lineWidth = 1.5;
      history.forEach((vector, i) => {
        if (i === 0) return;
        const a = at(history[i - 1]);
        const b = at(vector);
        ctx.globalAlpha = 0.1 + 0.5 * (i / history.length);
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.lineTo(b.x, b.y);
        ctx.stroke();
      });
      ctx.globalAlpha = 1;
    }

    const tip = at(this._drawn);
    ctx.strokeStyle = colors.vector;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.lineTo(tip.x, tip.y);
    ctx.stroke();
    ctx.fillStyle = colors.vector;
    ctx.globalAlpha = tip.depth < 0 ? 0.55 : 1;
    ctx.beginPath();
    ctx.arc(tip.x, tip.y, 4, 0, Math.PI * 2);
    ctx.fill();
    ctx.globalAlpha = 1;
  }

  _paintCircle(ctx, at, point, colors) {
    ctx.strokeStyle = colors.faint;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let i = 0; i <= 48; i += 1) {
      const angle = (i / 48) * Math.PI * 2;
      const projected = at(point(angle));
      if (i === 0) ctx.moveTo(projected.x, projected.y);
      else ctx.lineTo(projected.x, projected.y);
    }
    ctx.stroke();
  }
}

function easeOut(t) {
  return 1 - (1 - t) * (1 - t) * (1 - t);
}

function now() {
  return typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now();
}

function prefersReducedMotion() {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

if (!customElements.get('tf-bloch-sphere')) {
  customElements.define('tf-bloch-sphere', TfBlochSphere);
}

export { TfBlochSphere, DEFAULT_LABELS as BLOCH_LABELS };
