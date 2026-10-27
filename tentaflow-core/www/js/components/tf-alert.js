// =============================================================================
// File: tf-alert.js
// Opis: Inline notification component with tone variants (info, success,
//       warning, danger), optional title, message, and dismiss button.
//       `icon="<sprite-id>"` replaces the tone glyph with a sprite symbol,
//       `inline-actions` puts the slotted actions beside the text (they wrap
//       below it when the row is narrow).
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
    return ['tone', 'title', 'message', 'dismissable', 'icon', 'inline-actions', 'live'];
  }

  constructor() {
    super();
    this._root = null;
    this._actionsEl = null;
    this._messageEl = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
    // A parser that connects the element before it appends its children (the slotted message arrives after the build).
    if (typeof MutationObserver === 'function') {
      this._late ??= new MutationObserver(() => this._adoptLateMessage());
      this._late.observe(this, { childList: true });
    }
  }

  disconnectedCallback() {
    this._late?.disconnect();
  }

  _adoptLateMessage() {
    const late = Array.from(this.children).find((c) => c.getAttribute('slot') === 'message');
    if (!late) return;
    // The attribute wins; a slotted element beside it would only be a stray child.
    if (this.getAttribute('message')) {
      late.remove();
      return;
    }
    late.removeAttribute('slot');
    this._messageEl = late;
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
    // A message that needs markup of its own (a name that must not break across lines) arrives as a slotted element;
    // the `message` attribute stays plain text.
    const messageContent = this.querySelector('[slot="message"]');
    if (messageContent) messageContent.removeAttribute('slot');
    this._messageEl = messageContent;
    // Fallback for the common `<tf-alert>text</tf-alert>` form: capture loose child
    // text (NOT element children like the actions container) before the clear
    // below drops it. Without this the message is silently lost and the alert
    // renders as an empty coloured box — `message` attribute still wins when set.
    this._childText = Array.from(this.childNodes)
      .filter((n) => n.nodeType === Node.TEXT_NODE)
      .map((n) => n.textContent)
      .join('')
      .trim();
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
    const message = this.getAttribute('message') || this._childText || '';
    const dismissable = this.hasAttribute('dismissable');

    this._root.className = `tf-alert ${tone}`;
    // `live` makes the alert a live region: a message that appears or changes after the page is read out, which a plain banner does not do.
    if (this.hasAttribute('live')) this._root.setAttribute('role', 'alert');
    else this._root.removeAttribute('role');
    this._root.classList.toggle('inline-actions', this.hasAttribute('inline-actions'));
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
    const spriteId = (this.getAttribute('icon') || '').replace(/[^a-z0-9_-]/gi, '');
    if (spriteId) {
      const use = document.createElementNS(svgNs, 'use');
      use.setAttribute('href', `#i-${spriteId}`);
      icon.appendChild(use);
    } else {
      const iconPathEl = document.createElementNS(svgNs, 'path');
      iconPathEl.setAttribute('d', TONE_ICONS[tone]);
      icon.appendChild(iconPathEl);
    }
    this._root.appendChild(icon);

    // Title and message are attacker-reachable (foreign-node manifests, addon
    // state). Assign as textContent so any HTML in them is rendered inert.
    const content = document.createElement('div');
    content.className = 'tf-alert-content';
    // With `inline-actions` the text is one flex item, so the actions sit beside
    // it as a block instead of between the title and the message.
    let text = content;
    if (this.hasAttribute('inline-actions')) {
      text = document.createElement('div');
      text.className = 'tf-alert-text';
      content.appendChild(text);
    }
    if (title) {
      const titleEl = document.createElement('div');
      titleEl.className = 'tf-alert-title';
      titleEl.textContent = title;
      text.appendChild(titleEl);
    }
    if (message || this._messageEl) {
      const msgEl = document.createElement('div');
      msgEl.className = 'tf-alert-message';
      if (message) msgEl.textContent = message;
      else msgEl.appendChild(this._messageEl);
      text.appendChild(msgEl);
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
