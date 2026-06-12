// =============================================================================
// File: tf-section-card.js
// Opis: Grouped content panel with optional title, icon, and action link.
//       Light DOM component that wraps slotted content in a styled card.
//       Children with slot="subtitle" / slot="actions" / slot="footer" are
//       placed in the header (under title / right side) and after the body.
//       The `header-divider` attribute shows a divider under the header.
//       The `plain` attribute is the headerless bare-container variant: the
//       component leaves its light DOM untouched so callers can drive the
//       .tf-card--* token classes directly on the host element.
// =============================================================================

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

class TfSectionCard extends HTMLElement {
  static get observedAttributes() {
    return ['title', 'icon', 'action-text', 'action-href', 'header-divider'];
  }

  constructor() {
    super();
    this._root = null;
    this._head = null;
    this._body = null;
    this._titleSpan = null;
    this._action = null;
    this._divider = null;
    this._hasHeadSlots = false;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    if (this.hasAttribute('plain')) {
      // Plain variant: no wrappers, children stay as-is on the host.
      this._root = this;
      return;
    }

    // Partition slotted content before clearing
    const subtitle = [];
    const actions = [];
    const footer = [];
    const body = [];
    while (this.firstChild) {
      const node = this.firstChild;
      node.remove();
      const slot = node.nodeType === 1 ? node.getAttribute('slot') : null;
      if (slot === 'subtitle') subtitle.push(node);
      else if (slot === 'actions') actions.push(node);
      else if (slot === 'footer') footer.push(node);
      else body.push(node);
    }
    this._hasHeadSlots = subtitle.length > 0 || actions.length > 0;

    const card = document.createElement('div');
    card.className = 'tf-section-card';

    this._head = document.createElement('div');
    this._head.className = 'tf-section-card-head';
    this._titleSpan = document.createElement('span');
    this._titleSpan.className = 'tf-section-card-title';
    if (subtitle.length) {
      const titles = document.createElement('div');
      titles.className = 'tf-section-card__titles';
      titles.appendChild(this._titleSpan);
      for (const node of subtitle) titles.appendChild(node);
      this._head.appendChild(titles);
    } else {
      this._head.appendChild(this._titleSpan);
    }
    this._action = document.createElement('a');
    this._action.className = 'tf-section-card-action';
    this._action.style.display = 'none';
    this._head.appendChild(this._action);
    for (const node of actions) this._head.appendChild(node);
    card.appendChild(this._head);

    this._divider = document.createElement('div');
    this._divider.className = 'tf-section-card__header-divider';
    this._divider.style.display = 'none';
    card.appendChild(this._divider);

    this._body = document.createElement('div');
    this._body.className = 'tf-section-card-body';
    for (const node of body) this._body.appendChild(node);
    card.appendChild(this._body);

    for (const node of footer) card.appendChild(node);

    this.appendChild(card);
    this._root = card;
  }

  _update() {
    if (this.hasAttribute('plain')) return;

    const title = this.getAttribute('title') || '';
    const icon = (this.getAttribute('icon') || '').trim();
    const actionText = this.getAttribute('action-text') || '';
    const actionHref = this.getAttribute('action-href') || '#';

    this._divider.style.display = this.hasAttribute('header-divider') ? '' : 'none';

    if (!title && !actionText && !this._hasHeadSlots) {
      this._head.style.display = 'none';
    } else {
      this._head.style.display = '';
    }

    const iconHtml = icon
      ? `<svg class="tf-section-card-icon" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="/img/icons.svg#icon-${icon}"/></svg>`
      : '';
    this._titleSpan.innerHTML = `${iconHtml}${escapeHtml(title)}`;

    if (actionText) {
      this._action.textContent = actionText;
      this._action.setAttribute('href', actionHref);
      this._action.style.display = '';
    } else {
      this._action.style.display = 'none';
    }
  }
}

customElements.define('tf-section-card', TfSectionCard);
export { TfSectionCard };
