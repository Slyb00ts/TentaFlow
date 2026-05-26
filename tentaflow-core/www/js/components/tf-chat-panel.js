// =============================================================================
// File: js/components/tf-chat-panel.js
// Description: Embeddable side panel for addon chat. 320px wide panel with
//              header (tf-face mini + title + status + close), scrollable body
//              of tf-chat-bubble messages, and tf-chat-composer footer.
//              Compact variant with smaller bubbles for side-panel use.
// Example: <tf-chat-panel title="AI Assistant" open></tf-chat-panel>
// =============================================================================

import './tf-chat-bubble.js';
import './tf-chat-composer.js';

class TfChatPanel extends HTMLElement {
  static get observedAttributes() {
    return ['title', 'open'];
  }

  constructor() {
    super();
    this._built = false;
    this._messages = [];
    this._body = null;
  }

  set messages(arr) {
    this._messages = Array.isArray(arr) ? arr : [];
    if (this._built) this._renderMessages();
  }

  get messages() {
    return this._messages;
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._updateVisibility();
  }

  attributeChangedCallback(name) {
    if (!this._built) return;
    if (name === 'title') {
      const titleEl = this.querySelector('.tf-chat-panel-title');
      if (titleEl) titleEl.textContent = this.getAttribute('title') || 'AI Assistant';
    }
    if (name === 'open') this._updateVisibility();
  }

  _build() {
    const title = this.getAttribute('title') || 'AI Assistant';

    this.innerHTML = `
      <div class="tf-chat-panel">
        <div class="tf-chat-panel-head">
          <tf-face mode="idle" size="24"></tf-face>
          <span class="tf-chat-panel-title">${this._esc(title)}</span>
          <span style="flex:1"></span>
          <tf-chip status="ok" dot>Online</tf-chip>
          <tf-button variant="ghost" icon="x" size="sm" class="tf-chat-panel-close" aria-label="Close"></tf-button>
        </div>
        <div class="tf-chat-panel-body"></div>
        <tf-chat-composer placeholder="Write a message..."></tf-chat-composer>
      </div>`;

    this._body = this.querySelector('.tf-chat-panel-body');
    this._built = true;

    // Close button
    this.querySelector('.tf-chat-panel-close').addEventListener('click', () => {
      this.removeAttribute('open');
      this.dispatchEvent(new CustomEvent('close', { bubbles: true }));
    });

    // Forward send from composer
    this.querySelector('tf-chat-composer').addEventListener('send', (e) => {
      this.dispatchEvent(new CustomEvent('send', {
        bubbles: true,
        detail: e.detail,
      }));
    });

    this._renderMessages();
    this._updateVisibility();
  }

  _renderMessages() {
    if (!this._body) return;
    this._body.innerHTML = '';
    for (const msg of this._messages) {
      const bubble = document.createElement('tf-chat-bubble');
      bubble.setAttribute('role', msg.role || 'assistant');
      if (msg.sender) bubble.setAttribute('sender', msg.sender);
      if (msg.time) bubble.setAttribute('time', msg.time);
      if (msg.model) bubble.setAttribute('model', msg.model);
      if (msg.streaming) bubble.setAttribute('streaming', '');
      bubble.setContent(msg.text || '');
      this._body.appendChild(bubble);
    }
    // Auto-scroll to bottom
    this._body.scrollTop = this._body.scrollHeight;
  }

  // Append a single message without re-rendering all
  appendMessage(msg) {
    this._messages.push(msg);
    if (!this._body) return;
    const bubble = document.createElement('tf-chat-bubble');
    bubble.setAttribute('role', msg.role || 'assistant');
    if (msg.sender) bubble.setAttribute('sender', msg.sender);
    if (msg.time) bubble.setAttribute('time', msg.time);
    if (msg.model) bubble.setAttribute('model', msg.model);
    if (msg.streaming) bubble.setAttribute('streaming', '');
    bubble.setContent(this._esc(msg.text || ''));
    this._body.appendChild(bubble);
    this._body.scrollTop = this._body.scrollHeight;
  }

  _updateVisibility() {
    const panel = this.querySelector('.tf-chat-panel');
    if (panel) panel.style.display = this.hasAttribute('open') ? '' : 'none';
  }

  _esc(str) {
    if (!str) return '';
    const d = document.createElement('div');
    d.textContent = str;
    return d.innerHTML;
  }
}

customElements.define('tf-chat-panel', TfChatPanel);
export { TfChatPanel };
