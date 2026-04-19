// =============================================================================
// Plik: tf-searchbox.js
// Opis: Komponent <tf-searchbox> — input typu search z lupką i clearem.
//       Debounce configurowalne (domyslnie 200ms). Emituje event "search"
//       z detail.value. ESC i klik x czysci pole.
// Przyklad: <tf-searchbox placeholder="Szukaj..." debounce="300"></tf-searchbox>
// =============================================================================

class TfSearchbox extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'value', 'debounce'];
  }

  constructor() {
    super();
    this._root = null;
    this._input = null;
    this._clear = null;
    this._debounceId = 0;
    this._onInput = this._onInput.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onClear = this._onClear.bind(this);
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  disconnectedCallback() {
    clearTimeout(this._debounceId);
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._root) return;
    if (name === 'value' && this._input && this._input.value !== (newVal || '')) {
      this._input.value = newVal || '';
      this._syncHasValue();
    }
    this._update();
  }

  get value() { return this._input ? this._input.value : (this.getAttribute('value') || ''); }
  set value(v) {
    if (this._input) {
      this._input.value = v ?? '';
      this._syncHasValue();
    }
    this.setAttribute('value', v ?? '');
  }

  focus() { this._input?.focus(); }

  _build() {
    this.innerHTML = '';
    const label = document.createElement('label');
    label.className = 'tf-searchbox';

    const iconSvg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    iconSvg.classList.add('tf-searchbox-icon');
    const useEl = document.createElementNS('http://www.w3.org/2000/svg', 'use');
    useEl.setAttribute('href', '#i-search');
    iconSvg.appendChild(useEl);

    const input = document.createElement('input');
    input.type = 'search';
    input.autocomplete = 'off';
    input.spellcheck = false;
    input.addEventListener('input', this._onInput);
    input.addEventListener('keydown', this._onKeyDown);

    const clearBtn = document.createElement('button');
    clearBtn.type = 'button';
    clearBtn.className = 'tf-searchbox-clear';
    clearBtn.setAttribute('aria-label', 'Wyczysc');
    clearBtn.textContent = '×';
    clearBtn.addEventListener('click', this._onClear);

    label.appendChild(iconSvg);
    label.appendChild(input);
    label.appendChild(clearBtn);
    this.appendChild(label);

    this._root = label;
    this._input = input;
    this._clear = clearBtn;
  }

  _update() {
    const placeholder = this.getAttribute('placeholder') || '';
    const value = this.getAttribute('value') || '';
    this._input.placeholder = placeholder;
    if (document.activeElement !== this._input) {
      this._input.value = value;
    }
    this._syncHasValue();
  }

  _syncHasValue() {
    this._root.classList.toggle('has-value', (this._input.value || '').length > 0);
  }

  _onInput() {
    this._syncHasValue();
    const debounceMs = parseInt(this.getAttribute('debounce') || '200', 10);
    clearTimeout(this._debounceId);
    const value = this._input.value;
    this._debounceId = setTimeout(() => {
      this.setAttribute('value', value);
      this.dispatchEvent(new CustomEvent('search', {
        bubbles: true,
        detail: { value },
      }));
    }, Math.max(0, debounceMs));
  }

  _onKeyDown(e) {
    if (e.key === 'Escape') {
      this._onClear();
      e.stopPropagation();
    }
  }

  _onClear() {
    this._input.value = '';
    this._input.focus();
    this._syncHasValue();
    this.setAttribute('value', '');
    clearTimeout(this._debounceId);
    this.dispatchEvent(new CustomEvent('search', {
      bubbles: true,
      detail: { value: '' },
    }));
  }
}

customElements.define('tf-searchbox', TfSearchbox);
export { TfSearchbox };
