// =============================================================================
// File: components/tf-density-plot.js
// Description: <tf-density-plot> — the density matrix ρ of plan §13.6
//              (mockup Q15 "Macierz gęstości"): the real or the imaginary part
//              of a Hermitian matrix, as a HEAT grid or as the classic CITY
//              plot of isometric bars.
//
//              The two views answer different questions and both are needed:
//              the grid says WHERE the coherences are (the off-diagonal corners
//              that separate a superposition from a classical mixture), the
//              city says HOW BIG they are against the populations on the
//              diagonal. Negative entries are a real feature of ρ's imaginary
//              part, so the scale is diverging and a negative bar grows
//              DOWNWARD instead of being drawn as its own absolute value.
//
//              The element is fed a matrix, never a state: a two-qubit pair
//              matrix out of a keyframe and the full ρ of a small register are
//              the same picture, and the caller decides which it has (§13.6
//              draws the full matrix up to 6 qubits and pairs above).
//
//  Properties: matrix — {dim, rho, labels?}, labels — i18n dict.
//  Attributes: part ("re" | "im"), mode ("heat" | "city"), size.
//
// Example: plot.matrix = { dim: 4, rho: pair.rho, labels: ['00','01','10','11'] };
// =============================================================================

const SVG_NS = 'http://www.w3.org/2000/svg';
const DEFAULT_SIZE = 260;

const DEFAULT_LABELS = {
  density: 'Density matrix',
  empty: 'no matrix',
  real: 'Re',
  imaginary: 'Im',
};

// ---------------------------------------------------------------------------
// Reading the matrix — pure
// ---------------------------------------------------------------------------

/// The wire carries a matrix as `Vec<[f64; 2]>`; a simulator hands back the
/// same numbers already flattened. Both are read here so no caller reshapes.
export function complexEntries(rho) {
  const list = Array.from(rho || []);
  if (!list.length) return [];
  if (Array.isArray(list[0]) || ArrayBuffer.isView(list[0])) {
    return list.map((cell) => [Number(cell[0]) || 0, Number(cell[1]) || 0]);
  }
  const out = [];
  for (let i = 0; i + 1 < list.length; i += 2) out.push([Number(list[i]) || 0, Number(list[i + 1]) || 0]);
  return out;
}

/// One cell per entry of the square matrix, with the requested part. A matrix
/// whose entry count is not a perfect square is not a matrix and answers empty
/// rather than being padded into one.
export function densityCells(rho, part = 're', dim = 0) {
  const entries = complexEntries(rho);
  const size = Number(dim) > 0 ? Math.floor(Number(dim)) : Math.round(Math.sqrt(entries.length));
  if (!size || size * size !== entries.length) return { dim: 0, cells: [], peak: 0 };
  const index = part === 'im' ? 1 : 0;
  const cells = [];
  let peak = 0;
  for (let row = 0; row < size; row += 1) {
    for (let col = 0; col < size; col += 1) {
      const value = entries[row * size + col][index];
      peak = Math.max(peak, Math.abs(value));
      cells.push({ row, col, value });
    }
  }
  return { dim: size, cells, peak };
}

/// Diverging fill for one entry: positive toward the accent, negative toward
/// the warm end, transparency carrying the magnitude. A zero entry is drawn as
/// the plot's own background, so the eye reads the pattern and not a grid.
export function densityColor(value, peak) {
  const top = Math.max(1e-12, Number(peak) || 0);
  const ratio = Math.max(-1, Math.min(1, (Number(value) || 0) / top));
  const alpha = Math.abs(ratio) * 0.82;
  if (alpha < 0.02) return 'transparent';
  return ratio >= 0
    ? `rgba(167, 139, 250, ${alpha.toFixed(3)})`
    : `rgba(244, 114, 182, ${alpha.toFixed(3)})`;
}

/// The default axis labels: the basis states of a register of `log2(dim)`
/// qubits, which is what every ρ this component is given is indexed by.
export function basisLabels(dim) {
  const size = Math.max(0, Number(dim) || 0);
  const bits = Math.max(1, Math.round(Math.log2(size || 1)));
  return Array.from({ length: size }, (_, i) => i.toString(2).padStart(bits, '0'));
}

