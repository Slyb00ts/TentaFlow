// =============================================================================
// File: components/tf-shot-histogram.js
// Description: <tf-shot-histogram> — the measured distribution of a run
//              (plan §13.6; mockup Q15 "Histogram" and "Porównanie"): one group
//              of bars per bitstring, one bar per SERIES, 95 % Wilson whiskers
//              on every series that counted shots, and a logarithmic scale for
//              the tail a linear axis flattens to nothing.
//
//              A series is a distribution, not a picture: `{id, label, tone,
//              counts, probabilities, shots}`. `counts` is a map keyed by
//              bitstring; a series that carries exact probabilities instead
//              (the ideal distribution of the same circuit) has no shot total,
//              and therefore no whiskers — an error bar drawn on an exact
//              number would be a lie about where it came from.
//
//              The axis is the union of the series, heaviest first, capped at
//              `max-bars`; the metrics the host prints under the chart (TVD,
//              Hellinger fidelity) are computed over the FULL distributions by
//              the pure helpers here, never over the drawn window.
//
//  Properties: series — the distributions, labels — i18n dict.
//  Attributes: max-bars (default 16), log, whiskers="off", height.
//
// Example: hist.series = [{id: 'measured', label: 'QPU', counts, shots: 1024}];
// =============================================================================

/// z for a two-sided 95 % interval — the interval plan §13.6 names.
export const WILSON_Z = 1.959963984540054;

const DEFAULT_MAX_BARS = 16;
const DEFAULT_HEIGHT = 200;

const DEFAULT_LABELS = {
  histogram: 'Shot histogram',
  empty: 'no distribution',
  shots: 'shots',
  probability: 'probability',
  interval: '95% interval',
  more: '+{n} more states',
};

// ---------------------------------------------------------------------------
// Distributions — pure, and the only place these numbers are computed
// ---------------------------------------------------------------------------

/// One series as a probability map, whatever shape it arrived in. Counts are
/// normalised by the series' own shot total (its sum when it states none), so
/// two series of different shot counts still share one axis.
export function seriesProbabilities(series) {
  const out = new Map();
  if (!series) return out;
  const direct = series.probabilities;
  if (direct && typeof direct === 'object') {
    for (const [key, value] of Object.entries(direct)) {
      const p = Number(value);
      if (Number.isFinite(p) && p > 0) out.set(String(key), p);
    }
    return out;
  }
  const counts = series.counts || {};
  let total = Number(series.shots) || 0;
  if (!total) for (const value of Object.values(counts)) total += Number(value) || 0;
  if (!total) return out;
  for (const [key, value] of Object.entries(counts)) {
    const n = Number(value) || 0;
    if (n > 0) out.set(String(key), n / total);
  }
  return out;
}

/// Shots behind a series, or 0 for one that carries exact probabilities. It is
/// what decides whether a bar gets a whisker.
export function seriesShots(series) {
  if (!series) return 0;
  const stated = Number(series.shots) || 0;
  if (stated) return stated;
  if (series.probabilities) return 0;
  let total = 0;
  for (const value of Object.values(series.counts || {})) total += Number(value) || 0;
  return total;
}

/// The drawn axis: the union of every series' bitstrings, heaviest first (by
/// the largest probability any series gives it), capped and then put back in
/// lexical order so the axis reads the same between two repaints.
export function histogramAxis(list, limit = DEFAULT_MAX_BARS) {
  const peak = new Map();
  for (const series of list || []) {
    for (const [key, p] of seriesProbabilities(series)) {
      peak.set(key, Math.max(peak.get(key) || 0, p));
    }
  }
  const keys = Array.from(peak.keys())
    .sort((a, b) => peak.get(b) - peak.get(a) || a.localeCompare(b));
  const max = Math.max(1, Number(limit) || DEFAULT_MAX_BARS);
  const kept = keys.slice(0, max).sort((a, b) => a.localeCompare(b));
  return { bitstrings: kept, hidden: Math.max(0, keys.length - kept.length) };
}

