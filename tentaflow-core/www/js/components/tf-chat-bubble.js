// =============================================================================
// File: js/components/tf-chat-bubble.js
// Description: Reusable chat message bubble. Renders DOM identical to chat.js
//              renderBubble() using existing .msg-row / .bubble / .msg-actions
//              classes from style.css. Light DOM, no Shadow DOM.
// Example: <tf-chat-bubble role="assistant" sender="GPT" time="14:30">Hello</tf-chat-bubble>
// =============================================================================

function sprite(id) {
  return `<svg class="icon" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><use href="#i-${id}"/></svg>`;
}

class TfChatBubble extends HTMLElement {
  static get observedAttributes() {
    return ['role', 'sender', 'time', 'model', 'streaming'];
  }

  constructor() {
    super();
    this._built = false;
    this._slotContent = '';
  }

  connectedCallback() {
    if (!this._built) {
      if (!this._slotContent) this._slotContent = this.innerHTML;
      this._build();
    }
  }

  attributeChangedCallback() {
    if (this._built) this._build();
  }

  _build() {
    const role = this.getAttribute('role') || 'assistant';
    const sender = this.getAttribute('sender') || '';
    const time = this.getAttribute('time') || '';
    const model = this.getAttribute('model') || '';
    const streaming = this.hasAttribute('streaming');
    const isUser = role === 'user';

    const cls = isUser ? 'user' : 'assistant';

    const avatar = isUser
      ? `<div class="avatar user">${sender ? sender.charAt(0).toUpperCase() : 'U'}</div>`
      : `<div class="avatar assistant">${sprite('model')}</div>`;

    const meta = isUser
      ? `<div class="bubble-meta"><span>${this._esc(time)}</span><span class="who">${this._esc(sender)}</span></div>`
      : `<div class="bubble-meta"><span class="who">${this._esc(model || sender)}</span>${time ? `<span>·</span><span>${this._esc(time)}</span>` : ''}</div>`;

    const streamCaret = streaming && !isUser
      ? '<span class="streaming-caret"></span>'
      : '';

    const actions = isUser
      ? `<div class="msg-actions">
          <button type="button" class="msg-act" data-act="copy" title="Copy">${sprite('copy')}</button>
        </div>`
      : `<div class="msg-actions">
          <button type="button" class="msg-act" data-act="copy" title="Copy">${sprite('copy')}</button>
          <button type="button" class="msg-act" data-act="regenerate" title="Regenerate">${sprite('refresh')}</button>
        </div>`;

    const bubbleContent = `
      <div class="bubble-wrap">
        ${meta}
        <div class="bubble">${this._slotContent}${streamCaret}</div>
        ${actions}
      </div>`;

    const inner = isUser
      ? `${bubbleContent}${avatar}`
      : `${avatar}${bubbleContent}`;

    this.innerHTML = `<div class="msg-row ${cls}">${inner}</div>`;
    this._built = true;

    this.querySelector('.msg-actions')?.addEventListener('click', (e) => {
      const btn = e.target.closest('.msg-act');
      if (!btn) return;
      const act = btn.dataset.act;
      this.dispatchEvent(new CustomEvent('action', {
        bubbles: true,
        detail: { type: act },
      }));
    });
  }

  // Update slot content without full attribute change
  setContent(html) {
    this._slotContent = html;
    if (this._built) this._build();
  }

  _esc(str) {
    if (!str) return '';
    const d = document.createElement('div');
    d.textContent = str;
    return d.innerHTML;
  }
}

customElements.define('tf-chat-bubble', TfChatBubble);
export { TfChatBubble };
