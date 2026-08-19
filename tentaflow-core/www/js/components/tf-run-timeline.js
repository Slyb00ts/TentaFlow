// =============================================================================
// File: tf-run-timeline.js — multi-lane run timeline (model / messages / tools)
// Description:
//   <tf-run-timeline> plots one run as three lanes on a Canvas2D surface, with
//   turn boundaries, a TTFT/decode split on the model band, a minimap of the
//   whole dataset and an explicit time-scale toggle. Light DOM; every chrome
//   primitive is a tf-* component and the class vocabulary lives in
//   controls.css, so the widget also works when a host mounts it inside a
//   shadow root (controls.css is the only sheet adopted there).
//
//   Three hosts share it: the global Zdarzenia browser, the Code Studio session
//   tab and the agent run view. The ledger list and the record inspector are
//   NOT part of the component — the host owns those and couples them through
//   the events below plus highlight().
//
// Properties
//   .records = [{
//       id, seq, start, duration, lane, kind, origin, actor, actorKind,
//       name, detail, turn, ttft, error }]
//     start    — ms on the run clock (0 = run start)
//     duration — ms, or null/undefined for a record still IN FLIGHT. An
//                in-flight record is drawn as a start marker, never as a bar:
//                the log has no end for it and inventing one would be a lie.
//     lane     — 'model' | 'messages' | 'tools'
//     ttft     — ms until the first token, or null when not applicable
//   .epoch    = ms since the Unix epoch matching start=0. Set it and the axis,
//               the range chip and the tooltip show wall clock; leave it 0 and
//               they show elapsed time. Nothing is guessed either way.
//   .scale    = 'time' | 'equal'   (also the `scale` attribute)
//   .range    = { t0, t1 }         (ms on the run clock)
//   .selected = record id | null
//
// Methods
//   resetRange()   — back to the full extent of the data
//   highlight(id)  — external hover source (a ledger row) marks a band
//   destroy()      — detach observers and listeners
//
//   Property setters and the methods above are silent: only a user gesture on
//   the widget emits. That keeps a host that mirrors state back into the
//   component from looping.
//
// Events (bubbles, detail as noted)
//   record-hover  { id }        id is null when the pointer leaves every band
//   record-select { id }
//   range-change  { t0, t1 }
//
// Example:
//   const tl = document.createElement('tf-run-timeline');
//   tl.epoch = runStartedAtMs;
//   tl.records = records;
//   tl.addEventListener('record-hover', (e) => ledger.highlight(e.detail.id));
// =============================================================================

import './tf-segmented.js';
import './tf-chip.js';
import { I18n } from '/js/i18n.js';

const LANES = ['model', 'messages', 'tools'];
// Band geometry in CSS px, kept in sync with the .tf-rt__track height in
// controls.css. Tool calls alternate between two sub-rows so two parallel
// calls in one turn stay two visible bands instead of one overlap.
const LANE_TOP = [8, 32, 56];
const TOOL_STAGGER = 14;
const BAND_H = 12;
const TRACK_H = 82;
const MINIMAP_H = 22;

// A band never gets thinner than this fraction of the track, otherwise a 22 ms
// call next to a 4-minute build would round away to nothing.
const MIN_BAND_FRAC = 0.0035;
const MIN_EQUAL_FRAC = 0.014;
// Below this drag width the gesture was a click, not a brush.
const BRUSH_MIN_FRAC = 0.012;
const MIN_SPAN_MS = 200;
const ZOOM_IN = 0.8;
const ZOOM_OUT = 1.25;

const TICK_STEPS_MS = [
  1, 2, 5, 10, 25, 50, 100, 250, 500,
  1000, 2000, 5000, 10000, 15000, 30000,
  60000, 120000, 300000, 600000, 1800000, 3600000,
];

function ti(key, vars, fallback) {
  const v = I18n.t(`run_timeline.${key}`, vars || null);
  return v === `run_timeline.${key}` && fallback != null ? fallback : v;
}

function clamp(v, lo, hi) {
  return v < lo ? lo : v > hi ? hi : v;
}

function laneIndex(lane) {
  const i = LANES.indexOf(lane);
  return i < 0 ? 0 : i;
}

// null/undefined means the record is still in flight. Number(null) is 0, so the
// nullish check has to come first — otherwise an open record would silently
// become a zero-length bar, which is exactly the fabricated time the log must
// never show.
function durationOf(r) {
  if (r.duration === null || r.duration === undefined) return null;
  const d = Number(r.duration);
  return Number.isFinite(d) && d >= 0 ? d : null;
}

function startOf(r) {
  const s = Number(r.start);
  return Number.isFinite(s) ? s : 0;
}

function endOf(r) {
  const d = durationOf(r);
  return d === null ? startOf(r) : startOf(r) + d;
}

export class TfRunTimeline extends HTMLElement {
  static get observedAttributes() { return ['scale']; }

