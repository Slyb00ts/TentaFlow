// =============================================================================
// File: tf-empty-state.js
// Opis: No-data placeholder component. Displays a centered icon, title,
//       message, and an optional action slot for buttons.
// =============================================================================

class TfEmptyState extends HTMLElement {
  static get observedAttributes() {
    return ['icon', 'title', 'message'];
  }

  constructor() {
    super();
    this._root = null;
    this._actionSlot = null;
    this._slotContent = null;
    this._slottedIcon = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    // A child with slot="icon" (e.g. an externally rendered <svg>/<img>)
    // takes over the icon area; captured before the action sweep below.
    this._slottedIcon = this.querySelector(':scope > [slot="icon"]');
    if (this._slottedIcon) this._slottedIcon.remove();

    // Capture slotted content (action buttons)
    this._slotContent = document.createDocumentFragment();
    while (this.firstChild) {
      this._slotContent.appendChild(this.firstChild);
    }

    const el = document.createElement('div');
    el.className = 'tf-empty-state';

    this._iconEl = document.createElement('div');
    this._iconEl.className = 'tf-empty-state-icon';
    if (this._slottedIcon) {
      this._slottedIcon.removeAttribute('slot');
      this._iconEl.appendChild(this._slottedIcon);
    }
    el.appendChild(this._iconEl);

    this._titleEl = document.createElement('div');
    this._titleEl.className = 'tf-empty-state-title';
    el.appendChild(this._titleEl);

    this._msgEl = document.createElement('div');
    this._msgEl.className = 'tf-empty-state-message';
    el.appendChild(this._msgEl);

    this._actionSlot = document.createElement('div');
    this._actionSlot.className = 'tf-empty-state-actions';
    this._actionSlot.appendChild(this._slotContent);
    el.appendChild(this._actionSlot);

    this.appendChild(el);
    this._root = el;
  }

  _update() {
    const icon = (this.getAttribute('icon') || '').trim();
    const title = this.getAttribute('title') || '';
    const message = this.getAttribute('message') || '';

    if (this._slottedIcon) {
      this._iconEl.style.display = '';
    } else if (icon) {
      this._iconEl.innerHTML =
        `<svg width="48" height="48" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="/img/icons.svg#icon-${icon}"/></svg>`;
      this._iconEl.style.display = '';
    } else {
      this._iconEl.style.display = 'none';
    }

    this._titleEl.textContent = title;
    this._titleEl.style.display = title ? '' : 'none';

    this._msgEl.textContent = message;
    this._msgEl.style.display = message ? '' : 'none';

    // Hide actions slot if empty
    this._actionSlot.style.display =
      this._actionSlot.children.length > 0 ? '' : 'none';
  }
}

customElements.define('tf-empty-state', TfEmptyState);
export { TfEmptyState };
