// =============================================================================
// File: components/tf-state-timeline.js
// Description: <tf-state-timeline> — the transport of the evolution animation
//              (plan §13.6; mockup Q15 "Ewolucja"): a compact circuit strip
//              with a PLAYHEAD that slides between gates, the play/pause/step
//              buttons, the 0,5× / 1× / 2× speed and the time slider that drags
//              the state through the circuit in both directions.
//
//              The strip is built from the RECORDED STEPS, not from a circuit:
//              one column per keyframe, each naming the gate it was taken after
//              and the qubits it touched. That is deliberate — a run's evolution
//              is exactly the frames that were recorded, and a strip drawn from
//              some other source could show a gate no frame exists for.
//
//              `position` is continuous: an integer k sits exactly on the k-th
//              recorded frame, 0 is the register before the first step, and
//              anything between is INSIDE one gate. The element owns no clock;
//              the host advances the position and this draws it, which is what
//              keeps `prefers-reduced-motion` a decision of the one place that
//              can honour it.
//
//  Properties: steps — [{step, name, qubits, collapsing}], numQubits,
//              position, labels — i18n dict.
//  Attributes: playing, speed, column, row.
//  Events    : "seek" detail {position}; "transport" detail {action};
//              "speed-change" detail {speed}.
//
// Example: strip.steps = frames.map(stepOfFrame); strip.position = 1.5;
// =============================================================================

import './tf-button.js';
import './tf-segmented.js';
import './tf-slider.js';

const SVG_NS = 'http://www.w3.org/2000/svg';
const DEFAULT_COLUMN = 46;
const DEFAULT_ROW = 30;
const LABEL_WIDTH = 40;

/// Slider granularity: a hundred stops per gate is finer than any pointer, and
/// keeps the value an integer so the range input never rounds a step away.
const STOPS_PER_STEP = 100;

export const SPEEDS = [0.5, 1, 2];

const DEFAULT_LABELS = {
  timeline: 'Evolution',
  play: 'Play',
  pause: 'Pause',
  previous: 'Previous gate',
  next: 'Next gate',
  time: 'Time in the circuit',
  before: 'before the first gate',
  empty: 'no recorded evolution',
  step: 'step',
};

// ---------------------------------------------------------------------------
// Layout — pure
// ---------------------------------------------------------------------------

/// The strip as coordinates: one column per step, one row per qubit, and the
/// cells a column paints (the gate box, its control dots, the vertical link).
export function stripLayout(steps, numQubits, { column = DEFAULT_COLUMN, row = DEFAULT_ROW } = {}) {
  const list = Array.isArray(steps) ? steps : [];
  const columnWidth = Math.max(24, Number(column) || DEFAULT_COLUMN);
  const rowHeight = Math.max(18, Number(row) || DEFAULT_ROW);
  const wires = Math.max(1, Number(numQubits) || list.reduce(
    (top, step) => Math.max(top, ...(step.qubits || []).map((q) => Number(q) + 1)),
    0,
  ) || 1);
  const columns = list.map((step, index) => {
    const qubits = Array.from(step.qubits || [], Number).filter((q) => Number.isInteger(q) && q >= 0);
    const x = LABEL_WIDTH + columnWidth * index + columnWidth / 2;
    return {
      index,
      step: Number(step.step) || index + 1,
      name: String(step.name || ''),
      collapsing: Boolean(step.collapsing),
      x,
      qubits: qubits.map((qubit, position) => ({
        qubit,
        y: rowHeight * qubit + rowHeight / 2,
        // The first operand of a two-qubit gate is its control in every gate
        // the recorder emits; the rest are targets it acts on.
        role: qubits.length > 1 && position === 0 ? 'control' : 'target',
      })),
      top: qubits.length ? rowHeight * Math.min(...qubits) + rowHeight / 2 : 0,
      bottom: qubits.length ? rowHeight * Math.max(...qubits) + rowHeight / 2 : 0,
    };
  });
  return {
    columnWidth,
    rowHeight,
    wires,
    columns,
    width: LABEL_WIDTH + columnWidth * Math.max(1, list.length) + 8,
    height: rowHeight * wires,
  };
}

