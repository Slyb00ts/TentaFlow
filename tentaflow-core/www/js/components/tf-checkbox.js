// =============================================================================
// File: tf-checkbox.js
// Description: <tf-checkbox> — styled checkbox with checked, indeterminate and
//              disabled states. Emits 'change' with detail.checked.
// Example: <tf-checkbox label="Accept terms" checked></tf-checkbox>
// =============================================================================

class TfCheckbox extends HTMLElement {
  static get observedAttributes() { return ['checked', 'label', 'disabled', 'indeterminate']; }

  constructor() {
    super();
    this._root = null;
    this._input = null;
    this._textEl = null;
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

  get indeterminate() { return this.hasAttribute('indeterminate'); }
  set indeterminate(v) {
    if (v) this.setAttribute('indeterminate', '');
    else this.removeAttribute('indeterminate');
  }

  _build() {
    this.innerHTML = '';
    const label = document.createElement('label');
    label.className = 'tf-checkbox-label';
    label.addEventListener('click', this._onClick);
    label.addEventListener('keydown', this._onKey);

    const input = document.createElement('span');
    input.className = 'tf-checkbox-input';
    input.setAttribute('role', 'checkbox');
    input.setAttribute('tabindex', '0');

    const text = document.createElement('span');
    text.className = 'tf-checkbox-text';

    label.appendChild(input);
    label.appendChild(text);
    this.appendChild(label);
    this._root = label;
    this._input = input;
    this._textEl = text;
  }

  _update() {
    const labelText = this.getAttribute('label') || '';
    const disabled = this.hasAttribute('disabled');
    const checked = this.hasAttribute('checked');
    const indeterminate = this.hasAttribute('indeterminate');

    this._textEl.textContent = labelText;
    this._input.classList.toggle('checked', checked && !indeterminate);
    this._input.classList.toggle('indeterminate', indeterminate);
    this._root.classList.toggle('disabled', disabled);

    if (indeterminate) {
      this._input.setAttribute('aria-checked', 'mixed');
    } else {
      this._input.setAttribute('aria-checked', String(checked));
    }

    if (disabled) {
      this._input.setAttribute('tabindex', '-1');
      this._input.setAttribute('aria-disabled', 'true');
    } else {
      this._input.setAttribute('tabindex', '0');
      this._input.removeAttribute('aria-disabled');
    }
  }

  _onClick(e) {
    e.preventDefault();
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
    this.removeAttribute('indeterminate');
    const next = !this.hasAttribute('checked');
    if (next) this.setAttribute('checked', '');
    else this.removeAttribute('checked');
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { checked: next },
    }));
  }
}

customElements.define('tf-checkbox', TfCheckbox);
export { TfCheckbox };
