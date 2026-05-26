// =============================================================================
// File: tf-section-card.js
// Opis: Grouped content panel with optional title, icon, and action link.
//       Light DOM component that wraps slotted content in a styled card.
// =============================================================================

class TfSectionCard extends HTMLElement {
  static get observedAttributes() {
    return ['title', 'icon', 'action-text', 'action-href'];
  }

  constructor() {
    super();
    this._root = null;
    this._head = null;
    this._body = null;
    this._slotContent = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    // Capture slotted content before clearing
    this._slotContent = document.createDocumentFragment();
    while (this.firstChild) {
      this._slotContent.appendChild(this.firstChild);
    }

    const card = document.createElement('div');
    card.className = 'tf-section-card';

    this._head = document.createElement('div');
    this._head.className = 'tf-section-card-head';
    card.appendChild(this._head);

    this._body = document.createElement('div');
    this._body.className = 'tf-section-card-body';
    this._body.appendChild(this._slotContent);
    card.appendChild(this._body);

    this.appendChild(card);
    this._root = card;
  }

  _update() {
    const title = this.getAttribute('title') || '';
    const icon = (this.getAttribute('icon') || '').trim();
    const actionText = this.getAttribute('action-text') || '';
    const actionHref = this.getAttribute('action-href') || '#';

    if (!title && !actionText) {
      this._head.style.display = 'none';
      return;
    }
    this._head.style.display = '';

    const iconHtml = icon
      ? `<svg class="tf-section-card-icon" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="/img/icons.svg#icon-${icon}"/></svg>`
      : '';

    const actionHtml = actionText
      ? `<a class="tf-section-card-action" href="${actionHref}">${actionText}</a>`
      : '';

    this._head.innerHTML =
      `<span class="tf-section-card-title">${iconHtml}${title}</span>${actionHtml}`;
  }
}

customElements.define('tf-section-card', TfSectionCard);
export { TfSectionCard };
