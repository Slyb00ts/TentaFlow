// =============================================================================
// Plik: tf-select.js
// Opis: Komponent <tf-select> — wraper nad natywnym <select>. Dzieci <option>
//       sa przejmowane i umieszczane w select. Emituje "change" z detail.value.
// Przyklad: <tf-select value="rr"><option value="fa">First</option>...</tf-select>
// =============================================================================

class TfSelect extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'disabled', 'name', 'label'];
  }

  constructor() {
    super();
    this._group = null;
    this._labelEl = null;
    this._wrap = null;
    this._select = null;
    this._onChange = this._onChange.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    if (name === 'value' && this._select) this._select.value = newVal || '';
    this._update();
  }

  get value() { return this._select ? this._select.value : this.getAttribute('value'); }
  set value(v) {
    if (this._select) this._select.value = v ?? '';
    this.setAttribute('value', v ?? '');
  }

  // Replaces the inner <select> options at runtime (the light-DOM <option>
  // children are consumed at build time, so callers that fetch options async
  // must use this instead of re-setting innerHTML). `list` is [{value,label}];
  // `selected` keeps the current pick when present in the new list.
  setOptions(list, selected) {
    if (!this._select) this._build();
    this._select.innerHTML = '';
    for (const o of list || []) {
      const opt = document.createElement('option');
      opt.value = o.value ?? '';
      opt.textContent = o.label ?? String(o.value ?? '');
      if (o.disabled) opt.disabled = true;
      this._select.appendChild(opt);
    }
    if (selected != null) {
      this._select.value = String(selected);
      this.setAttribute('value', String(selected));
    }
  }

  _build() {
    // przejmij <option> z light DOM i przenies do wewnetrznego <select>
    const options = Array.from(this.querySelectorAll('option'));
    this.innerHTML = '';

    // Reuse the tf-input group/label structure so an optional label looks and
    // aligns identically to tf-input (same `.tf-input-group` + `.tf-label` CSS).
    const group = document.createElement('div');
    group.className = 'tf-input-group';

    const label = document.createElement('span');
    label.className = 'tf-label';
    group.appendChild(label);

    const wrap = document.createElement('div');
    wrap.className = 'tf-select-wrap';

    const select = document.createElement('select');
    select.className = 'tf-select';
    options.forEach((opt) => select.appendChild(opt));
    select.addEventListener('change', this._onChange);

    wrap.appendChild(select);
    group.appendChild(wrap);
    this.appendChild(group);

    this._group = group;
    this._labelEl = label;
    this._wrap = wrap;
    this._select = select;
  }

  _update() {
    if (this.hasAttribute('value')) {
      this._select.value = this.getAttribute('value');
    }
    this._select.disabled = this.hasAttribute('disabled');
    const name = this.getAttribute('name');
    if (name) this._select.name = name;
    const labelText = this.getAttribute('label') || '';
    this._labelEl.textContent = labelText;
    this._labelEl.style.display = labelText ? '' : 'none';
  }

  _onChange(e) {
    // Native <select> emituje wlasny `change` event ktory bubbles przez
    // light DOM tf-select'a. Bez stopPropagation caller (np. login.js)
    // dostawal DWA eventy: pierwszy native (bez detail) -> crash przy
    // e.detail.value, drugi CustomEvent z detail.
    e.stopPropagation();
    this.setAttribute('value', this._select.value);
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._select.value },
    }));
  }
}

customElements.define('tf-select', TfSelect);
export { TfSelect };
