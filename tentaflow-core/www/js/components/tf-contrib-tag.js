// =============================================================================
// File: tf-contrib-tag.js
// Description: <tf-contrib-tag addon="crm"> — cross-addon attribution tag
//              with color-coded label per addon domain. Light DOM.
// Example: <tf-contrib-tag addon="calendar"></tf-contrib-tag>
// =============================================================================

const ADDON_LABELS = {
  crm: 'CRM',
  calendar: 'Calendar',
  contacts: 'Contacts',
  billing: 'Billing',
  activity: 'Activity',
  ai: 'AI',
  documents: 'Documents',
};

class TfContribTag extends HTMLElement {
  static get observedAttributes() {
    return ['addon'];
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
    el.className = 'tf-contrib-tag';
    this.appendChild(el);
    this._el = el;
  }

  _update() {
    const addon = (this.getAttribute('addon') || '').toLowerCase();
    this._el.className = `tf-contrib-tag ${addon}`;
    this._el.textContent = ADDON_LABELS[addon] || addon;
  }
}

customElements.define('tf-contrib-tag', TfContribTag);
export { TfContribTag };
