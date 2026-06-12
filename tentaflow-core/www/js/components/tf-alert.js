// =============================================================================
// File: tf-alert.js
// Opis: Inline notification component with tone variants (info, success,
//       warning, danger), optional title, message, and dismiss button.
// =============================================================================

const TONE_ICONS = {
  info:    'M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm1 15h-2v-6h2zm0-8h-2V7h2z',
  success: 'M22 11.08V12a10 10 0 1 1-5.93-9.14M22 4 12 14.01l-3-3',
  warning: 'M12 9v4m0 4h.01M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z',
  danger:  'M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zm0 6v4m0 4h.01',
};

const VALID_TONES = new Set(['info', 'success', 'warning', 'danger']);

class TfAlert extends HTMLElement {
  static get observedAttributes() {
    return ['tone', 'title', 'message', 'dismissable'];
  }

  constructor() {
    super();
    this._root = null;
    this._actionsEl = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    // Preserve a slotted actions container (e.g. SDK-rendered buttons) before
    // clearing; it is re-appended into the content area on every update.
    const actionsContent = this.querySelector('[slot="actions"]');
    if (actionsContent) actionsContent.removeAttribute('slot');
    this._actionsEl = actionsContent;
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-alert';
    this.appendChild(el);
    this._root = el;
  }

  _update() {
    const tone = VALID_TONES.has(this.getAttribute('tone'))
      ? this.getAttribute('tone')
      : 'info';
    const title = this.getAttribute('title') || '';
    const message = this.getAttribute('message') || '';
    const dismissable = this.hasAttribute('dismissable');

    this._root.className = `tf-alert ${tone}`;

    const iconPath = TONE_ICONS[tone];
    const iconHtml = `<svg class="tf-alert-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="${iconPath}"/></svg>`;

    const titleHtml = title ? `<div class="tf-alert-title">${title}</div>` : '';
    const msgHtml = message ? `<div class="tf-alert-message">${message}</div>` : '';

    const closeHtml = dismissable
      ? `<button class="tf-alert-close" aria-label="Dismiss">&times;</button>`
      : '';

    this._root.innerHTML =
      `${iconHtml}<div class="tf-alert-content">${titleHtml}${msgHtml}</div>${closeHtml}`;

    if (this._actionsEl) {
      this._root.querySelector('.tf-alert-content').appendChild(this._actionsEl);
    }

    if (dismissable) {
      const btn = this._root.querySelector('.tf-alert-close');
      btn.addEventListener('click', () => {
        this.dispatchEvent(new CustomEvent('dismiss', { bubbles: true }));
        this.remove();
      }, { once: true });
    }
  }
}

customElements.define('tf-alert', TfAlert);
export { TfAlert };
