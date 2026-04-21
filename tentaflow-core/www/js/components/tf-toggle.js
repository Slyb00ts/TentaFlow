// =============================================================================
// Plik: tf-toggle.js
// Opis: Komponent <tf-toggle> — switch on/off. Klik/space/enter przelacza.
//       Emituje "change" z detail.checked. Reflektuje atrybut "checked".
// Przyklad: <tf-toggle checked></tf-toggle>
// =============================================================================

import { Sfx } from '/js/lib/sfx.js';

class TfToggle extends HTMLElement {
  static get observedAttributes() {
    return ['checked', 'disabled'];
  }

  constructor() {
    super();
    this._root = null;
    this._onClick = this._onClick.bind(this);
    this._onKey = this._onKey.bind(this);
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  get checked() { return this.hasAttribute('checked'); }
  set checked(v) {
    if (v) this.setAttribute('checked', '');
    else this.removeAttribute('checked');
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('span');
    el.className = 'tf-toggle';
    el.setAttribute('role', 'switch');
    el.setAttribute('tabindex', '0');
    el.addEventListener('click', this._onClick);
    el.addEventListener('keydown', this._onKey);
    this.appendChild(el);
    this._root = el;
  }

  _update() {
    const on = this.hasAttribute('checked');
    const disabled = this.hasAttribute('disabled');
    this._root.classList.toggle('on', on);
    this._root.setAttribute('aria-checked', String(on));
    if (disabled) {
      this._root.setAttribute('aria-disabled', 'true');
      this._root.setAttribute('tabindex', '-1');
    } else {
      this._root.removeAttribute('aria-disabled');
      this._root.setAttribute('tabindex', '0');
    }
  }

  _onClick() {
    if (this.hasAttribute('disabled')) return;
    this._toggle();
  }

  _onKey(e) {
    if (this.hasAttribute('disabled')) return;
    if (e.key === ' ' || e.key === 'Enter') {
      e.preventDefault();
      this._toggle();
    }
  }

  _toggle() {
    const next = !this.hasAttribute('checked');
    if (next) this.setAttribute('checked', '');
    else this.removeAttribute('checked');
    Sfx.play('toggle');
    // ripple FX
    this._root.classList.remove('tf-ripple');
    // reflow zeby restart animacji
    void this._root.offsetWidth;
    this._root.classList.add('tf-ripple');
    setTimeout(() => this._root?.classList.remove('tf-ripple'), 500);
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { checked: next },
    }));
  }
}

customElements.define('tf-toggle', TfToggle);
export { TfToggle };