/// Where the playhead sits for a continuous position. One step is one column
/// WIDE, so position 0 is the left edge of the first column and position 0.5
/// is its centre — which is exactly where that column's gate is drawn, so the
/// head crosses the gate at the moment the gate is half applied.
export function playheadX(layout, position) {
  const total = layout.columns.length;
  const p = Math.max(0, Math.min(Number(position) || 0, total));
  return LABEL_WIDTH + p * layout.columnWidth;
}

/// The step a position is INSIDE, 1-based, and how far through it is. Position
/// 0 is the register before the run and belongs to no step.
export function positionParts(position, total) {
  const count = Math.max(0, Number(total) || 0);
  const p = Math.max(0, Math.min(Number(position) || 0, count));
  if (p <= 0) return { step: 0, fraction: 0 };
  const step = Math.min(count, Math.ceil(p - 1e-9));
  return { step, fraction: p - (step - 1) };
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfStateTimeline extends HTMLElement {
  static get observedAttributes() {
    return ['playing', 'speed', 'column', 'row'];
  }

  constructor() {
    super();
    this._steps = [];
    this._numQubits = 0;
    this._position = 0;
    this._labels = { ...DEFAULT_LABELS };
    this._built = false;
    this._onClick = this._onClick.bind(this);
    this._onSlide = this._onSlide.bind(this);
    this._onSpeed = this._onSpeed.bind(this);
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._render();
  }

  attributeChangedCallback(name) {
    if (!this._built) return;
    if (name === 'playing') { this._syncTransport(); return; }
    if (name === 'speed') { this._syncSpeed(); return; }
    this._render();
  }

  get steps() { return this._steps; }

  set steps(value) {
    this._steps = Array.isArray(value) ? value : [];
    if (!this._built) this._build();
    this._render();
  }

  get numQubits() { return this._numQubits; }

  set numQubits(value) {
    this._numQubits = Math.max(0, Number(value) || 0);
    if (!this._built) this._build();
    this._render();
  }

  get position() { return this._position; }

  /// Moving the playhead must NOT rebuild the strip: this runs once per frame
  /// of the animation, and replacing a hundred SVG nodes at 60 fps is the
  /// difference between a smooth glide and a slideshow.
  set position(value) {
    this._position = Math.max(0, Math.min(Number(value) || 0, this._steps.length));
    if (!this._built) this._build();
    this._syncPlayhead();
  }

  get speed() {
    const value = Number(this.getAttribute('speed'));
    return SPEEDS.includes(value) ? value : 1;
  }

  set speed(value) { this.setAttribute('speed', String(Number(value) || 1)); }

  get playing() { return this.hasAttribute('playing'); }

  set playing(value) {
    if (value) this.setAttribute('playing', '');
    else this.removeAttribute('playing');
  }

  get labels() { return { ...this._labels }; }

  set labels(value) {
    this._labels = { ...DEFAULT_LABELS, ...(value || {}) };
    if (this._built) this._render();
  }

  _build() {
    this._built = true;
    this.classList.add('tf-timeline');
    this._scroll = document.createElement('div');
    this._scroll.className = 'tf-timeline__scroll';
    this._svg = document.createElementNS(SVG_NS, 'svg');
    this._svg.setAttribute('class', 'tf-timeline__strip');
    this._svg.setAttribute('role', 'img');
    this._scroll.appendChild(this._svg);

    this._transport = document.createElement('div');
    this._transport.className = 'tf-timeline__transport';
    this._transport.innerHTML = `
      <tf-button variant="ghost" size="sm" icon="chevron-left" data-transport="prev"></tf-button>
      <tf-button variant="primary" size="sm" icon="play" data-transport="play"></tf-button>
      <tf-button variant="ghost" size="sm" icon="chevron-right" data-transport="next"></tf-button>
      <tf-segmented size="sm" value="1" data-speed>
        ${SPEEDS.map((s) => `<option value="${s}">${String(s).replace('.', ',')}×</option>`).join('')}
      </tf-segmented>
      <tf-slider class="tf-timeline__slider" min="0" max="${STOPS_PER_STEP}" value="0" step="1"></tf-slider>
      <span class="tf-timeline__value"></span>`;
    this._transport.addEventListener('click', this._onClick);
    this._slider = this._transport.querySelector('tf-slider');
    this._slider.addEventListener('input', this._onSlide);
    this._segmented = this._transport.querySelector('[data-speed]');
    this._segmented.addEventListener('change', this._onSpeed);
    this._value = this._transport.querySelector('.tf-timeline__value');
    this.replaceChildren(this._scroll, this._transport);
  }

  _onClick(event) {
    const button = event.target.closest('[data-transport]');
    if (!button) return;
    const action = button.dataset.transport;
    if (action === 'play') this.playing = !this.playing;
    this.dispatchEvent(new CustomEvent('transport', { detail: { action } }));
  }

  _onSlide(event) {
    event.stopPropagation();
    const stops = Number(event.detail?.value ?? this._slider.value) || 0;
    const position = stops / STOPS_PER_STEP;
    this._position = position;
    this._syncPlayhead();
    this.dispatchEvent(new CustomEvent('seek', { detail: { position } }));
  }

  _onSpeed(event) {
    event.stopPropagation();
    const speed = Number(event.detail?.value) || 1;
    this.setAttribute('speed', String(speed));
    this.dispatchEvent(new CustomEvent('speed-change', { detail: { speed } }));
  }

  _syncTransport() {
    const play = this._transport.querySelector('[data-transport="play"]');
    if (!play) return;
    play.setAttribute('icon', this.playing ? 'pause' : 'play');
    play.setAttribute('title', this.playing ? this._labels.pause : this._labels.play);
    play.setAttribute('aria-label', this.playing ? this._labels.pause : this._labels.play);
    // Icon-only buttons, so the name is the only thing a screen reader has.
    for (const [action, label] of [['prev', this._labels.previous], ['next', this._labels.next]]) {
      const button = this._transport.querySelector(`[data-transport="${action}"]`);
      if (!button) continue;
      button.setAttribute('title', label);
      button.setAttribute('aria-label', label);
    }
  }

  _syncSpeed() {
    if (this._segmented) this._segmented.setAttribute('value', String(this.speed));
  }

  _syncPlayhead() {
    if (!this._layout) return;
    const head = this._svg.querySelector('.tf-timeline__playhead');
    const total = this._layout.columns.length;
    if (head) {
      const x = playheadX(this._layout, this._position);
      head.setAttribute('x1', x.toFixed(2));
      head.setAttribute('x2', x.toFixed(2));
    }
    for (const column of this._svg.querySelectorAll('[data-column]')) {
      const index = Number(column.dataset.column);
      column.classList.toggle('is-done', this._position >= index + 1 - 1e-9);
      column.classList.toggle('is-now', this._position > index && this._position < index + 1 - 1e-9);
    }
    const stops = Math.round(this._position * STOPS_PER_STEP);
    if (this._slider && Number(this._slider.getAttribute('value')) !== stops) {
      this._slider.setAttribute('value', String(stops));
    }
    const { step, fraction } = positionParts(this._position, total);
    const current = step ? this._layout.columns[step - 1] : null;
    this._value.textContent = current
      ? `t = ${this._position.toFixed(2)} / ${total} · ${this._labels.step} ${step} · ${current.name}`
      : `t = 0,00 / ${total} · ${this._labels.before}`;
    this._value.dataset.fraction = fraction.toFixed(2);
  }

  _render() {
    const layout = stripLayout(this._steps, this._numQubits, {
      column: Number(this.getAttribute('column')) || DEFAULT_COLUMN,
      row: Number(this.getAttribute('row')) || DEFAULT_ROW,
    });
    this._layout = layout;
    this._svg.setAttribute('viewBox', `0 0 ${layout.width} ${layout.height}`);
    this._svg.setAttribute('width', String(layout.width));
    this._svg.setAttribute('height', String(layout.height));
    this._svg.replaceChildren();
    for (let qubit = 0; qubit < layout.wires; qubit += 1) {
      const y = layout.rowHeight * qubit + layout.rowHeight / 2;
      const wire = document.createElementNS(SVG_NS, 'line');
      wire.setAttribute('class', 'tf-timeline__wire');
      wire.setAttribute('x1', String(LABEL_WIDTH - 6));
      wire.setAttribute('y1', y.toFixed(2));
      wire.setAttribute('x2', String(layout.width - 4));
      wire.setAttribute('y2', y.toFixed(2));
      this._svg.appendChild(wire);
      const label = document.createElementNS(SVG_NS, 'text');
      label.setAttribute('class', 'tf-timeline__qubit');
      label.setAttribute('x', String(LABEL_WIDTH - 10));
      label.setAttribute('y', (y + 4).toFixed(2));
      label.setAttribute('text-anchor', 'end');
      label.textContent = `q${qubit}`;
      this._svg.appendChild(label);
    }
    for (const column of layout.columns) {
      this._svg.appendChild(this._columnEl(column, layout));
    }
    const head = document.createElementNS(SVG_NS, 'line');
    head.setAttribute('class', 'tf-timeline__playhead');
    head.setAttribute('y1', '0');
    head.setAttribute('y2', String(layout.height));
    this._svg.appendChild(head);

    this._slider.setAttribute('max', String(Math.max(1, layout.columns.length) * STOPS_PER_STEP));
    this._slider.setAttribute('aria-label', this._labels.time);
    this._syncTransport();
    this._syncSpeed();
    this._syncPlayhead();
    this._svg.setAttribute('aria-label', layout.columns.length
      ? `${this._labels.timeline}: ${layout.columns.map((c) => `${c.step}. ${c.name}`).join(', ')}`
      : `${this._labels.timeline}: ${this._labels.empty}`);
  }

  _columnEl(column, layout) {
    const group = document.createElementNS(SVG_NS, 'g');
    group.setAttribute('class', 'tf-timeline__col');
    group.dataset.column = String(column.index);
    if (column.qubits.length > 1) {
      const link = document.createElementNS(SVG_NS, 'line');
      link.setAttribute('class', 'tf-timeline__link');
      link.setAttribute('x1', column.x.toFixed(2));
      link.setAttribute('y1', column.top.toFixed(2));
      link.setAttribute('x2', column.x.toFixed(2));
      link.setAttribute('y2', column.bottom.toFixed(2));
      group.appendChild(link);
    }
    for (const cell of column.qubits) {
      if (cell.role === 'control') {
        const dot = document.createElementNS(SVG_NS, 'circle');
        dot.setAttribute('class', 'tf-timeline__control');
        dot.setAttribute('cx', column.x.toFixed(2));
        dot.setAttribute('cy', cell.y.toFixed(2));
        dot.setAttribute('r', '4');
        group.appendChild(dot);
        continue;
      }
      const box = document.createElementNS(SVG_NS, 'rect');
      box.setAttribute('class', `tf-timeline__gate${column.collapsing ? ' is-measure' : ''}`);
      box.setAttribute('x', (column.x - 11).toFixed(2));
      box.setAttribute('y', (cell.y - 9).toFixed(2));
      box.setAttribute('width', '22');
      box.setAttribute('height', '18');
      box.setAttribute('rx', '4');
      group.appendChild(box);
      const text = document.createElementNS(SVG_NS, 'text');
      text.setAttribute('class', 'tf-timeline__gate-label');
      text.setAttribute('x', column.x.toFixed(2));
      text.setAttribute('y', (cell.y + 4).toFixed(2));
      text.setAttribute('text-anchor', 'middle');
      text.textContent = column.name.slice(0, 3).toUpperCase();
      group.appendChild(text);
    }
    const hit = document.createElementNS(SVG_NS, 'rect');
    hit.setAttribute('class', 'tf-timeline__hit');
    hit.setAttribute('x', (column.x - layout.columnWidth / 2).toFixed(2));
    hit.setAttribute('y', '0');
    hit.setAttribute('width', String(layout.columnWidth));
    hit.setAttribute('height', String(layout.height));
    hit.addEventListener('click', () => {
      const position = column.index + 1;
      this._position = position;
      this._syncPlayhead();
      this.dispatchEvent(new CustomEvent('seek', { detail: { position } }));
    });
    const title = document.createElementNS(SVG_NS, 'title');
    title.textContent = `${column.step}. ${column.name}`;
    hit.appendChild(title);
    group.appendChild(hit);
    return group;
  }
}

if (!customElements.get('tf-state-timeline')) {
  customElements.define('tf-state-timeline', TfStateTimeline);
}

export { TfStateTimeline, DEFAULT_LABELS as STATE_TIMELINE_LABELS };
