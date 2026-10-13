// =============================================================================
// File: tf-disclosure.js
// Opis: Collapsible section: a `label` that opens and closes its content. The
//       content is whatever is placed inside the element — also what is
//       appended after the first render. `open` is reflected on the host and a
//       `toggle` event carries the new state in `detail.open`, whether the user
//       or code changed it. Opening scrolls the content into view.
// =============================================================================

class TfDisclosure extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'open'];
  }

  constructor() {
    super();
    this._details = null;
    this._summary = null;
    this._body = null;
    this._announced = false;
    this._observer = null;
  }

  connectedCallback() {
    if (!this._details) this._build();
    this._update();
    this._observer?.observe(this, { childList: true });
  }

  disconnectedCallback() {
    this._observer?.disconnect();
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
    this._body = document.createElement('div');
    this._body.className = 'tf-disclosure__body';
    while (this.firstChild) this._body.appendChild(this.firstChild);

    this._summary = document.createElement('summary');
    this._summary.className = 'tf-disclosure__summary';

    this._details = document.createElement('details');
    this._details.className = 'tf-disclosure';
    this._details.append(this._summary, this._body);
    this._details.addEventListener('toggle', () => {
      if (this._details.open !== this.open) this.open = this._details.open;
      this._announce();
    });
    this.appendChild(this._details);
    this._announced = this.open;

    // Whatever is appended to the host later belongs inside the fold too.
    this._observer = new MutationObserver(() => {
      for (const node of [...this.childNodes]) {
        if (node !== this._details) this._body.appendChild(node);
      }
    });
  }

  _update() {
    this._summary.textContent = this.getAttribute('label') || '';
    if (this._details.open !== this.open) this._details.open = this.open;
    this._announce();
  }

  _announce() {
    if (this._announced === this.open) return;
    this._announced = this.open;
    this.dispatchEvent(new CustomEvent('toggle', { detail: { open: this.open } }));
    if (this.open) this._body.scrollIntoView?.({ block: 'nearest' });
  }
}

customElements.define('tf-disclosure', TfDisclosure);
export { TfDisclosure };
