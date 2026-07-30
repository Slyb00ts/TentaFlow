// =============================================================================
// File: tf-badge.js
// Opis: Numeric badge component with tone variants. Renders a small pill with
//       a value from attribute or slot content.
// =============================================================================

const VALID_TONES = new Set(['accent', 'danger', 'success', 'warning', 'info', 'neutral']);

class TfBadge extends HTMLElement {
  static get observedAttributes() {
    return ['tone', 'value'];
  }

  constructor() {
    super();
    this._span = null;
    this._slotText = '';
  }

  connectedCallback() {
    if (!this._span) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._span) this._update();
  }

  _build() {
    this._slotText = this.textContent.trim();
    this.innerHTML = '';
    const span = document.createElement('span');
    span.className = 'tf-badge';
    this.appendChild(span);
    this._span = span;
  }

  _update() {
    const tone = VALID_TONES.has(this.getAttribute('tone'))
      ? this.getAttribute('tone')
      : 'accent';
    const value = this.getAttribute('value');
    const text = value !== null ? value : this._slotText;

    this._span.className = `tf-badge ${tone}`;
    this._span.textContent = text;
  }
}

customElements.define('tf-badge', TfBadge);
export { TfBadge };
