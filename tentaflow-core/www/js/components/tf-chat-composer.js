// =============================================================================
// File: js/components/tf-chat-composer.js
// Description: Reusable chat message composer. Uses existing .composer-wrap /
//              .composer classes from style.css. Contains attach, textarea,
//              voice and send buttons. Enter sends, Shift+Enter inserts newline.
// Example: <tf-chat-composer placeholder="Ask anything..."></tf-chat-composer>
// =============================================================================

import { I18n } from '/js/i18n.js';

// The composer is shared with the chat screen, so its furniture has to speak the
// operator's language like the rest of the app — these strings were hardcoded.
function ct(key) {
  return I18n.t(`composer.${key}`);
}

const DEFAULT_MAX_LENGTH = 4096;

class TfChatComposer extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'max-length', 'disabled'];
  }

  constructor() {
    super();
    this._built = false;
    this._textarea = null;
    this._counter = null;
    this._sendBtn = null;
  }

  get value() {
    return this._textarea ? this._textarea.value : '';
  }

  set value(v) {
    if (this._textarea) {
      this._textarea.value = v;
      this._updateCounter();
    }
  }

  get maxLength() {
    const attr = this.getAttribute('max-length');
    return attr ? parseInt(attr, 10) : DEFAULT_MAX_LENGTH;
  }

  connectedCallback() {
    if (!this._built) this._build();
    this._updateDisabled();
  }

  attributeChangedCallback(name) {
    if (!this._built) return;
    if (name === 'placeholder') {
      const ta = this.querySelector('tf-textarea');
      if (ta) ta.setAttribute('placeholder', this.getAttribute('placeholder') || ct('placeholder'));
    }
    if (name === 'disabled') this._updateDisabled();
  }

  _build() {
    const placeholder = this.getAttribute('placeholder') || ct('placeholder');

    this.innerHTML = `
      <div class="composer-wrap">
        <div class="composer">
          <tf-button variant="ghost" icon="paperclip" class="composer-attach" aria-label="${this._esc(ct('attach'))}"></tf-button>
          <tf-textarea autogrow rows="1" placeholder="${this._esc(placeholder)}"></tf-textarea>
          <div style="display:flex;gap:4px;align-self:end">
            <tf-button variant="ghost" icon="mic" class="composer-voice" aria-label="${this._esc(ct('voice'))}"></tf-button>
            <tf-button variant="primary" icon="send" class="composer-send" aria-label="${this._esc(ct('send'))}"></tf-button>
          </div>
        </div>
        <div class="composer-hints">
          <span class="kbd"><kbd>Enter</kbd> ${this._esc(ct('hint_send'))}</span>
          <span class="kbd"><kbd>Shift</kbd>+<kbd>Enter</kbd> ${this._esc(ct('hint_newline'))}</span>
          <span class="spacer"></span>
          <span class="counter">0 / ${this.maxLength}</span>
        </div>
      </div>`;

    this._textarea = this.querySelector('tf-textarea');
    this._counter = this.querySelector('.counter');
    this._sendBtn = this.querySelector('.composer-send');
    this._built = true;

    // Send on button click
    this._sendBtn.addEventListener('click', () => this._send());

    // Voice button
    this.querySelector('.composer-voice').addEventListener('click', () => {
      this.dispatchEvent(new CustomEvent('voice', { bubbles: true }));
    });

    // Keyboard: Enter sends, Shift+Enter newline
    this._textarea.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        this._send();
      }
    });

    // Character counter
    this._textarea.addEventListener('input', () => this._updateCounter());
  }

  _send() {
    if (this.hasAttribute('disabled')) return;
    const text = this.value.trim();
    if (!text) return;
    if (text.length > this.maxLength) return;
    this.dispatchEvent(new CustomEvent('send', {
      bubbles: true,
      detail: { text },
    }));
    this.value = '';
  }

  _updateCounter() {
    if (!this._counter || !this._textarea) return;
    const len = (this._textarea.value || '').length;
    const max = this.maxLength;
    this._counter.textContent = `${len} / ${max}`;
    this._counter.classList.toggle('warn', len > max * 0.9);
  }

  _updateDisabled() {
    const disabled = this.hasAttribute('disabled');
    if (this._textarea) {
      if (disabled) this._textarea.setAttribute('disabled', '');
      else this._textarea.removeAttribute('disabled');
    }
    if (this._sendBtn) {
      if (disabled) this._sendBtn.setAttribute('disabled', '');
      else this._sendBtn.removeAttribute('disabled');
    }
  }

  // Focus the textarea programmatically
  focus() {
    if (this._textarea) this._textarea.focus();
  }

  _esc(str) {
    if (!str) return '';
    const d = document.createElement('div');
    d.textContent = str;
    return d.innerHTML;
  }
}

customElements.define('tf-chat-composer', TfChatComposer);
export { TfChatComposer };
