// =============================================================================
// File: tf-stage.js
// Description: <tf-stage stage="qualify"> — pipeline stage indicator with
//              colored left border and label. Light DOM.
// Example: <tf-stage stage="won"></tf-stage>
// =============================================================================

const STAGE_LABELS = {
  lead: 'Lead',
  qualify: 'Qualify',
  offer: 'Offer',
  commit: 'Commit',
  execute: 'Execute',
  won: 'Won',
  lost: 'Lost',
};

class TfStage extends HTMLElement {
  static get observedAttributes() {
    return ['stage'];
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
    el.className = 'tf-stage';
    this.appendChild(el);
    this._el = el;
  }

  _update() {
    const stage = (this.getAttribute('stage') || 'lead').toLowerCase();
    this._el.className = `tf-stage ${stage}`;
    this._el.textContent = STAGE_LABELS[stage] || stage;
  }
}

customElements.define('tf-stage', TfStage);
export { TfStage };
