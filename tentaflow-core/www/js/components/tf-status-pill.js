// =============================================================================
// File: tf-status-pill.js
// Description: <tf-status-pill status="ok" label="Running"> — runtime status
//              indicator with pulsing dot and label. Light DOM.
// Example: <tf-status-pill status="err" label="Down"></tf-status-pill>
// =============================================================================

class TfStatusPill extends HTMLElement {
  static get observedAttributes() {
    return ['status', 'label'];
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
    const el = document.createElement('span');
    el.className = 'tf-status-pill';
    this.appendChild(el);
    this._el = el;
  }

  _update() {
    const status = (this.getAttribute('status') || 'ok').toLowerCase();
    const label = this.getAttribute('label') || '';
    this._el.className = `tf-status-pill ${status}`;
    this._el.textContent = label;
  }
}

customElements.define('tf-status-pill', TfStatusPill);
export { TfStatusPill };
