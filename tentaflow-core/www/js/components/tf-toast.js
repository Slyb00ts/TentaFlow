// =============================================================================
// File: tf-toast.js
// Description: <tf-toast> — temporary notification popup. Use the static method
//              TfToast.show({tone, title, message, duration}) to create toasts
//              that auto-remove after a configurable duration.
// Example: TfToast.show({ tone: 'success', title: 'Saved', message: 'Changes applied.' });
//
// Declarative usage: append a <tf-toast> element with attributes yourself.
// The `persistent` boolean attribute disables the auto-dismiss timer (the
// owner controls the element lifecycle, e.g. SDK addon state). Children with
// slot="icon" / slot="action" are preserved and rendered before the title /
// after the message respectively.
// =============================================================================

import './tf-button.js';
import { I18n } from '/js/i18n.js';

class TfToast extends HTMLElement {
  static get observedAttributes() { return ['tone', 'title', 'message', 'duration', 'persistent']; }

  constructor() {
    super();
    this._root = null;
    this._timer = null;
    this._paused = false;
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
    // Preserve slotted icon/action children before clearing.
    const iconContent = this.querySelector('[slot="icon"]');
    const actionContent = this.querySelector('[slot="action"]');
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
    closeBtn.setAttribute('aria-label', I18n.t('common.close'));
    closeBtn.textContent = '×';
    closeBtn.addEventListener('click', () => this._dismiss());

    el.appendChild(closeBtn);
    if (iconContent) {
      iconContent.removeAttribute('slot');
      el.appendChild(iconContent);
    }
    el.appendChild(titleEl);
    el.appendChild(msgEl);
    if (actionContent) {
      actionContent.removeAttribute('slot');
      el.appendChild(actionContent);
    }
    this.appendChild(el);
    // A reader who is pointing at or tabbing within the toast is still reading it.
    this.addEventListener('mouseenter', () => this._pause());
    this.addEventListener('mouseleave', () => this._resume());
    this.addEventListener('focusin', () => this._pause());
    this.addEventListener('focusout', (event) => {
      if (!this.contains(event.relatedTarget)) this._resume();
    });
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

  // Restarts the dismiss timer after the toast's content was updated in place.
  refresh() {
    clearTimeout(this._timer);
    this._startDismiss();
  }

  _pause() {
    this._paused = true;
    clearTimeout(this._timer);
  }

  _resume() {
    this._paused = false;
    this._startDismiss();
  }

  _startDismiss() {
    if (this.hasAttribute('persistent') || this._paused) return;
    const duration = parseInt(this.getAttribute('duration'), 10) || 4000;
    this._timer = setTimeout(() => this._dismiss(), duration);
  }

  dismiss() { this._dismiss(); }

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
      // The live region exists before any toast is added to it, which is what
      // makes a screen reader announce the toast that arrives.
      c.setAttribute('role', 'status');
      c.setAttribute('aria-live', 'polite');
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
   * @param {{label: string, onClick: (toast: HTMLElement) => void}} [opts.action]
   *   A button after the message (e.g. "Undo"); the handler decides when the
   *   toast goes away, via `toast.dismiss()`.
   */
  static show({ tone = 'info', title = '', message = '', duration = 4000, action = null } = {}) {
    const container = TfToast._getContainer();
    const toast = document.createElement('tf-toast');
    if (action) {
      const button = document.createElement('tf-button');
      button.setAttribute('slot', 'action');
      button.setAttribute('variant', 'ghost');
      button.setAttribute('size', 'sm');
      button.setAttribute('label', action.label);
      button.addEventListener('click', () => action.onClick(toast));
      toast.appendChild(button);
    }
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
