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
    this._root.textContent = '';

    // Icon path comes from the trusted TONE_ICONS map keyed by a validated
    // tone, so no caller data reaches the SVG markup. Build via the SVG
    // namespace instead of innerHTML.
    const svgNs = 'http://www.w3.org/2000/svg';
    const icon = document.createElementNS(svgNs, 'svg');
    icon.setAttribute('class', 'tf-alert-icon');
    icon.setAttribute('width', '18');
    icon.setAttribute('height', '18');
    icon.setAttribute('viewBox', '0 0 24 24');
    icon.setAttribute('fill', 'none');
    icon.setAttribute('stroke', 'currentColor');
    icon.setAttribute('stroke-width', '2');
    icon.setAttribute('stroke-linecap', 'round');
    icon.setAttribute('stroke-linejoin', 'round');
    icon.setAttribute('aria-hidden', 'true');
    const iconPathEl = document.createElementNS(svgNs, 'path');
    iconPathEl.setAttribute('d', TONE_ICONS[tone]);
    icon.appendChild(iconPathEl);
    this._root.appendChild(icon);

    // Title and message are attacker-reachable (foreign-node manifests, addon
    // state). Assign as textContent so any HTML in them is rendered inert.
    const content = document.createElement('div');
    content.className = 'tf-alert-content';
    if (title) {
      const titleEl = document.createElement('div');
      titleEl.className = 'tf-alert-title';
      titleEl.textContent = title;
      content.appendChild(titleEl);
    }
    if (message) {
      const msgEl = document.createElement('div');
      msgEl.className = 'tf-alert-message';
      msgEl.textContent = message;
      content.appendChild(msgEl);
    }
    if (this._actionsEl) content.appendChild(this._actionsEl);
    this._root.appendChild(content);

    if (dismissable) {
      const btn = document.createElement('button');
      btn.className = 'tf-alert-close';
      btn.setAttribute('aria-label', 'Dismiss');
      btn.appendChild(document.createTextNode('×'));
      btn.addEventListener('click', () => {
        this.dispatchEvent(new CustomEvent('dismiss', { bubbles: true }));
        this.remove();
      }, { once: true });
      this._root.appendChild(btn);
    }
  }
}

customElements.define('tf-alert', TfAlert);
export { TfAlert };
