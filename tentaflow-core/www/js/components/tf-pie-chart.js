// =============================================================================
// Plik: components/tf-pie-chart.js
// Opis: Wykres kołowy/donut SVG (PieChart 0x0219). Slices jako <path>
// (annulus dla donut), agregacja "Other" powyżej maxSegments, etykiety %
// na slice'ach i legenda. Light DOM, klasy .tf-pie-chart__* z controls.css.
//
// Kontrakt danych (property `slices`):
//   Array<{ id: string|null, label: string, value: number (>0, skończona),
//           tone: string|null }>
// Properties: `variant`: 'pie'|'donut'; `showLabels`, `showLegend`: boolean;
// `maxSegments`: number (>0 — nadmiar agregowany w "Other");
// `height`: number px.
// =============================================================================

const SVG_NS = 'http://www.w3.org/2000/svg';
// Tone cycle for slices without an explicit tone.
const TONE_CYCLE = ['primary', 'success', 'warning', 'critical', 'info', 'muted', 'neutral'];
const SLICE_LABEL_THRESHOLD = 0.03;  // slices under 3% carry no on-slice text

class TfPieChart extends HTMLElement {
  constructor() {
    super();
    this._slices = [];
    this._variant = 'pie';
    this._showLabels = false;
    this._showLegend = false;
    this._maxSegments = 255;
    this._height = 200;
    this._appliedClasses = [];
    this._plot = null;
    this._ro = null;
  }

  set slices(value) { this._slices = Array.isArray(value) ? value : []; this._render(); }
  set variant(value) { this._variant = value === 'donut' ? 'donut' : 'pie'; this._render(); }
  set showLabels(value) { this._showLabels = Boolean(value); this._render(); }
  set showLegend(value) { this._showLegend = Boolean(value); this._render(); }
  set maxSegments(value) { const n = Number(value); if (Number.isInteger(n) && n > 0) this._maxSegments = n; this._render(); }
  set height(value) { const n = Number(value); if (Number.isFinite(n) && n > 0) this._height = n; this._render(); }

  connectedCallback() {
    if (typeof globalThis.ResizeObserver === 'function' && !this._ro) {
      this._ro = new globalThis.ResizeObserver(() => this._render());
      if (this._plot) this._ro.observe(this._plot);
    }
    if (!this._plot) this._render();
  }

  disconnectedCallback() {
    if (this._ro) { this._ro.disconnect(); this._ro = null; }
  }

