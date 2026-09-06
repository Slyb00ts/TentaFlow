// =============================================================================
// File: components/tf-state-bars.js
// Description: <tf-state-bars> — the amplitude bars of plan §13.6 (mockup Q15):
//              one bar per basis state, HEIGHT is the probability |a|² and
//              COLOUR is the phase. The phase wheel that decodes the colours
//              belongs to the HOST panel, not here: the same wheel legends the
//              Q-sphere next to it, and two wheels would be two legends for
//              one scale.
//
//              The element does not know a simulator: it takes the same state
//              payload `tf-mime-output` does — the flat interleaved amplitude
//              vector, or a keyframe's sparse `top` list — and reuses that
//              module's `amplitudeRows` and `phaseColor`, so a bar here and a
//              row in the amplitude table can never disagree about a phase.
//
//              Bars are laid out by BASIS INDEX (|00>, |01>, |10>, |11>), not
//              by weight: the axis has to stay put while the animation morphs
//              the bars, or the eye cannot follow one state through a gate.
//              A register wider than `max-bars` shows its heaviest states and
//              says how many it left out.
//
//  Properties: state  — {amplitudes|top, numQubits}, labels — i18n dict.
//  Attributes: max-bars (default 16), size ("sm" for a compact plot).
//
// Example: bars.state = { top: frame.top, numQubits: 4 };
// =============================================================================

import { amplitudeRows, phaseColor } from './tf-mime-output.js';

const DEFAULT_MAX_BARS = 16;

const DEFAULT_LABELS = {
  amplitudes: 'Amplitudes',
  phase: 'phase',
  empty: 'no amplitudes',
  more: '+{n} more states',
};

/// The bars to draw: the heaviest `limit` states, put back in basis order so
/// the axis is monotonic. Pure — the layout of the plot is testable without a
/// document.
export function barsFor(rows, limit = DEFAULT_MAX_BARS) {
  const max = Math.max(1, Number(limit) || DEFAULT_MAX_BARS);
  const list = Array.isArray(rows) ? rows.slice() : [];
  const kept = list.slice(0, max).sort((a, b) => a.index - b.index);
  return { bars: kept, hidden: Math.max(0, list.length - kept.length) };
}

/// Bar height as a percentage of the tallest bar, so a distribution of small
/// probabilities is still readable. A state that is present but tiny keeps a
/// visible sliver rather than disappearing into the axis.
export function barHeight(probability, peak) {
  const top = Number(peak) || 0;
  if (top <= 0) return 0;
  return Math.max(1.5, (Number(probability) || 0) / top * 100);
}

class TfStateBars extends HTMLElement {
  static get observedAttributes() {
    return ['max-bars', 'size'];
  }

  constructor() {
    super();
    this._state = null;
    this._labels = { ...DEFAULT_LABELS };
    this._built = false;
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._render();
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
    this.classList.add('tf-bars');
    this._plot = document.createElement('div');
    this._plot.className = 'tf-bars__plot';
    this._axis = document.createElement('div');
    this._axis.className = 'tf-bars__axis';
    this._foot = document.createElement('div');
    this._foot.className = 'tf-bars__foot';
    this.replaceChildren(this._plot, this._axis, this._foot);
  }

  _maxBars() {
    const value = Number(this.getAttribute('max-bars'));
    return Number.isFinite(value) && value > 0 ? value : DEFAULT_MAX_BARS;
  }

  _render() {
    const state = this._state || {};
    const numQubits = Math.max(1, Number(state.numQubits) || 1);
    const { bars, hidden } = barsFor(amplitudeRows(state, numQubits), this._maxBars());
    const peak = bars.reduce((top, row) => Math.max(top, row.probability), 0);
    this._plot.replaceChildren();
    this._axis.replaceChildren();
    if (!bars.length) {
      this._plot.classList.add('is-empty');
      this._plot.textContent = this._labels.empty;
      this._foot.textContent = '';
      this.setAttribute('aria-label', `${this._labels.amplitudes}: ${this._labels.empty}`);
      return;
    }
    this._plot.classList.remove('is-empty');
    for (const row of bars) {
      const column = document.createElement('div');
      column.className = 'tf-bars__col';
      const value = document.createElement('span');
      value.className = 'tf-bars__value';
      value.textContent = row.magnitude.toFixed(2);
      const bar = document.createElement('div');
      bar.className = 'tf-bars__bar';
      bar.style.height = `${barHeight(row.probability, peak).toFixed(2)}%`;
      bar.style.setProperty('--tf-bar-phase', phaseColor(row.phase));
      bar.title = `|${row.key}⟩ · |a| ${row.magnitude.toFixed(4)} · p ${row.probability.toFixed(4)}`
        + ` · ${this._labels.phase} ${(row.phase / Math.PI).toFixed(2)}π`;
      column.append(value, bar);
      this._plot.appendChild(column);
      const tick = document.createElement('span');
      tick.textContent = `|${row.key}⟩`;
      this._axis.appendChild(tick);
    }
    this._foot.textContent = hidden ? this._labels.more.replace('{n}', String(hidden)) : '';
    this.setAttribute('role', 'img');
    this.setAttribute('aria-label', `${this._labels.amplitudes}: ${bars
      .map((row) => `|${row.key}⟩ ${(row.probability * 100).toFixed(1)}%`).join(', ')}`);
  }
}

if (!customElements.get('tf-state-bars')) {
  customElements.define('tf-state-bars', TfStateBars);
}

export { TfStateBars, DEFAULT_LABELS as STATE_BARS_LABELS };
