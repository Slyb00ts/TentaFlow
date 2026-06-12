// =============================================================================
// File: components/tf-rating.js
// Description: Display-only rating (stars/hearts/circles symbols or numeric
// text). Attributes: value, max, variant, precision, show-value. Absent
// `value` renders the unknown state (em dash); a non-finite `value` renders
// the invalid state (aria-invalid). No interactivity by design — the SDK
// RatingDisplay component defines no handlers.
// =============================================================================

const SVG_NS = 'http://www.w3.org/2000/svg';
const RATING_VARIANTS = new Set(['stars', 'hearts', 'circles', 'numeric']);
const RATING_PRECISIONS = new Set(['full', 'half', 'decimal']);

const RATING_PATHS = {
  stars: 'M12 2.5l2.95 6.55 7.05.65-5.3 4.85 1.55 6.95L12 17.85 5.75 21.5 7.3 14.55 2 9.7l7.05-.65L12 2.5z',
  hearts: 'M12 21s-7-4.35-7-10a4 4 0 0 1 7-2.65A4 4 0 0 1 19 11c0 5.65-7 10-7 10z',
  circles: 'M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16z',
};

class TfRating extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'max', 'variant', 'precision', 'show-value'];
  }

  constructor() {
    super();
    // Stable per-instance prefix keeps clipPath ids unique across instances.
    this._clipPrefix = `tf-rating-clip-${Math.random().toString(36).slice(2, 10)}`;
    this._modClasses = [];
  }

  connectedCallback() {
    this._render();
  }

  attributeChangedCallback() {
    if (this.isConnected) this._render();
  }

  _render() {
    const maxRaw = Number(this.getAttribute('max'));
    const max = Number.isInteger(maxRaw) && maxRaw > 0 ? maxRaw : 5;
    const variant = RATING_VARIANTS.has(this.getAttribute('variant'))
      ? this.getAttribute('variant')
      : 'stars';
    const precision = RATING_PRECISIONS.has(this.getAttribute('precision'))
      ? this.getAttribute('precision')
      : 'full';
    const showValue = this.hasAttribute('show-value');

    this.classList.add('tf-rating');
    for (const c of this._modClasses) this.classList.remove(c);
    this._modClasses = [`tf-rating--variant-${variant}`, `tf-rating--precision-${precision}`];
    for (const c of this._modClasses) this.classList.add(c);
    this.setAttribute('role', 'img');

    // Tri-state value: absent attribute = unknown, non-finite = invalid.
    const rawAttr = this.getAttribute('value');
    const hasValue = rawAttr != null;
    const num = hasValue ? Number(rawAttr) : null;
    const invalid = hasValue && !Number.isFinite(num);
    const clamped = hasValue && !invalid ? Math.max(0, Math.min(max, num)) : 0;

    if (invalid) this.setAttribute('aria-invalid', 'true');
    else this.removeAttribute('aria-invalid');

    let display;
    if (precision === 'full') display = Math.round(clamped);
    else if (precision === 'half') display = Math.round(clamped * 2) / 2;
    else display = clamped;
    const formatted = precision === 'decimal' ? clamped.toFixed(1) : String(display);

    this.innerHTML = '';

    if (variant === 'numeric') {
      const txt = document.createElement('span');
      txt.classList.add('tf-rating__numeric');
      txt.textContent = hasValue && !invalid ? `${formatted} / ${max}` : `— / ${max}`;
      this.appendChild(txt);
      const ariaText = invalid ? 'invalid rating'
        : !hasValue ? `unknown of ${max}`
        : `${formatted} of ${max}`;
      this.setAttribute('aria-label', ariaText);
      return;
    }

    const path = RATING_PATHS[variant];
    const iconsRoot = document.createElement('div');
    iconsRoot.classList.add('tf-rating__icons');
    this.appendChild(iconsRoot);

    for (let i = 0; i < max; i++) {
      const svg = document.createElementNS(SVG_NS, 'svg');
      svg.setAttribute('viewBox', '0 0 24 24');
      svg.setAttribute('class', `tf-rating__icon tf-rating__icon--${variant}`);
      const trackEl = document.createElementNS(SVG_NS, 'path');
      trackEl.setAttribute('d', path);
      trackEl.setAttribute('class', 'tf-rating__icon-track');
      svg.appendChild(trackEl);
      const defs = document.createElementNS(SVG_NS, 'defs');
      const clip = document.createElementNS(SVG_NS, 'clipPath');
      const clipId = `${this._clipPrefix}-${i}`;
      clip.setAttribute('id', clipId);
      const rect = document.createElementNS(SVG_NS, 'rect');
      rect.setAttribute('x', '0');
      rect.setAttribute('y', '0');
      rect.setAttribute('width', String(24 * Math.max(0, Math.min(1, display - i))));
      rect.setAttribute('height', '24');
      clip.appendChild(rect);
      defs.appendChild(clip);
      svg.appendChild(defs);
      const fillEl = document.createElementNS(SVG_NS, 'path');
      fillEl.setAttribute('d', path);
      fillEl.setAttribute('class', 'tf-rating__icon-fill');
      fillEl.setAttribute('clip-path', `url(#${clipId})`);
      svg.appendChild(fillEl);
      iconsRoot.appendChild(svg);
    }

    const ariaText = invalid ? 'invalid rating'
      : !hasValue ? `unknown of ${max}`
      : `${display} of ${max}`;
    this.setAttribute('aria-label', ariaText);

    if (showValue) {
      const valueLabel = document.createElement('span');
      valueLabel.classList.add('tf-rating__value');
      valueLabel.textContent = !hasValue || invalid
        ? '—'
        : (precision === 'decimal' ? clamped.toFixed(1) : String(display));
      this.appendChild(valueLabel);
    }
  }
}

if (!customElements.get('tf-rating')) {
  customElements.define('tf-rating', TfRating);
}

export { TfRating };
