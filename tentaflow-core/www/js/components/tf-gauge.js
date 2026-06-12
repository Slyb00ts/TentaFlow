// =============================================================================
// File: components/tf-gauge.js
// Description: Radial gauge (circular/arc/semi) with threshold ticks and a
// centered value readout. Display-only. Attributes: value, min, max, variant,
// label, size, display-value. Property `thresholds` takes
// Array<{ value: number, tone: string, label: string|null }>.
// Absent `value` renders the empty state (muted, em dash); a non-finite
// `value` renders the invalid state (critical tone + aria-invalid).
// =============================================================================

const SVG_NS = 'http://www.w3.org/2000/svg';
const GAUGE_VARIANTS = new Set(['circular', 'arc', 'semi']);

function arcSpan(variant) {
  if (variant === 'circular') return Math.PI * 2;
  if (variant === 'arc') return Math.PI * 1.5;
  return Math.PI;
}

function arcStart(variant) {
  if (variant === 'circular') return -Math.PI / 2;
  if (variant === 'arc') return Math.PI * 0.75;
  return Math.PI;
}

function describeArc(cx, cy, r, startAngle, endAngle) {
  const sweepAngle = endAngle - startAngle;
  if (Math.abs(sweepAngle) < 1e-6) {
    const x = cx + r * Math.cos(startAngle);
    const y = cy + r * Math.sin(startAngle);
    return `M ${x} ${y}`;
  }
  // A full circle cannot be expressed as a single SVG arc — split in two.
  if (Math.abs(sweepAngle) >= Math.PI * 2 - 1e-6) {
    const midAngle = startAngle + Math.PI;
    const x0 = cx + r * Math.cos(startAngle);
    const y0 = cy + r * Math.sin(startAngle);
    const xMid = cx + r * Math.cos(midAngle);
    const yMid = cy + r * Math.sin(midAngle);
    return `M ${x0} ${y0} A ${r} ${r} 0 1 1 ${xMid} ${yMid} A ${r} ${r} 0 1 1 ${x0} ${y0}`;
  }
  const x0 = cx + r * Math.cos(startAngle);
  const y0 = cy + r * Math.sin(startAngle);
  const x1 = cx + r * Math.cos(endAngle);
  const y1 = cy + r * Math.sin(endAngle);
  const largeArc = Math.abs(sweepAngle) > Math.PI ? 1 : 0;
  const sweepFlag = sweepAngle > 0 ? 1 : 0;
  return `M ${x0} ${y0} A ${r} ${r} 0 ${largeArc} ${sweepFlag} ${x1} ${y1}`;
}