  _render() {
    for (const c of this._appliedClasses) this.classList.remove(c);
    const classes = ['tf-chart', 'tf-pie-chart', `tf-pie-chart--variant-${this._variant}`];
    for (const c of classes) this.classList.add(c);
    this._appliedClasses = classes;
    this.style.height = `${this._height}px`;

    const layout = document.createElement('div');
    layout.classList.add('tf-pie-chart__layout');

    this._plot = document.createElement('div');
    this._plot.classList.add('tf-pie-chart__plot');
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('width', '100%');
    svg.setAttribute('height', '100%');
    svg.setAttribute('role', 'img');
    svg.setAttribute('aria-label', this._variant === 'donut' ? 'Donut chart' : 'Pie chart');
    svg.classList.add('tf-pie-chart__svg');
    this._plot.appendChild(svg);
    layout.appendChild(this._plot);

    let legendEl = null;
    if (this._showLegend) {
      legendEl = document.createElement('ul');
      legendEl.classList.add('tf-pie-chart__legend');
      legendEl.setAttribute('role', 'list');
      layout.appendChild(legendEl);
    }

    this.replaceChildren(layout);
    if (this._ro) { this._ro.disconnect(); this._ro.observe(this._plot); }

    const rect = this._plot.getBoundingClientRect ? this._plot.getBoundingClientRect() : null;
    // happy-dom returns width=0 for unmounted nodes; fall back to a square box.
    const w = (rect && rect.width > 0) ? rect.width : this._height;
    const h = (rect && rect.height > 0) ? rect.height : this._height;
    svg.setAttribute('viewBox', `0 0 ${w} ${h}`);
    const cx = w / 2;
    const cy = h / 2;
    const radius = Math.min(w, h) / 2 * 0.85;
    const innerRadius = this._variant === 'donut' ? radius * 0.55 : 0;

    let slices = this._slices.filter((s) =>
      s != null && typeof s.value === 'number' && Number.isFinite(s.value) && s.value > 0);
    if (slices.length === 0) return;
    // maxSegments aggregation: keep the first (max-1), the rest become "Other".
    if (slices.length > this._maxSegments) {
      const kept = slices.slice(0, this._maxSegments - 1);
      const rest = slices.slice(this._maxSegments - 1);
      const restValue = rest.reduce((acc, s) => acc + s.value, 0);
      kept.push({ id: '__other__', label: 'Other', value: restValue, tone: 'muted' });
      slices = kept;
    }
    const total = slices.reduce((acc, s) => acc + s.value, 0);
    if (total <= 0) return;

    let startAngle = -Math.PI / 2;  // 12 o'clock
    for (let i = 0; i < slices.length; i++) {
      const slice = slices[i];
      const fraction = slice.value / total;
      const sweepAngle = fraction * Math.PI * 2;
      const endAngle = startAngle + sweepAngle;
      const tone = slice.tone || TONE_CYCLE[i % TONE_CYCLE.length];

      // Pie path: M cx cy → L outer_start → A outer_arc → Z.
      // Donut: M outer_start → A outer_arc → L inner_end → A inner_arc → Z.
      const ox0 = cx + Math.cos(startAngle) * radius;
      const oy0 = cy + Math.sin(startAngle) * radius;
      const ox1 = cx + Math.cos(endAngle) * radius;
      const oy1 = cy + Math.sin(endAngle) * radius;
      const largeArc = sweepAngle > Math.PI ? 1 : 0;
      let d;
      if (innerRadius > 0) {
        const ix0 = cx + Math.cos(endAngle) * innerRadius;
        const iy0 = cy + Math.sin(endAngle) * innerRadius;
        const ix1 = cx + Math.cos(startAngle) * innerRadius;
        const iy1 = cy + Math.sin(startAngle) * innerRadius;
        d = [
          `M ${ox0} ${oy0}`,
          `A ${radius} ${radius} 0 ${largeArc} 1 ${ox1} ${oy1}`,
          `L ${ix0} ${iy0}`,
          `A ${innerRadius} ${innerRadius} 0 ${largeArc} 0 ${ix1} ${iy1}`,
          'Z',
        ].join(' ');
      } else {
        d = [
          `M ${cx} ${cy}`,
          `L ${ox0} ${oy0}`,
          `A ${radius} ${radius} 0 ${largeArc} 1 ${ox1} ${oy1}`,
          'Z',
        ].join(' ');
      }
      const a11yLabel = `${slice.label}: ${slice.value} (${(fraction * 100).toFixed(1)}%)`;
      // Single 100% slice: the arc cannot close on itself (start == end) —
      // render a circle for pie, circle + hole for donut.
      if (slices.length === 1 || fraction >= 0.9999) {
        const circle = document.createElementNS(SVG_NS, 'circle');
        circle.setAttribute('cx', String(cx));
        circle.setAttribute('cy', String(cy));
        circle.setAttribute('r', String(radius));
        circle.classList.add('tf-pie-chart__slice');
        circle.classList.add(`tf-pie-chart__slice--tone-${tone}`);
        circle.setAttribute('data-slice-id', slice.id || slice.label);
        circle.setAttribute('data-value', String(slice.value));
        circle.setAttribute('aria-label', a11yLabel);
        svg.appendChild(circle);
        if (innerRadius > 0) {
          const hole = document.createElementNS(SVG_NS, 'circle');
          hole.setAttribute('cx', String(cx));
          hole.setAttribute('cy', String(cy));
          hole.setAttribute('r', String(innerRadius));
          hole.classList.add('tf-pie-chart__hole');
          svg.appendChild(hole);
        }
      } else {
        const path = document.createElementNS(SVG_NS, 'path');
        path.setAttribute('d', d);
        path.classList.add('tf-pie-chart__slice');
        path.classList.add(`tf-pie-chart__slice--tone-${tone}`);
        path.setAttribute('data-slice-id', slice.id || slice.label);
        path.setAttribute('data-value', String(slice.value));
        path.setAttribute('aria-label', a11yLabel);
        svg.appendChild(path);
      }

      if (this._showLabels && fraction >= SLICE_LABEL_THRESHOLD) {
        const midAngle = startAngle + sweepAngle / 2;
        const labelRadius = innerRadius > 0 ? (radius + innerRadius) / 2 : radius * 0.65;
        const lx = cx + Math.cos(midAngle) * labelRadius;
        const ly = cy + Math.sin(midAngle) * labelRadius;
        const text = document.createElementNS(SVG_NS, 'text');
        text.setAttribute('x', String(lx));
        text.setAttribute('y', String(ly));
        text.setAttribute('text-anchor', 'middle');
        text.setAttribute('dominant-baseline', 'middle');
        text.classList.add('tf-pie-chart__slice-label');
        text.textContent = `${(fraction * 100).toFixed(fraction >= 0.1 ? 0 : 1)}%`;
        svg.appendChild(text);
      }

      if (legendEl) {
        const li = document.createElement('li');
        li.classList.add('tf-pie-chart__legend-item');
        const sw = document.createElement('span');
        sw.classList.add('tf-pie-chart__legend-swatch');
        sw.classList.add(`tf-pie-chart__legend-swatch--tone-${tone}`);
        li.appendChild(sw);
        const labelEl = document.createElement('span');
        labelEl.classList.add('tf-pie-chart__legend-label');
        labelEl.textContent = slice.label;
        li.appendChild(labelEl);
        const valEl = document.createElement('span');
        valEl.classList.add('tf-pie-chart__legend-value');
        valEl.textContent = `${slice.value} (${(fraction * 100).toFixed(1)}%)`;
        li.appendChild(valEl);
        legendEl.appendChild(li);
      }

      startAngle = endAngle;
    }
  }
}

if (!customElements.get('tf-pie-chart')) {
  customElements.define('tf-pie-chart', TfPieChart);
}

export { TfPieChart };
