// =============================================================================
// File: tf-skeleton.js
// Description: <tf-skeleton> — loading skeleton placeholder.
//   Attributes: variant (text|circle|rect), width, height, lines (for text).
// =============================================================================

class TfSkeleton extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'width', 'height', 'lines'];
  }

  constructor() {
    super();
    this._wrap = null;
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._update();
  }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-skeleton';
    this.appendChild(wrap);
    this._wrap = wrap;
  }

  _update() {
    const variant = this.getAttribute('variant') || 'rect';
    const width = this.getAttribute('width');
    const height = this.getAttribute('height');
    const lines = parseInt(this.getAttribute('lines') || '3', 10);

    this._wrap.innerHTML = '';
    this._wrap.classList.remove(
      'tf-skeleton-text', 'tf-skeleton-circle', 'tf-skeleton-rect'
    );

    if (variant === 'text') {
      this._wrap.classList.add('tf-skeleton-text');
      for (let i = 0; i < lines; i++) {
        const line = document.createElement('div');
        line.className = 'tf-skeleton-line';
        // Last line is shorter for visual rhythm.
        if (i === lines - 1 && lines > 1) line.style.width = '65%';
        this._wrap.appendChild(line);
      }
    } else if (variant === 'circle') {
      this._wrap.classList.add('tf-skeleton-circle');
      const size = width || height || '48px';
      this._wrap.style.width = size;
      this._wrap.style.height = size;
    } else {
      this._wrap.classList.add('tf-skeleton-rect');
      if (width) this._wrap.style.width = width;
      if (height) this._wrap.style.height = height;
    }

    this._wrap.setAttribute('aria-hidden', 'true');
  }
}

customElements.define('tf-skeleton', TfSkeleton);
export { TfSkeleton };
