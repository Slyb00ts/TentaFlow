// =============================================================================
// File: tf-color-input.js
// Description: <tf-color-input> — color picker with swatch preview.
//   Attributes: value (hex), label, disabled.
//   Events: change (detail: {value}).
// =============================================================================

class TfColorInput extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'label', 'disabled'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._swatch = null;
    this._input = null;
    this._labelEl = null;
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._update();
  }

  get value() {
    return this._input ? this._input.value : (this.getAttribute('value') || '#000000');
  }

  set value(v) {
    if (this._input) this._input.value = v ?? '#000000';
    this.setAttribute('value', v ?? '#000000');
    if (this._swatch) this._swatch.style.backgroundColor = v ?? '#000000';
  }

  _build() {
    this.innerHTML = '';

    const wrap = document.createElement('div');
    wrap.className = 'tf-color-input';

    const labelEl = document.createElement('span');
    labelEl.className = 'tf-color-input-label';
    wrap.appendChild(labelEl);

    const row = document.createElement('div');
    row.className = 'tf-color-input-row';

    const swatch = document.createElement('button');
    swatch.type = 'button';
    swatch.className = 'tf-color-swatch';
    swatch.setAttribute('aria-label', 'Pick color');
    swatch.addEventListener('click', () => {
      if (this.hasAttribute('disabled')) return;
      this._input.click();
    });
    row.appendChild(swatch);

    const input = document.createElement('input');
    input.type = 'color';
    input.className = 'tf-color-input-native';
    input.style.position = 'absolute';
    input.style.opacity = '0';
    input.style.width = '0';
    input.style.height = '0';
    input.style.pointerEvents = 'none';
    input.addEventListener('input', () => {
      swatch.style.backgroundColor = input.value;
      this.setAttribute('value', input.value);
    });
    input.addEventListener('change', (e) => {
      e.stopPropagation();
      swatch.style.backgroundColor = input.value;
      this.setAttribute('value', input.value);
      this.dispatchEvent(new CustomEvent('change', {
        bubbles: true,
        detail: { value: input.value },
      }));
    });
    row.appendChild(input);

    const hexDisplay = document.createElement('span');
    hexDisplay.className = 'tf-color-input-hex';
    row.appendChild(hexDisplay);

    wrap.appendChild(row);
    this.appendChild(wrap);

    this._wrap = wrap;
    this._swatch = swatch;
    this._input = input;
    this._labelEl = labelEl;
    this._hexDisplay = hexDisplay;
  }

  _update() {
    const value = this.getAttribute('value') || '#000000';
    const label = this.getAttribute('label') || '';
    const disabled = this.hasAttribute('disabled');

    this._input.value = value;
    this._swatch.style.backgroundColor = value;
    this._hexDisplay.textContent = value;

    this._labelEl.textContent = label;
    this._labelEl.style.display = label ? '' : 'none';

    this._swatch.disabled = disabled;
    this._input.disabled = disabled;
  }
}

customElements.define('tf-color-input', TfColorInput);
export { TfColorInput };