/// Isometric placement of the city plot. `unit` is the footprint of one bar,
/// and a bar's height is its value against the peak — signed, because ρ has
/// negative entries and a city of absolute values would hide them.
export function cityLayout(cells, dim, { size = DEFAULT_SIZE, peak = 1 } = {}) {
  const n = Math.max(1, Number(dim) || 1);
  const unit = (size * 0.62) / n;
  const tall = size * 0.34;
  const originX = size / 2;
  const originY = size * 0.72;
  const top = Math.max(1e-12, Number(peak) || 0);
  const iso = (row, col, lift) => ({
    x: originX + (col - row) * unit * 0.5,
    y: originY + (col + row - n + 1) * unit * 0.25 - lift,
  });
  return (cells || []).map((cell) => {
    const height = (cell.value / top) * tall;
    return { ...cell, unit, height, base: iso(cell.row, cell.col, 0), apex: iso(cell.row, cell.col, height) };
  })
    // Painter's order: the far corner of the grid first, so a tall bar in
    // front covers the ones behind it rather than the other way around.
    .sort((a, b) => (a.row + a.col) - (b.row + b.col) || a.col - b.col);
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

class TfDensityPlot extends HTMLElement {
  static get observedAttributes() {
    return ['part', 'mode', 'size'];
  }

  constructor() {
    super();
    this._matrix = null;
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

  get matrix() { return this._matrix; }

  set matrix(value) {
    this._matrix = value || null;
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
    this.classList.add('tf-density');
    this._body = document.createElement('div');
    this._body.className = 'tf-density__body';
    this._scale = document.createElement('div');
    this._scale.className = 'tf-density__scale';
    this.replaceChildren(this._body, this._scale);
  }

  _size() {
    const value = Number(this.getAttribute('size'));
    return Number.isFinite(value) && value > 80 ? value : DEFAULT_SIZE;
  }

  _render() {
    const source = this._matrix || {};
    const part = this.getAttribute('part') === 'im' ? 'im' : 're';
    const { dim, cells, peak } = densityCells(source.rho, part, source.dim);
    this._body.replaceChildren();
    if (!dim) {
      this._body.classList.add('is-empty');
      this._body.textContent = this._labels.empty;
      this._scale.replaceChildren();
      this.setAttribute('aria-label', `${this._labels.density}: ${this._labels.empty}`);
      return;
    }
    this._body.classList.remove('is-empty');
    const labels = Array.isArray(source.labels) && source.labels.length === dim
      ? source.labels.map(String)
      : basisLabels(dim);
    if (this.getAttribute('mode') === 'city') this._body.appendChild(this._city(cells, dim, peak, labels));
    else this._body.appendChild(this._heat(cells, dim, peak, labels));
    this._paintScale(peak, part);
    this.setAttribute('role', 'img');
    this.setAttribute('aria-label', `${this._labels.density} ${part === 'im' ? this._labels.imaginary : this._labels.real}`
      + `: ${cells.filter((c) => Math.abs(c.value) > 1e-6)
        .map((c) => `${labels[c.row]},${labels[c.col]} ${c.value.toFixed(3)}`).join('; ') || '0'}`);
  }

  _heat(cells, dim, peak, labels) {
    const wrap = document.createElement('div');
    wrap.className = 'tf-density__heat';
    wrap.style.setProperty('--tf-density-dim', String(dim));
    const corner = document.createElement('span');
    corner.className = 'tf-density__corner';
    wrap.appendChild(corner);
    for (const label of labels) {
      const head = document.createElement('span');
      head.className = 'tf-density__col-label';
      head.textContent = label;
      wrap.appendChild(head);
    }
    for (let row = 0; row < dim; row += 1) {
      const head = document.createElement('span');
      head.className = 'tf-density__row-label';
      head.textContent = labels[row];
      wrap.appendChild(head);
      for (let col = 0; col < dim; col += 1) {
        const cell = cells[row * dim + col];
        const box = document.createElement('span');
        box.className = 'tf-density__cell';
        box.style.background = densityColor(cell.value, peak);
        box.textContent = Math.abs(cell.value) < 5e-3 ? '0' : cell.value.toFixed(2);
        box.title = `ρ[${labels[row]}, ${labels[col]}] = ${cell.value.toFixed(4)}`;
        wrap.appendChild(box);
      }
    }
    return wrap;
  }

  _city(cells, dim, peak, labels) {
    const size = this._size();
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('class', 'tf-density__city');
    svg.setAttribute('viewBox', `0 0 ${size} ${size}`);
    svg.setAttribute('width', String(size));
    svg.setAttribute('height', String(size));
    for (const bar of cityLayout(cells, dim, { size, peak })) {
      if (Math.abs(bar.value) < 1e-6) continue;
      const half = bar.unit * 0.5;
      const quarter = bar.unit * 0.25;
      const fill = densityColor(bar.value, peak);
      const group = document.createElementNS(SVG_NS, 'g');
      group.setAttribute('class', 'tf-density__bar');
      const face = (points, klass) => {
        const polygon = document.createElementNS(SVG_NS, 'polygon');
        polygon.setAttribute('class', klass);
        polygon.setAttribute('points', points.map(([x, y]) => `${x.toFixed(2)},${y.toFixed(2)}`).join(' '));
        polygon.setAttribute('fill', fill);
        return polygon;
      };
      const { base, apex } = bar;
      group.appendChild(face([
        [apex.x, apex.y - quarter], [apex.x + half, apex.y], [apex.x, apex.y + quarter], [apex.x - half, apex.y],
      ], 'tf-density__face tf-density__face--top'));
      group.appendChild(face([
        [apex.x - half, apex.y], [apex.x, apex.y + quarter], [base.x, base.y + quarter], [base.x - half, base.y],
      ], 'tf-density__face tf-density__face--left'));
      group.appendChild(face([
        [apex.x + half, apex.y], [apex.x, apex.y + quarter], [base.x, base.y + quarter], [base.x + half, base.y],
      ], 'tf-density__face tf-density__face--right'));
      const title = document.createElementNS(SVG_NS, 'title');
      title.textContent = `ρ[${labels[bar.row]}, ${labels[bar.col]}] = ${bar.value.toFixed(4)}`;
      group.appendChild(title);
      svg.appendChild(group);
    }
    return svg;
  }

  _paintScale(peak, part) {
    this._scale.replaceChildren();
    const low = document.createElement('span');
    low.textContent = `−${peak.toFixed(2)}`;
    const bar = document.createElement('i');
    const high = document.createElement('span');
    high.textContent = `+${peak.toFixed(2)}`;
    const name = document.createElement('span');
    name.className = 'tf-density__part';
    name.textContent = part === 'im' ? this._labels.imaginary : this._labels.real;
    this._scale.append(name, low, bar, high);
  }
}

if (!customElements.get('tf-density-plot')) {
  customElements.define('tf-density-plot', TfDensityPlot);
}

export { TfDensityPlot, DEFAULT_LABELS as DENSITY_LABELS };