/// Wilson score interval of `successes` out of `total` at `z` sigma. It is the
/// interval §13.6 asks for because the textbook normal interval collapses to
/// zero width exactly where a quantum histogram spends most of its bars —
/// p̂ = 0 and p̂ = 1 — and would draw a certainty the shots do not support.
export function wilsonInterval(successes, total, z = WILSON_Z) {
  const n = Math.max(0, Number(total) || 0);
  const k = Math.max(0, Math.min(n, Number(successes) || 0));
  if (!n) return { low: 0, high: 1, center: 0 };
  const p = k / n;
  const z2 = z * z;
  const denominator = 1 + z2 / n;
  const center = (p + z2 / (2 * n)) / denominator;
  const half = (z / denominator) * Math.sqrt((p * (1 - p)) / n + z2 / (4 * n * n));
  return { low: Math.max(0, center - half), high: Math.min(1, center + half), center };
}

/// Total variation distance ½Σ|p−q| over the UNION of the two distributions.
export function totalVariationDistance(a, b) {
  const left = a instanceof Map ? a : new Map(Object.entries(a || {}));
  const right = b instanceof Map ? b : new Map(Object.entries(b || {}));
  let sum = 0;
  for (const key of new Set([...left.keys(), ...right.keys()])) {
    sum += Math.abs((Number(left.get(key)) || 0) - (Number(right.get(key)) || 0));
  }
  return sum / 2;
}

/// Hellinger fidelity (Σ√(p·q))² — 1 for identical distributions, 0 for
/// disjoint ones. The same definition the node uses for `RunComparison`.
export function hellingerFidelity(a, b) {
  const left = a instanceof Map ? a : new Map(Object.entries(a || {}));
  const right = b instanceof Map ? b : new Map(Object.entries(b || {}));
  let sum = 0;
  for (const key of new Set([...left.keys(), ...right.keys()])) {
    sum += Math.sqrt(Math.max(0, Number(left.get(key)) || 0) * Math.max(0, Number(right.get(key)) || 0));
  }
  return Math.min(1, sum * sum);
}

/// Bar height in percent. The log scale is anchored at `floor` (a thousandth
/// of the peak by default) so a probability of zero has a bottom to stand on
/// instead of running off to minus infinity.
export function barHeight(probability, peak, { log = false, floor = 0 } = {}) {
  const p = Math.max(0, Number(probability) || 0);
  const top = Math.max(1e-12, Number(peak) || 0);
  if (!log) return Math.max(p > 0 ? 1.2 : 0, (p / top) * 100);
  const bottom = Math.max(1e-9, Number(floor) || top / 1000);
  if (p <= bottom) return p > 0 ? 1.2 : 0;
  const span = Math.log10(top / bottom);
  if (span <= 0) return 100;
  return Math.max(1.2, (Math.log10(p / bottom) / span) * 100);
}

