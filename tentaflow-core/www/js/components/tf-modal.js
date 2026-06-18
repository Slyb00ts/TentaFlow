// =============================================================================
// File: tf-modal.js
// Description: <tf-modal> — modal/drawer overlay component.
//   Attributes: open (boolean), title, subtitle, variant (modal|drawer-right|
//   drawer-left|drawer-bottom|drawer-top), size (xs|sm|md|lg|xl|fullscreen),
//   no-dismiss (boolean — ESC/backdrop do not close), no-close (boolean —
//   hides the header close button).
//   Events: close.
//   Static: TfModal.open({title, body, actions}) → Promise.
// =============================================================================

// Footer action buttons in the static .open() API are tf-button elements.
import './tf-button.js';

const MODAL_SIZE_CLASSES = [
  'tf-modal--size-xs', 'tf-modal--size-sm', 'tf-modal--size-md',
  'tf-modal--size-lg', 'tf-modal--size-xl', 'tf-modal--size-fullscreen',
];

class TfModal extends HTMLElement {
  static get observedAttributes() {
    return ['open', 'title', 'subtitle', 'variant', 'size', 'no-dismiss', 'no-close'];
  }

  constructor() {
    super();
    this._backdrop = null;
    this._card = null;
    this._header = null;
    this._body = null;
    this._footer = null;
    this._titleEl = null;
    this._closeBtn = null;
    this._onEsc = this._onEsc.bind(this);
    this._onBackdropClick = this._onBackdropClick.bind(this);
  }

  connectedCallback() {
    if (!this._backdrop) this._build();
    this._update();
  }

  disconnectedCallback() {
    document.removeEventListener('keydown', this._onEsc);
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._backdrop) return;
    this._update();
  }

  get open() { return this.hasAttribute('open'); }
  set open(v) {
    if (v) this.setAttribute('open', '');
    else this.removeAttribute('open');
  }

  _build() {
    // Preserve slot children before clearing.
    const bodyContent = this.querySelector('[slot="body"]');
    const footerContent = this.querySelector('[slot="footer"]');
    this.innerHTML = '';

    const backdrop = document.createElement('div');
    backdrop.className = 'tf-modal-backdrop';
    backdrop.addEventListener('click', this._onBackdropClick);

    const card = document.createElement('div');
    card.className = 'tf-modal-card';
    card.setAttribute('role', 'dialog');
    card.setAttribute('aria-modal', 'true');

    const header = document.createElement('div');
    header.className = 'tf-modal-header';

    const titleEl = document.createElement('h2');
    titleEl.className = 'tf-modal-title';
    header.appendChild(titleEl);

    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'tf-modal-close';
    closeBtn.setAttribute('aria-label', 'Close');
    closeBtn.textContent = '×';
    closeBtn.addEventListener('click', () => this._dismiss());
    header.appendChild(closeBtn);

    card.appendChild(header);

    // Optional subtitle line rendered below the header; hidden when the
    // `subtitle` attribute is absent so the default layout is unchanged.
    const subtitleEl = document.createElement('p');
    subtitleEl.className = 'tf-modal-subtitle';
    subtitleEl.style.display = 'none';
    card.appendChild(subtitleEl);

    const body = document.createElement('div');
    body.className = 'tf-modal-body';
    if (bodyContent) {
      bodyContent.removeAttribute('slot');
      body.appendChild(bodyContent);
    }
    card.appendChild(body);

    const footer = document.createElement('div');
    footer.className = 'tf-modal-footer';
    if (footerContent) {
      footerContent.removeAttribute('slot');
      footer.appendChild(footerContent);
    }
    card.appendChild(footer);

    backdrop.appendChild(card);
    this.appendChild(backdrop);

    this._backdrop = backdrop;
    this._card = card;
    this._header = header;
    this._body = body;
    this._footer = footer;
    this._titleEl = titleEl;
    this._subtitleEl = subtitleEl;
    this._closeBtn = closeBtn;
  }

  _update() {
    const isOpen = this.hasAttribute('open');
    const title = this.getAttribute('title') || '';
    const subtitle = this.getAttribute('subtitle') || '';
    const variant = this.getAttribute('variant') || 'modal';
    const size = this.getAttribute('size') || '';

    this._titleEl.textContent = title;

    this._subtitleEl.textContent = subtitle;
    this._subtitleEl.style.display = subtitle ? '' : 'none';

    this._closeBtn.style.display = this.hasAttribute('no-close') ? 'none' : '';

    // Remove old variant classes.
    this._card.classList.remove(
      'tf-modal-card--modal', 'tf-modal-card--drawer-right',
      'tf-modal-card--drawer-left', 'tf-modal-card--drawer-bottom',
      'tf-modal-card--drawer-top'
    );
    this._card.classList.add(`tf-modal-card--${variant}`);

    // Optional size class; reuses the shared .tf-modal--size-* width rules.
    this._card.classList.remove(...MODAL_SIZE_CLASSES);
    if (size) this._card.classList.add(`tf-modal--size-${size}`);

    if (isOpen) {
      this._backdrop.classList.add('tf-modal-backdrop--open');
      this._card.classList.add('tf-modal-card--open');
      document.addEventListener('keydown', this._onEsc);
    } else {
      this._backdrop.classList.remove('tf-modal-backdrop--open');
      this._card.classList.remove('tf-modal-card--open');
      document.removeEventListener('keydown', this._onEsc);
    }
  }

  _dismiss() {
    this.removeAttribute('open');
    this.dispatchEvent(new CustomEvent('close', { bubbles: true }));
  }

  _onEsc(e) {
    if (this.hasAttribute('no-dismiss')) return;
    if (e.key === 'Escape') this._dismiss();
  }

  _onBackdropClick(e) {
    if (this.hasAttribute('no-dismiss')) return;
    if (e.target === this._backdrop) this._dismiss();
  }

  // -- Static imperative API --

  static open({ title = '', body = '', actions = [] } = {}) {
    return new Promise((resolve) => {
      const modal = document.createElement('tf-modal');
      modal.setAttribute('title', title);
      modal.setAttribute('variant', 'modal');

      const bodySlot = document.createElement('div');
      bodySlot.setAttribute('slot', 'body');
      if (typeof body === 'string') bodySlot.textContent = body;
      else if (body instanceof HTMLElement) bodySlot.appendChild(body);
      modal.appendChild(bodySlot);

      if (actions.length > 0) {
        const footerSlot = document.createElement('div');
        footerSlot.setAttribute('slot', 'footer');
        for (const action of actions) {
          const btn = document.createElement('tf-button');
          btn.setAttribute('variant', action.primary ? 'primary' : 'secondary');
          btn.textContent = action.label || '';
          btn.addEventListener('click', () => {
            resolve(action.value ?? action.label);
            modal.removeAttribute('open');
            setTimeout(() => modal.remove(), 300);
          });
          footerSlot.appendChild(btn);
        }
        modal.appendChild(footerSlot);
      }

      document.body.appendChild(modal);
      modal.setAttribute('open', '');

      modal.addEventListener('close', () => {
        resolve(null);
        setTimeout(() => modal.remove(), 300);
      }, { once: true });
    });
  }
}

customElements.define('tf-modal', TfModal);
export { TfModal };
