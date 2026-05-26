// =============================================================================
// File: tf-toast.js
// Description: <tf-toast> — temporary notification popup. Use the static method
//              TfToast.show({tone, title, message, duration}) to create toasts
//              that auto-remove after a configurable duration.
// Example: TfToast.show({ tone: 'success', title: 'Saved', message: 'Changes applied.' });
// =============================================================================

class TfToast extends HTMLElement {
  static get observedAttributes() { return ['tone', 'title', 'message', 'duration']; }

  constructor() {
    super();
    this._root = null;
    this._timer = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
    this._startDismiss();
  }

  disconnectedCallback() {
    clearTimeout(this._timer);
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-toast';

    const titleEl = document.createElement('div');
    titleEl.className = 'tf-toast-title';

    const msgEl = document.createElement('div');
    msgEl.className = 'tf-toast-message';

    const closeBtn = document.createElement('button');
    closeBtn.className = 'tf-toast-close';
    closeBtn.type = 'button';
    closeBtn.textContent = '×';
    closeBtn.addEventListener('click', () => this._dismiss());

    el.appendChild(closeBtn);
    el.appendChild(titleEl);
    el.appendChild(msgEl);
    this.appendChild(el);
    this._root = el;
    this._titleEl = titleEl;
    this._msgEl = msgEl;
  }

  _update() {
    const tone = this.getAttribute('tone') || 'info';
    const title = this.getAttribute('title') || '';
    const message = this.getAttribute('message') || '';

    this._root.className = `tf-toast ${tone}`;
    this._titleEl.textContent = title;
    this._titleEl.style.display = title ? '' : 'none';
    this._msgEl.textContent = message;
    this._msgEl.style.display = message ? '' : 'none';
  }

  _startDismiss() {
    const duration = parseInt(this.getAttribute('duration'), 10) || 4000;
    this._timer = setTimeout(() => this._dismiss(), duration);
  }

  _dismiss() {
    clearTimeout(this._timer);
    if (!this._root) return;
    this._root.classList.add('tf-toast-out');
    this._root.addEventListener('animationend', () => this.remove(), { once: true });
  }

  // Ensure a singleton container exists in the DOM
  static _getContainer() {
    let c = document.getElementById('tf-toast-container');
    if (!c) {
      c = document.createElement('div');
      c.id = 'tf-toast-container';
      c.className = 'tf-toast-container';
      document.body.appendChild(c);
    }
    return c;
  }

  /**
   * Show a toast notification.
   * @param {Object} opts
   * @param {'success'|'danger'|'info'|'warning'} opts.tone
   * @param {string} [opts.title]
   * @param {string} [opts.message]
   * @param {number} [opts.duration=4000]
   */
  static show({ tone = 'info', title = '', message = '', duration = 4000 } = {}) {
    const container = TfToast._getContainer();
    const toast = document.createElement('tf-toast');
    if (tone) toast.setAttribute('tone', tone);
    if (title) toast.setAttribute('title', title);
    if (message) toast.setAttribute('message', message);
    toast.setAttribute('duration', String(duration));
    container.appendChild(toast);
    return toast;
  }
}

customElements.define('tf-toast', TfToast);
export { TfToast };
