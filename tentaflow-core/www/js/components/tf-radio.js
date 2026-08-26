// =============================================================================
// File: tf-radio.js
// Description: <tf-radio> radio button and <tf-radio-group> container. Group
//              emits 'change' with detail.value when selection changes.
//              Opt-in variants: `hint` attribute renders helper text under the
//              label; `card` attribute on tf-radio (with `cards` on the group)
//              renders the option as a selectable card whose light-DOM children
//              become the card content; `orientation` on the group exposes the
//              .tf-radio-group__list layout hooks for SDK modifier classes.
// Example: <tf-radio-group name="color" value="red">
//            <tf-radio value="red" label="Red"></tf-radio>
//            <tf-radio value="blue" label="Blue"></tf-radio>
//          </tf-radio-group>
// The `nested` attribute on tf-radio-group opts into DESCENDANT (not just
// direct-child) tf-radio discovery and leaves caller-provided light-DOM
// markup untouched (no move-into-wrap) — for renderers that interleave
// section headings between radios (e.g. donor lists grouped by environment)
// and still need one logical group/selection across the whole list.
// =============================================================================

class TfRadio extends HTMLElement {
  static get observedAttributes() { return ['value', 'label', 'hint', 'disabled']; }

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
  get checked() { return this._input?.getAttribute('aria-checked') === 'true'; }

  _build() {
    if (this.hasAttribute('card')) {
      this._buildCard();
      return;
    }
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

    const hint = document.createElement('span');
    hint.className = 'tf-radio__hint';
    hint.style.display = 'none';

    label.appendChild(input);
    label.appendChild(text);
    label.appendChild(hint);
    this.appendChild(label);
    this._root = label;
    this._input = input;
    this._textEl = text;
    this._hintEl = hint;
  }

  // Card variant keeps renderer-provided light-DOM children (icon, title,
  // description, badge) and wraps them in a .tf-radio-card-group__card label.
  _buildCard() {
    const content = Array.from(this.childNodes);
    const label = document.createElement('label');
    label.className = 'tf-radio-card-group__card';
    label.addEventListener('click', this._onClick);
    label.addEventListener('keydown', this._onKey);

    const input = document.createElement('span');
    input.className = 'tf-radio-card-group__input';
    input.setAttribute('role', 'radio');
    input.setAttribute('tabindex', '0');

    label.appendChild(input);
    for (const node of content) label.appendChild(node);
    this.appendChild(label);
    this._root = label;
    this._input = input;
    this._textEl = null;
    this._hintEl = null;
  }

  _update() {
    const disabled = this.hasAttribute('disabled');
    const group = this.closest('tf-radio-group');
    const selected = group ? group.value === this.value : false;

    if (this.hasAttribute('card')) {
      this._root.classList.toggle('tf-radio-card-group__card--selected', selected);
      this._root.classList.toggle('tf-radio-card-group__card--disabled', disabled);
    } else {
      this._textEl.textContent = this.getAttribute('label') || '';
      const hintText = this.getAttribute('hint') || '';
      this._hintEl.textContent = hintText;
      this._hintEl.style.display = hintText ? '' : 'none';
      this._input.classList.toggle('checked', selected);
      this._root.classList.toggle('disabled', disabled);
    }
    this._input.setAttribute('aria-checked', String(selected));

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
    if (this.hasAttribute('nested')) {
      // Caller owns the light-DOM layout (section headings interleaved with
      // tf-radio elements at any depth) — track selection only, never move
      // nodes around. The non-nested path below sets `role="radiogroup"` on
      // the generated `wrap` div; nested mode has no such wrapper, so the
      // role has to land on the host itself or assistive tech never learns
      // this is a single logical radio group.
      this.setAttribute('role', 'radiogroup');
      this._wrap = this;
      this._labelEl = null;
      return;
    }
    const existing = Array.from(this.querySelectorAll(':scope > tf-radio'));
    const labelText = this.getAttribute('label') || '';

    // Only build wrapper if not present yet
    const label = document.createElement('span');
    label.className = 'tf-radio-group-label';
    label.textContent = labelText;
    label.style.display = labelText ? '' : 'none';

    const wrap = document.createElement('div');
    if (this.hasAttribute('cards')) {
      wrap.className = 'tf-radio-card-group';
    } else {
      wrap.className = 'tf-radio-group';
      // SDK RadioGroup orientation/density modifiers live on the host and
      // target .tf-radio-group__list descendants; expose the hook only when
      // the opt-in attribute is present so dashboard markup stays unchanged.
      if (this.hasAttribute('orientation')) wrap.classList.add('tf-radio-group__list');
    }
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
