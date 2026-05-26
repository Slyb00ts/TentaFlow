// =============================================================================
// File: tf-spinner.js
// Description: <tf-spinner size="md"> — loading spinner with CSS border
//              animation and glow trail. Light DOM.
// Example: <tf-spinner size="lg"></tf-spinner>
// =============================================================================

class TfSpinner extends HTMLElement {
  static get observedAttributes() {
    return ['size'];
  }

  constructor() {
    super();
    this._el = null;
  }

  connectedCallback() {
    if (!this._el) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._el) this._update();
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-spinner';
    el.setAttribute('role', 'status');
    el.setAttribute('aria-label', 'Loading');
    this.appendChild(el);
    this._el = el;
  }

  _update() {
    const size = (this.getAttribute('size') || 'md').toLowerCase();
    this._el.className = `tf-spinner ${size}`;
  }
}

customElements.define('tf-spinner', TfSpinner);
export { TfSpinner };
