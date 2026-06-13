// =============================================================================
// File: tf-spinner.js
// Description: <tf-spinner size="md" variant="default" tone="primary"> — loading
//              spinner. Renders a light-DOM container with the variant subtree
//              (circle / dots / bars) styled by controls.css .tf-spinner--*.
// Example: <tf-spinner size="lg" variant="dots" tone="success"></tf-spinner>
// =============================================================================

const VALID_VARIANTS = new Set(['default', 'ring', 'dots', 'bars']);
const VALID_TONES = new Set([
  'neutral', 'primary', 'info', 'success', 'warning', 'critical', 'muted',
]);
const VALID_SIZES = new Set(['xs', 'sm', 'md', 'lg', 'xl']);

class TfSpinner extends HTMLElement {
  static get observedAttributes() {
    return ['size', 'variant', 'tone'];
  }

  constructor() {
    super();
    this._el = null;
  }

  connectedCallback() {
    if (!this._el) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._el) this._update();
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.setAttribute('role', 'status');
    el.setAttribute('aria-label', this.getAttribute('aria-label') || 'Loading');
    this.appendChild(el);
    this._el = el;
  }

  _update() {
    const size = VALID_SIZES.has(this.getAttribute('size'))
      ? this.getAttribute('size')
      : 'md';
    const variant = VALID_VARIANTS.has(this.getAttribute('variant'))
      ? this.getAttribute('variant')
      : 'default';
    const tone = VALID_TONES.has(this.getAttribute('tone'))
      ? this.getAttribute('tone')
      : 'primary';

    this._el.className =
      `tf-spinner tf-spinner--${variant} tf-spinner--size-${size} tf-spinner--tone-${tone}`;

    let parts;
    if (variant === 'dots') {
      parts = '<span class="tf-spinner__dot"></span>'.repeat(3);
    } else if (variant === 'bars') {
      parts = '<span class="tf-spinner__bar"></span>'.repeat(4);
    } else {
      parts = '<span class="tf-spinner__circle"></span>';
    }
    this._el.innerHTML = parts;
  }
}

customElements.define('tf-spinner', TfSpinner);
export { TfSpinner };
