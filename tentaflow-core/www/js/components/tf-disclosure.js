// =============================================================================
// File: tf-disclosure.js
// Opis: Collapsible section: a `label` that opens and closes its content. The
//       content is whatever is placed inside the element. `open` is reflected
//       on the host and a `toggle` event carries the new state in
//       `detail.open`.
// =============================================================================

class TfDisclosure extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'open'];
  }

  constructor() {
    super();
    this._details = null;
    this._summary = null;
  }

  connectedCallback() {
    if (!this._details) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._details) this._update();
  }

  get open() {
    return this.hasAttribute('open');
  }

  set open(value) {
    if (value) this.setAttribute('open', '');
    else this.removeAttribute('open');
  }

  _build() {
    const body = document.createElement('div');
    body.className = 'tf-disclosure__body';
    while (this.firstChild) body.appendChild(this.firstChild);

    this._summary = document.createElement('summary');
    this._summary.className = 'tf-disclosure__summary';

    this._details = document.createElement('details');
    this._details.className = 'tf-disclosure';
    this._details.append(this._summary, body);
    this._details.addEventListener('toggle', () => {
      if (this._details.open === this.open) return;
      this.open = this._details.open;
      this.dispatchEvent(new CustomEvent('toggle', { detail: { open: this._details.open } }));
    });
    this.appendChild(this._details);
  }

  _update() {
    this._summary.textContent = this.getAttribute('label') || '';
    if (this._details.open !== this.open) this._details.open = this.open;
  }
}

customElements.define('tf-disclosure', TfDisclosure);
export { TfDisclosure };
