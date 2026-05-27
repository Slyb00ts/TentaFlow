// =============================================================================
// File: tf-divider.js
// Description: <tf-divider label="Section" subtle> — section divider with
//              optional centered label and subtle mode. Light DOM.
// Example: <tf-divider label="Details"></tf-divider>
// =============================================================================

class TfDivider extends HTMLElement {
  static get observedAttributes() {
    return ['subtle', 'label'];
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
    el.className = 'tf-divider';
    this.appendChild(el);
    this._el = el;
  }

  _update() {
    const subtle = this.hasAttribute('subtle');
    const label = this.getAttribute('label');
    const cls = ['tf-divider'];
    if (subtle) cls.push('subtle');
    if (label) cls.push('labeled');
    this._el.className = cls.join(' ');

    if (label) {
      this._el.innerHTML = `<span class="tf-divider-line"></span><span class="tf-divider-label">${label}</span><span class="tf-divider-line"></span>`;
    } else {
      this._el.innerHTML = '';
    }
  }
}

customElements.define('tf-divider', TfDivider);
export { TfDivider };