class TfGauge extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'min', 'max', 'variant', 'label', 'size', 'display-value'];
  }

  constructor() {
    super();
    this._thresholds = [];
    this._variantClass = null;
  }

  connectedCallback() {
    this._render();
  }

  attributeChangedCallback() {
    if (this.isConnected) this._render();
  }

  set thresholds(value) {
    this._thresholds = Array.isArray(value)
      ? value.filter((t) => t && Number.isFinite(t.value) && typeof t.tone === 'string')
      : [];
    if (this.isConnected) this._render();
  }

  get thresholds() {
    return this._thresholds;
  }

  _numAttr(name, fallback) {
    const raw = this.getAttribute(name);
    if (raw == null) return fallback;
    const n = Number(raw);
    return Number.isFinite(n) ? n : fallback;
  }

  _render() {
    const size = Math.max(1, this._numAttr('size', 160));
    const min = this._numAttr('min', 0);
    const max = this._numAttr('max', 100);
    const range = max - min;
    const variant = GAUGE_VARIANTS.has(this.getAttribute('variant'))
      ? this.getAttribute('variant')
      : 'circular';
    const label = this.getAttribute('label');

    this.classList.add('tf-gauge');
    const variantClass = `tf-gauge--variant-${variant}`;
    if (this._variantClass && this._variantClass !== variantClass) {
      this.classList.remove(this._variantClass);
    }
    this.classList.add(variantClass);
    this._variantClass = variantClass;
    this.style.width = `${size}px`;
    this.style.height = `${size}px`;

    // Tri-state value: absent attribute = empty, non-finite = invalid.
    const rawAttr = this.getAttribute('value');
    const hasValue = rawAttr != null;
    const num = hasValue ? Number(rawAttr) : null;
    const invalid = hasValue && !Number.isFinite(num);

    this.innerHTML = '';
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('viewBox', `0 0 ${size} ${size}`);
    svg.setAttribute('width', String(size));
    svg.setAttribute('height', String(size));
    svg.setAttribute('role', 'img');
    svg.classList.add('tf-gauge__svg');
    this.appendChild(svg);

    const cx = size / 2;
    const cy = size / 2;
    const radius = size * 0.4;
    const strokeW = size * 0.08;
    const span = arcSpan(variant);
    const start = arcStart(variant);

    const track = document.createElementNS(SVG_NS, 'path');
    track.setAttribute('d', describeArc(cx, cy, radius, start, start + span));
    track.setAttribute('fill', 'none');
    track.setAttribute('stroke-width', String(strokeW));
    track.classList.add('tf-gauge__track');
    svg.appendChild(track);

    const valueArc = document.createElementNS(SVG_NS, 'path');
    valueArc.setAttribute('fill', 'none');
    valueArc.setAttribute('stroke-width', String(strokeW));
    svg.appendChild(valueArc);

    for (const th of this._thresholds) {
      const ratio = range > 0 ? (th.value - min) / range : -1;
      if (ratio < 0 || ratio > 1) continue;
      const angle = start + span * ratio;
      const inner = radius - strokeW * 0.6;
      const outer = radius + strokeW * 0.6;
      const tick = document.createElementNS(SVG_NS, 'line');
      tick.setAttribute('x1', String(cx + Math.cos(angle) * inner));
      tick.setAttribute('y1', String(cy + Math.sin(angle) * inner));
      tick.setAttribute('x2', String(cx + Math.cos(angle) * outer));
      tick.setAttribute('y2', String(cy + Math.sin(angle) * outer));
      tick.classList.add('tf-gauge__threshold');
      tick.classList.add(`tf-gauge__threshold--tone-${th.tone}`);
      if (th.label != null) {
        const title = document.createElementNS(SVG_NS, 'title');
        title.textContent = String(th.label);
        tick.setAttribute('aria-label', title.textContent);
        tick.appendChild(title);
      }
      svg.appendChild(tick);
    }

    const valueText = document.createElementNS(SVG_NS, 'text');
    valueText.setAttribute('x', String(cx));
    valueText.setAttribute('y', String(cy));
    valueText.setAttribute('text-anchor', 'middle');
    valueText.setAttribute('dominant-baseline', 'middle');
    valueText.classList.add('tf-gauge__value-text');
    svg.appendChild(valueText);

    if (label != null) {
      const labelText = document.createElementNS(SVG_NS, 'text');
      labelText.setAttribute('x', String(cx));
      labelText.setAttribute('y', String(cy + size * 0.12));
      labelText.setAttribute('text-anchor', 'middle');
      labelText.classList.add('tf-gauge__label');
      labelText.textContent = label;
      svg.appendChild(labelText);
    }

    if (!hasValue || invalid) {
      valueArc.setAttribute('d', describeArc(cx, cy, radius, start, start));
      const tone = invalid ? 'critical' : 'muted';
      valueArc.setAttribute('class', `tf-gauge__value-arc tf-gauge__value-arc--tone-${tone}`);
      valueText.textContent = '—';
      svg.setAttribute('aria-label', `— (${min}-${max})`);
      if (invalid) svg.setAttribute('aria-invalid', 'true');
      return;
    }

    const clamped = Math.max(min, Math.min(max, num));
    const ratio = range > 0 ? (clamped - min) / range : 0;
    valueArc.setAttribute('d', describeArc(cx, cy, radius, start, start + span * ratio));
    let tone = 'primary';
    for (const th of this._thresholds) {
      if (clamped >= th.value) tone = th.tone;
    }
    valueArc.setAttribute('class', `tf-gauge__value-arc tf-gauge__value-arc--tone-${tone}`);
    const display = this.getAttribute('display-value');
    valueText.textContent = display != null ? display : String(clamped);
    svg.setAttribute('aria-label', `${valueText.textContent} (${min}-${max})`);
  }
}

if (!customElements.get('tf-gauge')) {
  customElements.define('tf-gauge', TfGauge);
}

export { TfGauge };