  constructor() {
    super();
    this._records = [];
    this._epoch = 0;
    this._scale = 'time';
    this._range = null;
    this._selected = null;
    this._hot = null;

    this._built = false;
    this._ro = null;
    this._unsubLang = null;
    this._raf = 0;
    this._geomDirty = true;
    this._colors = null;
    this._hatch = null;

    // Hit testing: the bands are re-drawn into an offscreen canvas keyed by
    // record index, so a pointer move is one array lookup instead of a scan
    // over every visible record.
    this._hitCanvas = null;
    this._hitData = null;
    this._hitStale = true;
    this._hitIndex = [];

    this._drag = null;
    this._miniDrag = false;

    this._onWheel = this._onWheel.bind(this);
    this._onPointerDown = this._onPointerDown.bind(this);
    this._onPointerMove = this._onPointerMove.bind(this);
    this._onPointerUp = this._onPointerUp.bind(this);
    this._onPointerLeave = this._onPointerLeave.bind(this);
    this._onDblClick = this._onDblClick.bind(this);
    this._onContextMenu = this._onContextMenu.bind(this);
    this._onMiniDown = this._onMiniDown.bind(this);
    this._onMiniMove = this._onMiniMove.bind(this);
    this._onMiniUp = this._onMiniUp.bind(this);
  }

  connectedCallback() {
    if (!this._built) this._build();
    if (typeof ResizeObserver !== 'undefined' && !this._ro) {
      this._ro = new ResizeObserver(() => this._invalidate());
      this._ro.observe(this._track);
    }
    if (!this._unsubLang && I18n.subscribe) {
      this._unsubLang = I18n.subscribe(() => this._invalidate(false));
    }
    this._invalidate();
  }

  disconnectedCallback() {
    if (this._unsubLang) { this._unsubLang(); this._unsubLang = null; }
    if (this._ro) { this._ro.disconnect(); this._ro = null; }
    if (this._raf) { cancelAnimationFrame(this._raf); this._raf = 0; }
  }

  attributeChangedCallback(name, _prev, value) {
    if (name === 'scale' && value && value !== this._scale) {
      this._scale = value === 'equal' ? 'equal' : 'time';
      if (this._built) {
        this._scaleCtl.setAttribute('value', this._scale);
        this._invalidate();
      }
    }
  }

  // ---------------------------------------------------------------------------
  // Public API
  // ---------------------------------------------------------------------------

  set records(value) {
    const wasFull = this._isFullExtent();
    this._records = Array.isArray(value) ? value.slice() : [];
    this._records.sort((a, b) => startOf(a) - startOf(b) || (a.seq || 0) - (b.seq || 0));
    if (wasFull || !this._range) this._range = null;
    else {
      const ext = this.extent;
      this._range = {
        t0: clamp(this._range.t0, ext.t0, ext.t1),
        t1: clamp(this._range.t1, ext.t0, ext.t1),
      };
      if (this._range.t1 - this._range.t0 < 1) this._range = null;
    }
    this._invalidate();
  }

  get records() { return this._records; }

  set epoch(value) {
    const n = Number(value);
    this._epoch = Number.isFinite(n) ? n : 0;
    this._invalidate();
  }

  get epoch() { return this._epoch; }

  set scale(value) {
    const next = value === 'equal' ? 'equal' : 'time';
    if (next === this._scale) return;
    this.setAttribute('scale', next);
  }

  get scale() { return this._scale; }

  set range(value) {
    if (!value) { this._range = null; this._invalidate(); return; }
    const ext = this.extent;
    const t0 = clamp(Number(value.t0), ext.t0, ext.t1);
    const t1 = clamp(Number(value.t1), t0 + 1, ext.t1);
    this._range = { t0, t1 };
    this._invalidate();
  }

  get range() {
    if (this._range) return { ...this._range };
    return this.extent;
  }

  get extent() {
    if (!this._records.length) return { t0: 0, t1: 1 };
    let lo = Infinity;
    let hi = -Infinity;
    for (const r of this._records) {
      const s = startOf(r);
      const e = endOf(r);
      if (s < lo) lo = s;
      if (e > hi) hi = e;
    }
    if (!Number.isFinite(lo)) return { t0: 0, t1: 1 };
    if (hi <= lo) hi = lo + 1;
    return { t0: lo, t1: hi };
  }

  set selected(id) {
    const next = id == null ? null : String(id);
    if (next === this._selected) return;
    this._selected = next;
    this._invalidate(false);
  }

  get selected() { return this._selected; }

  resetRange() {
    this._range = null;
    this._invalidate();
  }

  highlight(id) {
    const next = id == null ? null : String(id);
    if (next === this._hot) return;
    this._hot = next;
    this._invalidate(false);
  }

  destroy() {
    this.disconnectedCallback();
    if (this._track) {
      this._track.removeEventListener('wheel', this._onWheel);
      this._track.removeEventListener('pointerdown', this._onPointerDown);
      this._track.removeEventListener('pointermove', this._onPointerMove);
      this._track.removeEventListener('pointerup', this._onPointerUp);
      this._track.removeEventListener('pointerleave', this._onPointerLeave);
      this._track.removeEventListener('dblclick', this._onDblClick);
      this._track.removeEventListener('contextmenu', this._onContextMenu);
    }
    if (this._minimap) {
      this._minimap.removeEventListener('pointerdown', this._onMiniDown);
      this._minimap.removeEventListener('pointermove', this._onMiniMove);
      this._minimap.removeEventListener('pointerup', this._onMiniUp);
    }
    this.innerHTML = '';
    this._built = false;
  }

