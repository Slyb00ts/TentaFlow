// =============================================================================
// File: tf-radio.js
// Description: <tf-radio> radio button and <tf-radio-group> container. Group
//              emits 'change' with detail.value when selection changes.
// Example: <tf-radio-group name="color" value="red">
//            <tf-radio value="red" label="Red"></tf-radio>
//            <tf-radio value="blue" label="Blue"></tf-radio>
//          </tf-radio-group>
// =============================================================================

class TfRadio extends HTMLElement {
  static get observedAttributes() { return ['value', 'label', 'disabled']; }

  constructor() {
    super();
    this._root = null;
    this._input = null;
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

  get value() { return this.getAttribute('value') || ''; }
  get name() { return this.getAttribute('name') || ''; }
  get checked() { return this._input?.classList.contains('checked') ?? false; }

  _build() {
    this.innerHTML = '';
    const label = document.createElement('label');
    label.className = 'tf-radio-label';
    label.addEventListener('click', this._onClick);
    label.addEventListener('keydown', this._onKey);

    const input = document.createElement('span');
    input.className = 'tf-radio-input';
    input.setAttribute('role', 'radio');
    input.setAttribute('tabindex', '0');

    const text = document.createElement('span');
    text.className = 'tf-radio-text';

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
    const group = this.closest('tf-radio-group');
    const selected = group ? group.value === this.value : false;

    this._textEl.textContent = labelText;
    this._input.classList.toggle('checked', selected);
    this._input.setAttribute('aria-checked', String(selected));
    this._root.classList.toggle('disabled', disabled);

    if (disabled) {
      this._input.setAttribute('tabindex', '-1');
      this._input.setAttribute('aria-disabled', 'true');
    } else {
      this._input.setAttribute('tabindex', selected ? '0' : '-1');
      this._input.removeAttribute('aria-disabled');
    }
  }

  _onClick(e) {
    e.preventDefault();
    if (this.hasAttribute('disabled')) return;
    const group = this.closest('tf-radio-group');
    if (group) group.value = this.value;
  }

  _onKey(e) {
    if (this.hasAttribute('disabled')) return;
    if (e.key === ' ' || e.key === 'Enter') {
      e.preventDefault();
      this._onClick(e);
    }
  }
}

class TfRadioGroup extends HTMLElement {
  static get observedAttributes() { return ['name', 'value', 'label']; }

  constructor() {
    super();
    this._labelEl = null;
    this._wrap = null;
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._sync();
  }

  attributeChangedCallback() {
    if (this._wrap) this._sync();
  }

  get value() { return this.getAttribute('value') || ''; }
  set value(v) {
    if (v === this.value) return;
    this.setAttribute('value', v);
    this._sync();
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: v },
    }));
  }

  _build() {
    const existing = Array.from(this.querySelectorAll(':scope > tf-radio'));
    const labelText = this.getAttribute('label') || '';

    // Only build wrapper if not present yet
    const label = document.createElement('span');
    label.className = 'tf-radio-group-label';
    label.textContent = labelText;
    label.style.display = labelText ? '' : 'none';

    const wrap = document.createElement('div');
    wrap.className = 'tf-radio-group';
    wrap.setAttribute('role', 'radiogroup');

    this.prepend(label);
    // Move radios into wrap
    for (const radio of existing) wrap.appendChild(radio);
    this.appendChild(wrap);
    this._labelEl = label;
    this._wrap = wrap;
  }

  _sync() {
    if (this._labelEl) {
      const labelText = this.getAttribute('label') || '';
      this._labelEl.textContent = labelText;
      this._labelEl.style.display = labelText ? '' : 'none';
    }
    const radios = this.querySelectorAll('tf-radio');
    for (const r of radios) r._update?.();
  }
}

customElements.define('tf-radio', TfRadio);
customElements.define('tf-radio-group', TfRadioGroup);
export { TfRadio, TfRadioGroup };
