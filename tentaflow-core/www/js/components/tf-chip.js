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
    return ['status', 'dot'];
  }

  constructor() {
    super();
    this._span = null;
    this._label = '';
  }

  connectedCallback() {
    if (!this._span) this._build();
    this._update();
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
    const cls = ['tf-chip'];
    if (STATUS_CLASSES.has(status)) cls.push(status);
    else cls.push('info');
    this._span.className = cls.join(' ');
    this._span.innerHTML = hasDot
      ? `<span class="tf-chip-dot"></span>${this._label}`
      : this._label;
  }
}

customElements.define('tf-chip', TfChip);
export { TfChip };
