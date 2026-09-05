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
    this._observer = null;
    this._onChange = this._onChange.bind(this);
    this._onLightMutation = this._onLightMutation.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
    // Callers that fill a select AFTER the upgrade (async data, a partial
    // re-render) assign light-DOM <option>s. Without adoption those options sit
    // outside the built <select> and the browser paints them as bare text.
    if (!this._observer && typeof MutationObserver !== 'undefined') {
      this._observer = new MutationObserver(this._onLightMutation);
      this._observer.observe(this, { childList: true });
    }
  }

  disconnectedCallback() {
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
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

  // The host carries no tabindex, so focus has to reach the real <select> —
  // same forwarding as tf-input, which callers already rely on.
  focus() { this._select?.focus(); }

  // Replaces the inner <select> options at runtime (the light-DOM <option>
  // children are consumed at build time, so callers that fetch options async
  // must use this instead of re-setting innerHTML). `list` is [{value,label}];
  // `selected` keeps the current pick when present in the new list.
  setOptions(list, selected) {
    if (!this._select) this._build();
    // A light-DOM <option> that has not been adopted yet — markup written by
    // innerHTML whose mutation record has not been delivered — would be moved
    // into the select AFTER this call and append itself to the list it was
    // meant to replace. Replacing the options replaces BOTH places.
    for (const node of Array.from(this.children)) {
      if (node.tagName === 'OPTION' || node.tagName === 'OPTGROUP') node.remove();
    }
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

  // `innerHTML = '<option>…'` on an upgraded host destroys the built structure,
  // while `appendChild(option)` leaves it intact — the two need different
  // repairs, and neither may re-enter (the repair itself mutates children, but
  // leaves no top-level <option> behind, so the next callback returns early).
  _onLightMutation() {
    const loose = Array.from(this.children).filter(
      (n) => n.tagName === 'OPTION' || n.tagName === 'OPTGROUP'
    );
    if (!loose.length) return;
    if (this._select && this.contains(this._select)) loose.forEach((n) => this._select.appendChild(n));
    else this._build();
    this._update();
  }

  _build() {
    // Przejmij top-level <option> ORAZ <optgroup> z light DOM zachowujac ich
    // kolejnosc i strukture grupowania. Wczesniej `querySelectorAll('option')`
    // splaszczalo grupy, gubiac etykiety <optgroup> w finalnym UI.
    const topLevel = Array.from(this.children).filter(
      (n) => n.tagName === 'OPTION' || n.tagName === 'OPTGROUP'
    );
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
    topLevel.forEach((node) => select.appendChild(node));
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