  // ---------------------------------------------------------------------------
  // DOM
  // ---------------------------------------------------------------------------

  _build() {
    this.innerHTML = '';
    const root = document.createElement('div');
    root.className = 'tf-rt';

    const bar = document.createElement('div');
    bar.className = 'tf-rt__bar';

    const scaleCtl = document.createElement('tf-segmented');
    scaleCtl.className = 'tf-rt__scale';
    scaleCtl.setAttribute('size', 'sm');
    scaleCtl.setAttribute('value', this._scale);
    for (const value of ['time', 'equal']) {
      const opt = document.createElement('option');
      opt.setAttribute('value', value);
      scaleCtl.appendChild(opt);
    }
    this._scaleCtl = scaleCtl;
    scaleCtl.addEventListener('change', (e) => {
      e.stopPropagation();
      this._scale = e.detail.value === 'equal' ? 'equal' : 'time';
      this.setAttribute('scale', this._scale);
      this._invalidate();
    });
    bar.appendChild(scaleCtl);

    const spacer = document.createElement('span');
    spacer.className = 'tf-rt__spacer';
    bar.appendChild(spacer);

    this._countEl = document.createElement('span');
    this._countEl.className = 'tf-rt__count';
    bar.appendChild(this._countEl);

    this._rangeChip = document.createElement('tf-chip');
    this._rangeChip.className = 'tf-rt__range';
    this._rangeChip.setAttribute('status', 'accent');
    this._rangeChip.setAttribute('clickable', '');
    this._rangeChip.title = ti('range_reset', null, 'reset to full extent');
    this._rangeChip.hidden = true;
    this._rangeChip.addEventListener('click', () => {
      this._range = null;
      this._invalidate();
      this._emitRange();
    });
    bar.appendChild(this._rangeChip);
    root.appendChild(bar);

    const plot = document.createElement('div');
    plot.className = 'tf-rt__plot';
    const labels = document.createElement('div');
    labels.className = 'tf-rt__lanes';
    for (const [i] of LANES.entries()) {
      const s = document.createElement('span');
      s.style.top = `${LANE_TOP[i] + 1}px`;
      labels.appendChild(s);
    }
    plot.appendChild(labels);
    this._laneLabels = labels;

    this._track = document.createElement('div');
    this._track.className = 'tf-rt__track';
    this._canvas = document.createElement('canvas');
    this._canvas.className = 'tf-rt__canvas';
    this._track.appendChild(this._canvas);

    this._brush = document.createElement('div');
    this._brush.className = 'tf-rt__brush';
    this._brush.hidden = true;
    this._track.appendChild(this._brush);


    this._empty = document.createElement('div');
    this._empty.className = 'tf-rt__empty';
    this._empty.hidden = true;
    this._track.appendChild(this._empty);

    plot.appendChild(this._track);
    root.appendChild(plot);

    const axisRow = document.createElement('div');
    axisRow.className = 'tf-rt__axis-row';
    const axisGutter = document.createElement('span');
    axisGutter.className = 'tf-rt__axis-gutter';
    axisRow.appendChild(axisGutter);
    this._axis = document.createElement('div');
    this._axis.className = 'tf-rt__axis';
    axisRow.appendChild(this._axis);
    root.appendChild(axisRow);

    this._minimap = document.createElement('div');
    this._minimap.className = 'tf-rt__minimap';
    this._miniCanvas = document.createElement('canvas');
    this._minimap.appendChild(this._miniCanvas);
    this._miniWin = document.createElement('div');
    this._miniWin.className = 'tf-rt__window';
    this._minimap.appendChild(this._miniWin);
    root.appendChild(this._minimap);

    const legend = document.createElement('div');
    legend.className = 'tf-rt__legend';
    for (const cls of ['wait', 'decode', 'tool', 'error', 'inflight']) {
      const item = document.createElement('i');
      const sw = document.createElement('span');
      sw.className = `tf-rt__sw tf-rt__sw--${cls}`;
      item.appendChild(sw);
      const text = document.createElement('span');
      text.className = 'tf-rt__text';
      item.appendChild(text);
      legend.appendChild(item);
    }
    root.appendChild(legend);
    this._legend = legend;

    const hint = document.createElement('div');
    hint.className = 'tf-rt__hint';
    for (let i = 0; i < 4; i += 1) {
      const s = document.createElement('span');
      s.appendChild(document.createElement('kbd'));
      const text = document.createElement('span');
      text.className = 'tf-rt__text';
      s.appendChild(text);
      hint.appendChild(s);
    }
    hint.appendChild(document.createElement('span'));
    root.appendChild(hint);
    this._hint = hint;

    // The tooltip lives on the root, not on the track: the track clips its
    // children so the bands stay inside the view, and a tooltip clipped at the
    // 82nd pixel would lose its last lines.
    this._tip = document.createElement('div');
    this._tip.className = 'tf-rt__tip';
    this._tip.hidden = true;
    root.appendChild(this._tip);
    this._root = root;

    this.appendChild(root);

    this._track.addEventListener('wheel', this._onWheel, { passive: false });
    this._track.addEventListener('pointerdown', this._onPointerDown);
    this._track.addEventListener('pointermove', this._onPointerMove);
    this._track.addEventListener('pointerup', this._onPointerUp);
    this._track.addEventListener('pointerleave', this._onPointerLeave);
    this._track.addEventListener('dblclick', this._onDblClick);
    this._track.addEventListener('contextmenu', this._onContextMenu);
    this._minimap.addEventListener('pointerdown', this._onMiniDown);
    this._minimap.addEventListener('pointermove', this._onMiniMove);
    this._minimap.addEventListener('pointerup', this._onMiniUp);

    this._built = true;
  }