/// Everything the element draws, as data: one group per bitstring, one bar per
/// series, with the whisker of a sampled series already in percent of the plot.
export function histogramLayout(list, { limit = DEFAULT_MAX_BARS, log = false, whiskers = true } = {}) {
  const series = (list || []).filter(Boolean);
  const { bitstrings, hidden } = histogramAxis(series, limit);
  const probabilities = series.map(seriesProbabilities);
  const shots = series.map(seriesShots);
  let peak = 0;
  for (const map of probabilities) for (const key of bitstrings) peak = Math.max(peak, map.get(key) || 0);
  const floor = peak / 1000;
  const groups = bitstrings.map((bitstring) => ({
    bitstring,
    bars: series.map((entry, i) => {
      const probability = probabilities[i].get(bitstring) || 0;
      const total = shots[i];
      const count = total ? Math.round(probability * total) : null;
      const bar = {
        seriesId: String(entry.id ?? i),
        label: String(entry.label ?? entry.id ?? i),
        tone: String(entry.tone || 'accent'),
        probability,
        count,
        height: barHeight(probability, peak, { log, floor }),
        whisker: null,
      };
      if (whiskers && total) {
        const interval = wilsonInterval(count, total);
        bar.whisker = {
          low: interval.low,
          high: interval.high,
          lowPercent: barHeight(interval.low, peak, { log, floor }),
          highPercent: barHeight(interval.high, peak, { log, floor }),
        };
      }
      return bar;
    }),
  }));
  return { groups, hidden, peak, series };
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfShotHistogram extends HTMLElement {
  static get observedAttributes() {
    return ['max-bars', 'log', 'whiskers', 'height'];
  }

  constructor() {
    super();
    this._series = [];
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

  get series() { return this._series; }

  set series(value) {
    this._series = Array.isArray(value) ? value : [];
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
    this.classList.add('tf-hist');
    this._scroll = document.createElement('div');
    this._scroll.className = 'tf-hist__scroll';
    this._plot = document.createElement('div');
    this._plot.className = 'tf-hist__plot';
    this._scroll.appendChild(this._plot);
    this._legend = document.createElement('div');
    this._legend.className = 'tf-hist__legend';
    this._foot = document.createElement('div');
    this._foot.className = 'tf-hist__foot';
    this.replaceChildren(this._scroll, this._legend, this._foot);
  }

  _number(name, fallback) {
    const value = Number(this.getAttribute(name));
    return Number.isFinite(value) && value > 0 ? value : fallback;
  }

  _render() {
    const layout = histogramLayout(this._series, {
      limit: this._number('max-bars', DEFAULT_MAX_BARS),
      log: this.hasAttribute('log'),
      whiskers: this.getAttribute('whiskers') !== 'off',
    });
    this._plot.style.setProperty('--tf-hist-height', `${this._number('height', DEFAULT_HEIGHT)}px`);
    this._plot.replaceChildren();
    if (!layout.groups.length) {
      this._plot.classList.add('is-empty');
      this._plot.textContent = this._labels.empty;
      this._legend.replaceChildren();
      this._foot.textContent = '';
      this.setAttribute('aria-label', `${this._labels.histogram}: ${this._labels.empty}`);
      return;
    }
    this._plot.classList.remove('is-empty');
    for (const group of layout.groups) {
      this._plot.appendChild(this._groupEl(group));
    }
    this._paintLegend(layout.series);
    this._foot.textContent = layout.hidden
      ? this._labels.more.replace('{n}', String(layout.hidden))
      : '';
    this.setAttribute('role', 'img');
    this.setAttribute('aria-label', `${this._labels.histogram}: ${layout.groups
      .map((g) => `${g.bitstring} ${g.bars.map((b) => `${(b.probability * 100).toFixed(1)}%`).join(' / ')}`)
      .join(', ')}`);
  }

  _groupEl(group) {
    const el = document.createElement('div');
    el.className = 'tf-hist__group';
    const bars = document.createElement('div');
    bars.className = 'tf-hist__bars';
    for (const bar of group.bars) {
      const column = document.createElement('div');
      column.className = `tf-hist__bar tf-hist__bar--${bar.tone}`;
      column.style.height = `${bar.height.toFixed(2)}%`;
      const parts = [
        `|${group.bitstring}⟩`, bar.label,
        `${this._labels.probability} ${(bar.probability * 100).toFixed(2)} %`,
      ];
      if (bar.count !== null) parts.push(`${bar.count} ${this._labels.shots}`);
      if (bar.whisker) {
        parts.push(`${this._labels.interval} ${(bar.whisker.low * 100).toFixed(1)}–${(bar.whisker.high * 100).toFixed(1)} %`);
        const whisker = document.createElement('span');
        whisker.className = 'tf-hist__whisker';
        // The whisker is positioned against the BAR, whose own height is the
        // centre of the interval: both ends therefore move with the scale.
        const height = Math.max(0.01, bar.height);
        whisker.style.bottom = `${((bar.whisker.lowPercent - height) / height * 100).toFixed(2)}%`;
        whisker.style.height = `${((bar.whisker.highPercent - bar.whisker.lowPercent) / height * 100).toFixed(2)}%`;
        column.appendChild(whisker);
      }
      column.title = parts.join(' · ');
      bars.appendChild(column);
    }
    const tick = document.createElement('span');
    tick.className = 'tf-hist__tick';
    tick.textContent = group.bitstring;
    el.append(bars, tick);
    return el;
  }

  _paintLegend(series) {
    this._legend.replaceChildren();
    if (series.length < 2) return;
    for (const entry of series) {
      const item = document.createElement('span');
      item.className = `tf-hist__key tf-hist__key--${String(entry.tone || 'accent')}`;
      item.textContent = String(entry.label ?? entry.id ?? '');
      this._legend.appendChild(item);
    }
  }
}

if (!customElements.get('tf-shot-histogram')) {
  customElements.define('tf-shot-histogram', TfShotHistogram);
}

export { TfShotHistogram, DEFAULT_LABELS as SHOT_HISTOGRAM_LABELS };
