// =============================================================================
// Plik: tf-select.js
// Opis: Komponent <tf-select> — wraper nad natywnym <select>. Dzieci <option>
//       sa przejmowane i umieszczane w select. Emituje "change" z detail.value.
// Przyklad: <tf-select value="rr"><option value="fa">First</option>...</tf-select>
// =============================================================================

class TfSelect extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'disabled', 'name'];
  }

  constructor() {
    super();
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

  _build() {
    // przejmij <option> z light DOM i przenies do wewnetrznego <select>
    const options = Array.from(this.querySelectorAll('option'));
    this.innerHTML = '';

    const wrap = document.createElement('div');
    wrap.className = 'tf-select-wrap';

    const select = document.createElement('select');
    select.className = 'tf-select';
    options.forEach((opt) => select.appendChild(opt));
    select.addEventListener('change', this._onChange);

    wrap.appendChild(select);
    this.appendChild(wrap);

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
  }

  _onChange() {
    this.setAttribute('value', this._select.value);
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._select.value },
    }));
  }
}

customElements.define('tf-select', TfSelect);
export { TfSelect };
