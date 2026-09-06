// =============================================================================
// File: tf-slider.js
// Description: <tf-slider> — styled range input. Emits 'input' (continuous) and
//              'change' (on release) with detail.value. Reflects .value property.
//              `aria-label` is forwarded to the inner range input, which is the
//              element a screen reader actually focuses.
// Example: <tf-slider min="0" max="100" value="50" step="1"></tf-slider>
// =============================================================================

class TfSlider extends HTMLElement {
  static get observedAttributes() { return ['min', 'max', 'value', 'step', 'disabled', 'aria-label']; }

  constructor() {
    super();
    this._input = null;
    this._onInput = this._onInput.bind(this);
    this._onChange = this._onChange.bind(this);
  }

  connectedCallback() {
    if (!this._input) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal) return;
    if (!this._input) return;
    if (name === 'value' && this._input.value !== newVal) {
      this._input.value = newVal ?? '0';
      this._updateTrackFill();
    }
    this._update();
  }

  get value() { return this._input ? this._input.value : (this.getAttribute('value') || '0'); }
  set value(v) {
    if (this._input) {
      this._input.value = v;
      this._updateTrackFill();
    }
    this.setAttribute('value', v);
  }

  _build() {
    this.innerHTML = '';
    const input = document.createElement('input');
    input.type = 'range';
    input.className = 'tf-slider';
    input.addEventListener('input', this._onInput);
    input.addEventListener('change', this._onChange);
    this.appendChild(input);
    this._input = input;
  }

  _update() {
    const min = this.getAttribute('min') || '0';
    const max = this.getAttribute('max') || '100';
    const step = this.getAttribute('step') || '1';
    const value = this.getAttribute('value') || '0';
    const disabled = this.hasAttribute('disabled');

    this._input.min = min;
    this._input.max = max;
    this._input.step = step;
    if (document.activeElement !== this._input) this._input.value = value;
    this._input.disabled = disabled;
    // The host is not the focusable element, so a label left on it would never
    // be announced.
    const label = this.getAttribute('aria-label');
    if (label) this._input.setAttribute('aria-label', label);
    else this._input.removeAttribute('aria-label');
    this._updateTrackFill();
  }

  _updateTrackFill() {
    const min = parseFloat(this._input.min) || 0;
    const max = parseFloat(this._input.max) || 100;
    const val = parseFloat(this._input.value) || 0;
    const pct = ((val - min) / (max - min)) * 100;
    this._input.style.setProperty('--tf-slider-pct', `${pct}%`);
  }

  _onInput() {
    this.setAttribute('value', this._input.value);
    this._updateTrackFill();
    this.dispatchEvent(new CustomEvent('input', {
      bubbles: true,
      detail: { value: this._input.value },
    }));
  }

  _onChange() {
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._input.value },
    }));
  }
}

customElements.define('tf-slider', TfSlider);
export { TfSlider };
