// =============================================================================
// File: tf-avatar.js
// Opis: User avatar component. Shows an image when src is set, otherwise
//       renders initials with a toned background. Supports sm/md/lg sizes.
// =============================================================================

const VALID_SIZES = new Set(['sm', 'md', 'lg']);
const VALID_TONES = new Set(['accent', 'success', 'danger']);

class TfAvatar extends HTMLElement {
  static get observedAttributes() {
    return ['initials', 'size', 'tone', 'src'];
  }

  constructor() {
    super();
    this._root = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-avatar';
    this.appendChild(el);
    this._root = el;
  }

  _update() {
    const initials = (this.getAttribute('initials') || '').toUpperCase();
    const size = VALID_SIZES.has(this.getAttribute('size'))
      ? this.getAttribute('size')
      : 'md';
    const tone = VALID_TONES.has(this.getAttribute('tone'))
      ? this.getAttribute('tone')
      : 'accent';
    const src = (this.getAttribute('src') || '').trim();

    this._root.className = `tf-avatar ${size} ${tone}`;

    if (src) {
      this._root.innerHTML = `<img class="tf-avatar-img" src="${src}" alt="${initials}" />`;
    } else {
      this._root.textContent = initials;
    }
  }
}

customElements.define('tf-avatar', TfAvatar);
export { TfAvatar };
