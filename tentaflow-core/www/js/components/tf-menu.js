// =============================================================================
// Plik: tf-menu.js
// Opis: Komponent <tf-menu> — kontekstowe menu z elementami <tf-menu-item>
//       i <tf-menu-divider>. Shadow DOM (open). Animacja Apple-style:
//       scale 0.9 -> 1 + blur-in 8px -> 0 + stagger itemow co 25ms.
//       Click-outside zamyka. Metody .open()/.close() oraz atrybut "open".
// Przyklad:
//   <tf-menu placement="bottom-start">
//     <tf-menu-item action="edit" icon="edit">Edytuj</tf-menu-item>
//     <tf-menu-divider></tf-menu-divider>
//     <tf-menu-item action="delete" icon="trash" danger>Usun</tf-menu-item>
//   </tf-menu>
// =============================================================================

import { adoptControlsInto } from './shared-styles.js';
import { Sfx } from '/js/lib/sfx.js';

class TfMenuItem extends HTMLElement {
  static get observedAttributes() {
    return ['icon', 'danger', 'action'];
  }

  constructor() {
    super();
    this._btn = null;
    this._label = '';
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    if (!this._btn) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._btn) this._update();
  }

  _build() {
    this._label = this.innerHTML;
    this.innerHTML = '';
    // w Shadow DOM rodzica — tu budujemy w light DOM itemu,
    // rodzic przeniesie go do shadow slot gdy otwiera
    const el = document.createElement('div');
    el.className = 'tf-menu-item';
    el.setAttribute('role', 'menuitem');
    el.tabIndex = 0;
    el.addEventListener('click', this._onClick);
    this.appendChild(el);
    this._btn = el;
  }

  _update() {
    const icon = this.getAttribute('icon');
    const danger = this.hasAttribute('danger');
    this._btn.classList.toggle('danger', danger);
    const iconHtml = icon
      ? `<svg width="14" height="14" aria-hidden="true"><use href="#i-${icon}"/></svg>`
      : '';
    this._btn.innerHTML = `${iconHtml}<span>${this._label}</span>`;
  }

  _onClick() {
    const action = this.getAttribute('action') || '';
    this.dispatchEvent(new CustomEvent('tf-menu-select', {
      bubbles: true,
      composed: true,
      detail: { action, item: this },
    }));
  }
}
customElements.define('tf-menu-item', TfMenuItem);

class TfMenuDivider extends HTMLElement {
  connectedCallback() {
    if (!this.firstElementChild) {
      const el = document.createElement('div');
      el.className = 'tf-menu-divider';
      this.appendChild(el);
    }
  }
}
customElements.define('tf-menu-divider', TfMenuDivider);

class TfMenu extends HTMLElement {
  static get observedAttributes() {
    return ['open', 'placement'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._box = null;
    this._staggerTimers = [];
    this._onDocClick = this._onDocClick.bind(this);
    this._onSelect = this._onSelect.bind(this);
    this._onKey = this._onKey.bind(this);
  }

  connectedCallback() {
    if (!this._box) this._build();
    this._update();
    document.addEventListener('pointerdown', this._onDocClick, true);
    document.addEventListener('keydown', this._onKey);
    this.addEventListener('tf-menu-select', this._onSelect);
  }

  disconnectedCallback() {
    document.removeEventListener('pointerdown', this._onDocClick, true);
    document.removeEventListener('keydown', this._onKey);
    this.removeEventListener('tf-menu-select', this._onSelect);
    this._clearStagger();
  }

  attributeChangedCallback(name) {
    if (!this._box) return;
    if (name === 'open') this._update();
    if (name === 'placement') this._update();
  }

  open() { this.setAttribute('open', ''); }
  close() { this.removeAttribute('open'); }
  toggle() { if (this.hasAttribute('open')) this.close(); else this.open(); }

  _build() {
    adoptControlsInto(this._shadow);
    const box = document.createElement('div');
    box.className = 'tf-menu';
    box.setAttribute('role', 'menu');
    const slot = document.createElement('slot');
    box.appendChild(slot);
    this._shadow.appendChild(box);
    this._box = box;
  }

  _update() {
    const placement = this.getAttribute('placement') || 'bottom-start';
    this._box.setAttribute('data-placement', placement);
    const isOpen = this.hasAttribute('open');
    if (isOpen) {
      if (!this._wasOpen) Sfx.play('menu-open');
      this._wasOpen = true;
      this._box.classList.add('open');
      this._applyStagger();
      this.dispatchEvent(new CustomEvent('open', { bubbles: true }));
    } else {
      if (this._wasOpen) Sfx.play('menu-close');
      this._wasOpen = false;
      this._box.classList.remove('open');
      this._clearStagger();
      this.dispatchEvent(new CustomEvent('close', { bubbles: true }));
    }
  }

  _applyStagger() {
    const items = Array.from(this.querySelectorAll(':scope > tf-menu-item'));
    this._clearStagger();
    items.forEach((it, i) => {
      const delay = i * 25;
      // delay jest ustawiany inline; CSS rozni sie dla open/close
      it.style.transitionDelay = `${delay}ms`;
    });
  }

  _clearStagger() {
    this._staggerTimers.forEach((t) => clearTimeout(t));
    this._staggerTimers = [];
    const items = Array.from(this.querySelectorAll(':scope > tf-menu-item'));
    items.forEach((it) => { it.style.transitionDelay = '0ms'; });
  }

  _onDocClick(e) {
    if (!this.hasAttribute('open')) return;
    const path = e.composedPath();
    if (path.includes(this)) return;
    this.close();
  }

  _onKey(e) {
    if (!this.hasAttribute('open')) return;
    if (e.key === 'Escape') {
      e.stopPropagation();
      this.close();
    }
  }

  _onSelect(e) {
    this.close();
    this.dispatchEvent(new CustomEvent('action', {
      bubbles: true,
      detail: { action: e.detail.action },
    }));
  }
}

customElements.define('tf-menu', TfMenu);
export { TfMenu, TfMenuItem, TfMenuDivider };
