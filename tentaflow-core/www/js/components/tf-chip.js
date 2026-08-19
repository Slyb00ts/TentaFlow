// =============================================================================
// Plik: tf-chip.js
// Opis: Komponent <tf-chip status="..." dot> — status pill.
//       Wspierane statusy: ok | warn | err | info | accent |
//       online | offline | pending | recording (node / capture states) |
//       scope-chat | scope-deploy | scope-mesh-read | scope-mesh-admin |
//       scope-trace | scope-license (API key scopes).
//       Light DOM + klasa .tf-chip, opcjonalna pulsujaca kropka.
//       The `label` attribute overrides the text (reactive updates via
//       setAttribute), `removable` adds a × button emitting a `remove` event,
//       and a child with slot="lead" (e.g. avatar) is kept before the label.
//       `variant="tag"` + `tone` + `size` renders a static tag (.tf-tag--*
//       classes) instead of the status pill.
//       `icon="<sprite-id>"` puts a 12px leading glyph before the label and
//       `mono` switches the chip to the monospace face with normal casing —
//       the pair used for machine-readable context (branch name, mode, profile)
//       that must stay legible character by character.
// Przyklad: <tf-chip status="online" dot>Online</tf-chip>
//           <tf-chip mono icon="branch">cs/piotr/9f2a1c4b</tf-chip>
// =============================================================================

const STATUS_CLASSES = new Set([
  'ok', 'warn', 'err', 'info', 'accent', 'neutral',
  'online', 'offline', 'pending', 'recording',
  'scope-chat', 'scope-deploy', 'scope-mesh-read',
  'scope-mesh-admin', 'scope-trace', 'scope-license',
]);

// Tones for the `dot-tone` attribute — dot color independent of chip status.
const DOT_TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);

const TAG_TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);
const TAG_SIZES = new Set(['xs', 'sm', 'md']);

function safeIconName(value) {
  const text = String(value || '').trim();
  return /^[a-z0-9_-]{1,64}$/i.test(text) ? text : '';
}

class TfChip extends HTMLElement {
  static get observedAttributes() {
    return [
      'status', 'dot', 'dot-tone', 'clickable', 'active', 'icon',
      'label', 'variant', 'tone', 'size', 'removable', 'mono',
    ];
  }

  constructor() {
    super();
    this._span = null;
    this._label = '';
    this._lead = null;
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    if (!this._span) this._build();
    this.addEventListener('keydown', this._onKeyDown);
    this.addEventListener('click', this._onClick);
    this._update();
  }

  disconnectedCallback() {
    this.removeEventListener('keydown', this._onKeyDown);
    this.removeEventListener('click', this._onClick);
  }

  attributeChangedCallback() {
    if (this._span) this._update();
  }

  _build() {
    // Element children marked slot="lead" (avatar etc.) are preserved and
    // re-inserted before the label; remaining text becomes the default label.
    for (const child of [...this.children]) {
      if (child.getAttribute('slot') === 'lead') {
        this._lead = child;
        child.remove();
      }
    }
    this._label = this.textContent;
    this.innerHTML = '';
    const span = document.createElement('span');
    span.className = 'tf-chip';
    this.appendChild(span);
    this._span = span;
  }

  _update() {
    const span = this._span;
    const label = this.hasAttribute('label')
      ? this.getAttribute('label')
      : this._label;
    span.textContent = '';

    if ((this.getAttribute('variant') || '') === 'tag') {
      // Static read-only tag variant — reuses .tf-tag styling, label only.
      const tone = (this.getAttribute('tone') || '').toLowerCase();
      const size = (this.getAttribute('size') || '').toLowerCase();
      const cls = ['tf-tag'];
      if (TAG_TONES.has(tone)) cls.push(`tf-tag--tone-${tone}`);
      if (TAG_SIZES.has(size)) cls.push(`tf-tag--size-${size}`);
      span.className = cls.join(' ');
      span.appendChild(document.createTextNode(label));
      this.removeAttribute('role');
      this.removeAttribute('tabindex');
      return;
    }

    const status = (this.getAttribute('status') || 'info').toLowerCase();
    const hasDot = this.hasAttribute('dot');
    const icon = safeIconName(this.getAttribute('icon'));
    const cls = ['tf-chip'];
    if (STATUS_CLASSES.has(status)) cls.push(status);
    else cls.push('info');
    // Tryb klikalny — chip moze pelnic role filtra/togglera. Klasy 'clickable'
    // i 'active' sa stylowane przez controls.css lub CSS modulu uzywajacego.
    if (this.hasAttribute('clickable')) cls.push('clickable');
    if (this.hasAttribute('active')) cls.push('active');
    // Monospace face + normal casing: identifiers must not be upper-cased or
    // letter-spaced, or a branch name stops being copy-readable.
    if (this.hasAttribute('mono')) cls.push('tf-chip--mono');
    span.className = cls.join(' ');

    if (hasDot) {
      const dot = document.createElement('span');
      dot.className = 'tf-chip-dot';
      const dotTone = (this.getAttribute('dot-tone') || '').toLowerCase();
      if (DOT_TONES.has(dotTone)) dot.classList.add(`tf-chip-dot--tone-${dotTone}`);
      span.appendChild(dot);
    }
    if (icon) {
      const tmp = document.createElement('span');
      tmp.innerHTML =
        `<svg class="tf-chip-icon" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-${icon}"/></svg>`;
      span.appendChild(tmp.firstChild);
    }
    if (this._lead) span.appendChild(this._lead);
    span.appendChild(document.createTextNode(label));
    if (this.hasAttribute('removable')) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'tf-chip__remove';
      btn.setAttribute('aria-label', 'Remove');
      btn.textContent = '×';
      span.appendChild(btn);
    }

    if (this.hasAttribute('clickable')) {
      this.setAttribute('role', 'button');
      this.setAttribute('tabindex', '0');
    } else {
      this.removeAttribute('role');
      this.removeAttribute('tabindex');
    }
  }

  _onClick(e) {
    const target = e.target;
    if (target && target.closest && target.closest('.tf-chip__remove')) {
      e.stopPropagation();
      this.dispatchEvent(new CustomEvent('remove'));
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
