// =============================================================================
// File: tf-progress-bar.js
// Opis: Progress bar component with tone variants, size options, optional label,
//       and a shimmer animation on the fill track.
// =============================================================================

const VALID_TONES = new Set(['accent', 'success', 'warning', 'danger']);
const VALID_SIZES = new Set(['sm', 'md', 'lg']);

class TfProgressBar extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'tone', 'size', 'label', 'orientation'];
  }

  constructor() {
    super();
    this._root = null;
    this._fill = null;
    this._labelEl = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    this.innerHTML = '';

    const wrap = document.createElement('div');
    wrap.className = 'tf-progress-bar-wrap';

    this._labelEl = document.createElement('div');
    this._labelEl.className = 'tf-progress-bar-label';
    wrap.appendChild(this._labelEl);

    const track = document.createElement('div');
    track.className = 'tf-progress-bar';

    this._fill = document.createElement('div');
    this._fill.className = 'tf-progress-bar-fill';
    track.appendChild(this._fill);

    wrap.appendChild(track);
    this.appendChild(wrap);
    this._root = track;
  }

  _update() {
    const raw = parseFloat(this.getAttribute('value') || '0');
    const value = Math.max(0, Math.min(100, isNaN(raw) ? 0 : raw));
    const tone = VALID_TONES.has(this.getAttribute('tone'))
      ? this.getAttribute('tone')
      : 'accent';
    const size = VALID_SIZES.has(this.getAttribute('size'))
      ? this.getAttribute('size')
      : 'md';
    const label = this.getAttribute('label') || '';
    const vertical = this.getAttribute('orientation') === 'vertical';

    this._root.className = `tf-progress-bar ${size}${vertical ? ' vertical' : ''}`;
    this._fill.className = `tf-progress-bar-fill ${tone}`;
    // Vertical bars grow bottom→top (height), horizontal ones left→right (width).
    if (vertical) {
      this._fill.style.width = '';
      this._fill.style.height = `${value}%`;
    } else {
      this._fill.style.height = '';
      this._fill.style.width = `${value}%`;
    }

    if (label) {
      this._labelEl.textContent = label;
      this._labelEl.style.display = '';
    } else {
      this._labelEl.style.display = 'none';
    }
  }
}

customElements.define('tf-progress-bar', TfProgressBar);
export { TfProgressBar };