  // Static chrome text is applied on render, not on build: the element can be
  // upgraded before I18n.init() has resolved, and a host may switch language
  // while the widget is mounted.
  _applyLabels() {
    this._scaleCtl.title = ti('scale_hint', null,
      'At real-time scale a 22 ms call next to a 4-minute build is invisible.');
    const segs = this._scaleCtl.querySelectorAll('.tf-seg-opt');
    const scaleText = [ti('scale_time', null, 'real time'), ti('scale_equal', null, 'equal spacing')];
    segs.forEach((el, i) => { if (scaleText[i]) el.lastChild.textContent = scaleText[i]; });

    const lanes = this._laneLabels.querySelectorAll('span');
    LANES.forEach((lane, i) => { lanes[i].textContent = ti(`lane_${lane}`, null, lane); });

    this._empty.textContent = ti('empty', null, 'No events in this range');
    this._rangeChip.title = ti('range_reset', null, 'back to the full extent');

    const legendText = [
      ti('legend_wait', null, 'waiting (TTFT)'),
      ti('legend_decode', null, 'decoding'),
      ti('legend_tool', null, 'tool'),
      ti('legend_error', null, 'error'),
      ti('legend_inflight', null, 'in flight (start only)'),
    ];
    this._legend.querySelectorAll('.tf-rt__text').forEach((el, i) => { el.textContent = legendText[i]; });

    const hints = [
      [ti('key_wheel', null, 'wheel'), ti('hint_zoom', null, 'zoom at the cursor')],
      [ti('key_drag', null, 'drag'), ti('hint_brush', null, 'select a range')],
      [ti('key_right', null, 'right button'), ti('hint_pan', null, 'pan')],
      [ti('key_dblclick', null, 'double-click'), ti('hint_reset', null, 'full extent')],
    ];
    const spans = this._hint.children;
    hints.forEach(([k, text], i) => {
      spans[i].querySelector('kbd').textContent = k;
      spans[i].querySelector('.tf-rt__text').textContent = text;
    });
    spans[4].textContent = ti('hint_minimap', null, 'minimap below = whole run + view window');
  }

  // ---------------------------------------------------------------------------
  // Data helpers
  // ---------------------------------------------------------------------------

  _isFullExtent() {
    if (!this._range) return true;
    const ext = this.extent;
    return this._range.t0 <= ext.t0 && this._range.t1 >= ext.t1;
  }

  // Records overlapping the current view window. Mirrors the ledger's own
  // filter, so a band and its row appear and disappear together.
  _visible() {
    const { t0, t1 } = this.range;
    return this._records.filter((r) => endOf(r) >= t0 && startOf(r) <= t1);
  }

  // Record → x/width in CSS px. The 'equal' branch drops the time axis on
  // purpose: at real-time scale a 22 ms call next to a 4-minute build is
  // invisible, and equal widths make the axis non-linear — which is exactly
  // why the scale is a toggle the user flips knowingly, never an automatism.
  _place(r, order, count, width) {
    if (this._scale === 'equal') {
      const i = order.get(r.id) ?? 0;
      const n = Math.max(1, count);
      return {
        x: (i / n) * width,
        w: Math.max(MIN_EQUAL_FRAC * width, (width / n) * 0.82),
        marker: durationOf(r) === null,
      };
    }
    const { t0, t1 } = this.range;
    const span = Math.max(1, t1 - t0);
    const dur = durationOf(r);
    return {
      x: ((startOf(r) - t0) / span) * width,
      w: dur === null ? 0 : Math.max(MIN_BAND_FRAC * width, (dur / span) * width),
      marker: dur === null,
    };
  }

  _bandTop(r) {
    const li = laneIndex(r.lane);
    if (li === 2) return LANE_TOP[2] + ((Number(r.seq) || 0) % 2) * TOOL_STAGGER;
    return LANE_TOP[li];
  }

  // ---------------------------------------------------------------------------
  // Formatting
  // ---------------------------------------------------------------------------

  _lang() { return I18n.getLanguage ? I18n.getLanguage() : 'en'; }

