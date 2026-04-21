// =============================================================================
// Plik: tf-chip.js
// Opis: Komponent <tf-chip status="..." dot> — status pill.
//       Wspierane statusy: ok | warn | err | info | accent |
//       online | offline | pending (node states) |
//       scope-chat | scope-deploy | scope-mesh-read | scope-mesh-admin |
//       scope-trace | scope-license (API key scopes).
//       Light DOM + klasa .tf-chip, opcjonalna pulsujaca kropka.
// Przyklad: <tf-chip status="online" dot>Online</tf-chip>
// =============================================================================

const STATUS_CLASSES = new Set([
  'ok', 'warn', 'err', 'info', 'accent',
  'online', 'offline', 'pending',
  'scope-chat', 'scope-deploy', 'scope-mesh-read',
  'scope-mesh-admin', 'scope-trace', 'scope-license',
]);

class TfChip extends HTMLElement {
  static get observedAttributes() {
    return ['status', 'dot', 'clickable', 'active', 'icon'];
  }

  constructor() {
    super();
    this._span = null;
    this._label = '';
    this._onKeyDown = this._onKeyDown.bind(this);
  }

  connectedCallback() {
    if (!this._span) this._build();
    this.addEventListener('keydown', this._onKeyDown);
    this._update();
  }

  disconnectedCallback() {
    this.removeEventListener('keydown', this._onKeyDown);
  }

  attributeChangedCallback() {
    if (this._span) this._update();
  }

  _build() {
    this._label = this.innerHTML;
    this.innerHTML = '';
    const span = document.createElement('span');
    span.className = 'tf-chip';
    this.appendChild(span);
    this._span = span;
  }

  _update() {
    const status = (this.getAttribute('status') || 'info').toLowerCase();
    const hasDot = this.hasAttribute('dot');
    const icon = (this.getAttribute('icon') || '').trim();
    const cls = ['tf-chip'];
    if (STATUS_CLASSES.has(status)) cls.push(status);
    else cls.push('info');
    // Tryb klikalny — chip moze pelnic role filtra/togglera. Klasy 'clickable'
    // i 'active' sa stylowane przez controls.css lub CSS modulu uzywajacego.
    if (this.hasAttribute('clickable')) cls.push('clickable');
    if (this.hasAttribute('active')) cls.push('active');
    this._span.className = cls.join(' ');
    const parts = [];
    if (hasDot) parts.push('<span class="tf-chip-dot"></span>');
    if (icon) {
      parts.push(
        `<svg class="tf-chip-icon" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-${icon}"/></svg>`,
      );
    }
    parts.push(this._label);
    this._span.innerHTML = parts.join('');
    if (this.hasAttribute('clickable')) {
      this.setAttribute('role', 'button');
      this.setAttribute('tabindex', '0');
    } else {
      this.removeAttribute('role');
      this.removeAttribute('tabindex');
    }
  }

  _onKeyDown(e) {
    if (!this.hasAttribute('clickable')) return;
    if (e.key !== ' ' && e.key !== 'Enter') return;
    e.preventDefault();
    this.click();
  }
}

customElements.define('tf-chip', TfChip);
export { TfChip };