  _fmtDuration(ms) {
    const lang = this._lang();
    if (ms >= 60000) {
      return `${(ms / 60000).toLocaleString(lang, { minimumFractionDigits: 1, maximumFractionDigits: 1 })} min`;
    }
    if (ms >= 1000) {
      return `${(ms / 1000).toLocaleString(lang, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} s`;
    }
    return `${Math.round(ms).toLocaleString(lang)} ms`;
  }

  // With no epoch there is no wall clock to show, so the label stays elapsed
  // time rather than inventing a date.
  _fmtStamp(ms) {
    if (!this._epoch) return this._fmtDuration(Math.max(0, ms));
    return new Date(this._epoch + ms).toLocaleTimeString(this._lang(), { hour12: false });
  }

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  // `geom` false means only the highlight changed: the band rectangles stayed
  // where they were, so the hit map survives and no getImageData is needed.
  _invalidate(geom = true) {
    if (geom) this._geomDirty = true;
    if (!this._built || this._raf) return;
    this._raf = requestAnimationFrame(() => {
      this._raf = 0;
      this._render();
    });
  }

  _palette() {
    if (this._colors) return this._colors;
    const cs = getComputedStyle(this);
    const read = (name, fallback) => (cs.getPropertyValue(name).trim() || fallback);
    this._colors = {
      waitA: read('--tf-rt-wait-a', '#4b5563'),
      waitB: read('--tf-rt-wait-b', '#374151'),
      decode: read('--tf-rt-decode', '#6366f1'),
      tool: read('--tf-rt-tool', '#0e7490'),
      error: read('--tf-rt-error', '#ef4444'),
      inflight: read('--tf-rt-inflight', '#f59e0b'),
      turn: read('--tf-rt-turn', '#2f3668'),
      label: read('--tf-rt-label', '#6a7196'),
      hot: read('--tf-rt-hot', '#a78bfa'),
      selected: read('--tf-rt-selected', '#818cf8'),
    };
    return this._colors;
  }

  _hatchPattern(ctx) {
    if (this._hatch) return this._hatch;
    const c = this._palette();
    const tile = document.createElement('canvas');
    tile.width = 8;
    tile.height = 8;
    const g = tile.getContext('2d');
    g.fillStyle = c.waitB;
    g.fillRect(0, 0, 8, 8);
    g.strokeStyle = c.waitA;
    g.lineWidth = 4;
    g.beginPath();
    g.moveTo(-2, 6); g.lineTo(6, -2);
    g.moveTo(2, 10); g.lineTo(10, 2);
    g.stroke();
    this._hatch = ctx.createPattern(tile, 'repeat');
    return this._hatch;
  }

  _render() {
    this._applyLabels();
    const width = Math.max(1, this._track.clientWidth);
    const dpr = window.devicePixelRatio || 1;
    this._canvas.width = Math.round(width * dpr);
    this._canvas.height = Math.round(TRACK_H * dpr);
    this._canvas.style.width = `${width}px`;
    this._canvas.style.height = `${TRACK_H}px`;

    const ctx = this._canvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, TRACK_H);

    const list = this._visible();
    const order = new Map();
    list.forEach((r, i) => order.set(r.id, i));

    this._empty.hidden = list.length > 0;
    this._drawTurns(ctx, list, order, width);
    this._drawBands(ctx, list, order, width, false);

    if (this._geomDirty) { this._hitStale = true; this._geomDirty = false; }
    this._renderAxis(width);
    this._renderMinimap();
    this._renderChrome(list.length);
  }

  _drawTurns(ctx, list, order, width) {
    const c = this._palette();
    const seen = new Set();
    ctx.save();
    ctx.font = '700 9px "JetBrains Mono", ui-monospace, monospace';
    ctx.textBaseline = 'top';
    for (const r of list) {
      const turn = r.turn;
      if (turn == null || seen.has(turn)) continue;
      seen.add(turn);
      const p = this._place(r, order, list.length, width);
      if (p.x < -0.05 * width || p.x > 1.05 * width) continue;
      const x = Math.round(p.x) + 0.5;
      ctx.strokeStyle = c.turn;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, TRACK_H);
      ctx.stroke();
      ctx.fillStyle = c.label;
      ctx.fillText(`T${turn}`, x + 4, 2);
    }
    ctx.restore();
  }

  _drawBands(ctx, list, order, width, hit) {
    const c = this._palette();
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, width, TRACK_H);
    ctx.clip();

    if (hit) this._hitIndex = list.slice();

    for (const [i, r] of list.entries()) {
      const p = this._place(r, order, list.length, width);
      const top = this._bandTop(r);
      if (p.x > width + 20 || p.x + p.w < -20) continue;
      const key = hit ? this._indexColor(i) : null;

      if (p.marker) {
        this._drawInFlight(ctx, p.x, top, key || c.inflight, hit);
        continue;
      }

      const w = Math.max(1, p.w);
      if (hit) {
        ctx.fillStyle = key;
        ctx.fillRect(p.x, top, Math.max(3, w), BAND_H);
        continue;
      }

      const ttft = Number(r.ttft);
      const hasSplit = laneIndex(r.lane) === 0 && Number.isFinite(ttft) && ttft > 0 && !r.error;
      ctx.save();
      ctx.beginPath();
      this._roundRect(ctx, p.x, top, w, BAND_H, 3);
      ctx.clip();
      if (r.error) {
        ctx.fillStyle = c.error;
        ctx.fillRect(p.x, top, w, BAND_H);
      } else if (hasSplit) {
        // The model band is cut in two: waiting for the first token and
        // decoding. "the model thought for 8 s" does not say which it did.
        const waitW = Math.min(w * 0.9, (ttft / Math.max(1, durationOf(r))) * w);
        ctx.fillStyle = this._hatchPattern(ctx);
        ctx.fillRect(p.x, top, waitW, BAND_H);
        ctx.fillStyle = c.decode;
        ctx.fillRect(p.x + waitW, top, w - waitW, BAND_H);
      } else {
        ctx.fillStyle = laneIndex(r.lane) === 2 ? c.tool : c.decode;
        ctx.fillRect(p.x, top, w, BAND_H);
      }
      ctx.restore();

      ctx.strokeStyle = 'rgba(0,0,0,0.35)';
      ctx.lineWidth = 1;
      ctx.beginPath();
      this._roundRect(ctx, p.x + 0.5, top + 0.5, Math.max(1, w - 1), BAND_H - 1, 3);
      ctx.stroke();

      if (r.id === this._hot || r.id === this._selected) {
        ctx.strokeStyle = r.id === this._selected ? c.selected : c.hot;
        ctx.lineWidth = 2;
        ctx.beginPath();
        this._roundRect(ctx, p.x - 1.5, top - 1.5, w + 3, BAND_H + 3, 4);
        ctx.stroke();
      }
    }
    ctx.restore();
  }

  // An in-flight record has a start and nothing else. It gets a marker, never a
  // bar: a bar would state an end time the log does not have.
  _drawInFlight(ctx, x, top, color, hit) {
    if (hit) {
      ctx.fillStyle = color;
      ctx.fillRect(x - 3, top, 12, BAND_H);
      return;
    }
    ctx.save();
    ctx.fillStyle = color;
    ctx.fillRect(x, top, 2, BAND_H);
    ctx.beginPath();
    ctx.moveTo(x + 3, top + 1);
    ctx.lineTo(x + 9, top + BAND_H / 2);
    ctx.lineTo(x + 3, top + BAND_H - 1);
    ctx.closePath();
    ctx.fill();
    ctx.restore();
  }

  _roundRect(ctx, x, y, w, h, r) {
    if (typeof ctx.roundRect === 'function') { ctx.roundRect(x, y, w, h, r); return; }
    ctx.rect(x, y, w, h);
  }

  _indexColor(i) {
    const v = i + 1;
    return `rgb(${(v >> 16) & 255},${(v >> 8) & 255},${v & 255})`;
  }

  _renderAxis(width) {
    this._axis.textContent = '';
    if (this._scale === 'equal') {
      const note = document.createElement('span');
      note.className = 'tf-rt__axis-note';
      note.textContent = ti('axis_equal', null, 'equal spacing — the axis is not linear');
      this._axis.appendChild(note);
      return;
    }
    const { t0, t1 } = this.range;
    const span = Math.max(1, t1 - t0);
    const target = Math.max(3, Math.floor(width / 130));
    const raw = span / target;
    let step = TICK_STEPS_MS[TICK_STEPS_MS.length - 1];
    for (const s of TICK_STEPS_MS) { if (s >= raw) { step = s; break; } }
    const first = Math.ceil(t0 / step) * step;
    for (let t = first; t <= t1; t += step) {
      const px = ((t - t0) / span) * width;
      const tick = document.createElement('span');
      tick.className = 'tf-rt__tick';
      tick.style.left = `${(px / width) * 100}%`;
      // A centred label would hang outside the axis at either end, so the
      // edge ticks anchor to their own side instead of being dropped.
      if (px < 34) tick.style.transform = 'translateX(0)';
      else if (px > width - 34) tick.style.transform = 'translateX(-100%)';
      tick.textContent = this._fmtStamp(t);
      this._axis.appendChild(tick);
    }
  }

  // The minimap always draws the WHOLE dataset on a real-time axis — it is the
  // frame of reference for the view window and must not follow the toggle.
  _renderMinimap() {
    const width = Math.max(1, this._minimap.clientWidth);
    const dpr = window.devicePixelRatio || 1;
    this._miniCanvas.width = Math.round(width * dpr);
    this._miniCanvas.height = Math.round(MINIMAP_H * dpr);
    this._miniCanvas.style.width = `${width}px`;
    this._miniCanvas.style.height = `${MINIMAP_H}px`;

    const ctx = this._miniCanvas.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, MINIMAP_H);

    const ext = this.extent;
    const full = Math.max(1, ext.t1 - ext.t0);
    const c = this._palette();
    ctx.globalAlpha = 0.7;
    for (const r of this._records) {
      const x = ((startOf(r) - ext.t0) / full) * width;
      const dur = durationOf(r);
      ctx.fillStyle = r.error ? c.error
        : dur === null ? c.inflight
          : laneIndex(r.lane) === 2 ? c.tool : c.decode;
      ctx.fillRect(x, 8, dur === null ? 2 : Math.max(1, (dur / full) * width), 6);
    }
    ctx.globalAlpha = 1;

    const view = this.range;
    this._miniWin.style.left = `${((view.t0 - ext.t0) / full) * 100}%`;
    this._miniWin.style.width = `${((view.t1 - view.t0) / full) * 100}%`;
  }

  _renderChrome(count) {
    this._countEl.textContent = ti('in_view', { count }, `${count} records in view`);
    const full = this._isFullExtent();
    this._rangeChip.hidden = full;
    if (!full) {
      const { t0, t1 } = this.range;
      this._rangeChip.setAttribute(
        'label',
        `${ti('range_label', null, 'range')}: ${this._fmtStamp(t0)}–${this._fmtStamp(t1)} ✕`,
      );
    }
  }

  // ---------------------------------------------------------------------------
  // Hit testing
  // ---------------------------------------------------------------------------

  _buildHitMap() {
    const width = Math.max(1, this._track.clientWidth);
    if (!this._hitCanvas) this._hitCanvas = document.createElement('canvas');
    this._hitCanvas.width = width;
    this._hitCanvas.height = TRACK_H;
    const ctx = this._hitCanvas.getContext('2d', { willReadFrequently: true });
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, width, TRACK_H);
    const list = this._visible();
    const order = new Map();
    list.forEach((r, i) => order.set(r.id, i));
    this._drawBands(ctx, list, order, width, true);
    this._hitData = ctx.getImageData(0, 0, width, TRACK_H).data;
    this._hitW = width;
    this._hitStale = false;
  }

  _hitTest(x, y) {
    if (this._hitStale || !this._hitData) this._buildHitMap();
    const px = Math.floor(x);
    const py = Math.floor(y);
    if (px < 0 || py < 0 || px >= this._hitW || py >= TRACK_H) return null;
    const o = (py * this._hitW + px) * 4;
    const d = this._hitData;
    if (d[o + 3] === 0) return null;
    const v = (d[o] << 16) | (d[o + 1] << 8) | d[o + 2];
    if (v <= 0) return null;
    return this._hitIndex[v - 1] || null;
  }

  // ---------------------------------------------------------------------------
  // Interaction
  // ---------------------------------------------------------------------------

  _fracOf(e, el) {
    const rect = el.getBoundingClientRect();
    return (e.clientX - rect.left) / Math.max(1, rect.width);
  }

  _emitRange() {
    const { t0, t1 } = this.range;
    this.dispatchEvent(new CustomEvent('range-change', { detail: { t0, t1 }, bubbles: true }));
  }

  _setHot(id, e) {
    if (id === this._hot) { if (id && e) this._moveTip(e, id); return; }
    this._hot = id;
    this._invalidate(false);
    if (id && e) this._showTip(e, id); else this._hideTip();
    this.dispatchEvent(new CustomEvent('record-hover', { detail: { id }, bubbles: true }));
  }

  // Zoom keeps the timestamp under the cursor under the cursor: the anchor is
  // read before the span changes and the new window is laid out around it.
  _onWheel(e) {
    e.preventDefault();
    const ext = this.extent;
    const view = this.range;
    const frac = clamp(this._fracOf(e, this._track), 0, 1);
    const anchor = view.t0 + frac * (view.t1 - view.t0);
    const factor = e.deltaY > 0 ? ZOOM_OUT : ZOOM_IN;
    const span = Math.max(MIN_SPAN_MS, Math.min(ext.t1 - ext.t0, (view.t1 - view.t0) * factor));
    let t0 = Math.max(ext.t0, anchor - frac * span);
    const t1 = Math.min(ext.t1, t0 + span);
    t0 = Math.max(ext.t0, t1 - span);
    this._range = { t0, t1 };
    this._invalidate();
    this._emitRange();
  }

  _onContextMenu(e) { e.preventDefault(); }

  _onDblClick() {
    this._range = null;
    this._drag = null;
    this._brush.hidden = true;
    this._invalidate();
    this._emitRange();
  }

  _onPointerDown(e) {
    const view = this.range;
    const frac = this._fracOf(e, this._track);
    this._drag = {
      mode: e.button === 2 ? 'pan' : 'brush',
      startFrac: frac,
      t0: view.t0,
      t1: view.t1,
      moved: false,
    };
    if (this._drag.mode === 'brush') {
      this._brush.hidden = false;
      this._brush.style.left = `${frac * 100}%`;
      this._brush.style.width = '0%';
    } else {
      this._track.classList.add('tf-rt__track--panning');
    }
    this._track.setPointerCapture(e.pointerId);
    this._hideTip();
  }

  _onPointerMove(e) {
    const frac = this._fracOf(e, this._track);
    if (!this._drag) {
      const rect = this._track.getBoundingClientRect();
      const rec = this._hitTest(e.clientX - rect.left, e.clientY - rect.top);
      this._setHot(rec ? rec.id : null, e);
      return;
    }
    this._drag.moved = true;
    if (this._drag.mode === 'brush') {
      const a = Math.min(this._drag.startFrac, frac);
      const b = Math.max(this._drag.startFrac, frac);
      this._brush.style.left = `${a * 100}%`;
      this._brush.style.width = `${(b - a) * 100}%`;
      this._drag.endFrac = frac;
      return;
    }
    const ext = this.extent;
    const span = this._drag.t1 - this._drag.t0;
    const shift = (this._drag.startFrac - frac) * span;
    let t0 = this._drag.t0 + shift;
    let t1 = this._drag.t1 + shift;
    if (t0 < ext.t0) { t1 += ext.t0 - t0; t0 = ext.t0; }
    if (t1 > ext.t1) { t0 -= t1 - ext.t1; t1 = ext.t1; }
    this._range = { t0, t1 };
    this._invalidate();
    this._emitRange();
  }

  _onPointerUp(e) {
    const drag = this._drag;
    this._drag = null;
    this._track.classList.remove('tf-rt__track--panning');
    this._brush.hidden = true;
    if (!drag) return;
    if (drag.mode !== 'brush') return;

    const endFrac = drag.endFrac ?? drag.startFrac;
    const width = Math.abs(endFrac - drag.startFrac);
    if (width > BRUSH_MIN_FRAC) {
      const span = drag.t1 - drag.t0;
      const a = drag.t0 + Math.min(drag.startFrac, endFrac) * span;
      const b = drag.t0 + Math.max(drag.startFrac, endFrac) * span;
      this._range = { t0: a, t1: Math.max(a + 1, b) };
      this._invalidate();
      this._emitRange();
      return;
    }
    // Below the threshold the gesture was a click, so it selects a band.
    const rect = this._track.getBoundingClientRect();
    const rec = this._hitTest(e.clientX - rect.left, e.clientY - rect.top);
    if (!rec) return;
    this._selected = rec.id;
    this._invalidate(false);
    this.dispatchEvent(new CustomEvent('record-select', { detail: { id: rec.id }, bubbles: true }));
  }

  _onPointerLeave() {
    if (this._drag) return;
    this._setHot(null, null);
  }

  _onMiniDown(e) {
    this._miniDrag = true;
    this._minimap.setPointerCapture(e.pointerId);
    this._recentre(e);
  }

  _onMiniMove(e) { if (this._miniDrag) this._recentre(e); }

  _onMiniUp() { this._miniDrag = false; }

  // Clicking the minimap moves the view window there and keeps its span.
  _recentre(e) {
    const ext = this.extent;
    const view = this.range;
    const span = view.t1 - view.t0;
    const frac = clamp(this._fracOf(e, this._minimap), 0, 1);
    const centre = ext.t0 + frac * (ext.t1 - ext.t0);
    let t0 = Math.max(ext.t0, centre - span / 2);
    const t1 = Math.min(ext.t1, t0 + span);
    t0 = Math.max(ext.t0, t1 - span);
    this._range = { t0, t1 };
    this._invalidate();
    this._emitRange();
  }

  // ---------------------------------------------------------------------------
  // Tooltip
  // ---------------------------------------------------------------------------

  _showTip(e, id) {
    const r = this._records.find((x) => x.id === id);
    if (!r) { this._hideTip(); return; }
    const dur = durationOf(r);
    const lines = [r.name || r.kind || r.id];
    lines.push(dur === null
      ? ti('tip_inflight', null, 'in flight — no end recorded')
      : this._fmtDuration(dur));
    const ttft = Number(r.ttft);
    if (dur !== null && Number.isFinite(ttft) && ttft > 0) {
      lines.push(`${ti('tip_ttft', null, 'TTFT')} ${this._fmtDuration(ttft)} · ${ti('tip_decode', null, 'decoding')} ${this._fmtDuration(Math.max(0, dur - ttft))}`);
    }
    lines.push(this._fmtStamp(startOf(r)));
    if (r.turn != null) lines.push(ti('tip_turn', { turn: r.turn }, `turn ${r.turn}`));
    this._tip.textContent = '';
    for (const line of lines) {
      const div = document.createElement('div');
      div.textContent = line;
      this._tip.appendChild(div);
    }
    this._tip.hidden = false;
    this._moveTip(e, id);
  }

  _moveTip(e) {
    if (this._tip.hidden) return;
    const track = this._track.getBoundingClientRect();
    const root = this._root.getBoundingClientRect();
    const x = e.clientX - root.left;
    const flip = x > root.width - 220;
    this._tip.style.top = `${track.top - root.top + 6}px`;
    this._tip.style.left = flip ? 'auto' : `${x + 14}px`;
    this._tip.style.right = flip ? `${root.width - x + 14}px` : 'auto';
  }

  _hideTip() {
    this._tip.hidden = true;
    this._tip.style.left = 'auto';
    this._tip.style.right = 'auto';
  }
}

if (!customElements.get('tf-run-timeline')) {
  customElements.define('tf-run-timeline', TfRunTimeline);
}
