// =============================================================================
// Plik: tf-input.js
// Opis: Komponent <tf-input> — label + input + hint/error w light DOM. Wspiera
//       wariant z ikona (icon="search"), typ (text/email/password), disabled,
//       oraz slot="label" dla zlozonego labela (np. tekst + <tf-chip>).
//       Reflektuje .value do atrybutu i emituje "input"/"change".
// Przyklad: <tf-input label="Email" icon="search" hint="pomocniczy tekst"></tf-input>
//   z slotem: <tf-input><span slot="label">Klucz <tf-chip status="warn">secret</tf-chip></span></tf-input>
// =============================================================================

class TfInput extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'placeholder', 'value', 'hint', 'error', 'type', 'icon', 'disabled', 'autocomplete', 'autofocus', 'required', 'name'];
  }

  constructor() {
    super();
    this._group = null;
    this._input = null;
    this._labelEl = null;
    this._hintEl = null;
    this._errorEl = null;
    this._wrap = null;
    this._iconEl = null;
    this._slotObserver = null;
    this._hasSlotLabel = false;
    this._onInput = this._onInput.bind(this);
    this._onChange = this._onChange.bind(this);
    this._onFocus = this._onFocus.bind(this);
    this._onBlur = this._onBlur.bind(this);
    this._onChildrenMutated = this._onChildrenMutated.bind(this);
  }

  connectedCallback() {
    if (!this._group) this._build();
    this._update();
    // Obserwator reaguje na dodanie/usuniecie dziecka slot="label" po mount.
    if (!this._slotObserver) {
      this._slotObserver = new MutationObserver(this._onChildrenMutated);
      this._slotObserver.observe(this, { childList: true });
    }
  }

  disconnectedCallback() {
    if (this._slotObserver) {
      this._slotObserver.disconnect();
      this._slotObserver = null;
    }
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal) return;
    if (!this._group) return;
    if (name === 'value' && this._input && this._input.value !== newVal) {
      this._input.value = newVal ?? '';
    }
    this._update();
  }

  get value() { return this._input ? this._input.value : (this.getAttribute('value') || ''); }
  set value(v) {
    if (this._input) this._input.value = v ?? '';
    this.setAttribute('value', v ?? '');
  }

  focus() { this._input?.focus(); }

  _build() {
    // Zachowujemy element-dziecko z slot="label" przed wyczyszczeniem DOM.
    const slotLabelEl = this.querySelector(':scope > [slot="label"]');
    this.innerHTML = '';
    const group = document.createElement('div');
    group.className = 'tf-input-group';

    const label = document.createElement('span');
    label.className = 'tf-label';
    if (slotLabelEl) {
      slotLabelEl.removeAttribute('slot');
      label.appendChild(slotLabelEl);
      this._hasSlotLabel = true;
    }
    group.appendChild(label);

    // wrap jest uzywany zawsze — ale pokazujemy ikone tylko jesli jest atrybut icon
    const wrap = document.createElement('div');
    wrap.className = 'tf-input-wrap';

    const input = document.createElement('input');
    input.className = 'tf-input';
    input.addEventListener('input', this._onInput);
    input.addEventListener('change', this._onChange);
    input.addEventListener('focus', this._onFocus);
    input.addEventListener('blur', this._onBlur);

    const iconEl = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    iconEl.classList.add('tf-input-icon');
    const useEl = document.createElementNS('http://www.w3.org/2000/svg', 'use');
    iconEl.appendChild(useEl);

    wrap.appendChild(input);
    wrap.appendChild(iconEl);
    group.appendChild(wrap);

    const hint = document.createElement('span');
    hint.className = 'tf-hint';
    group.appendChild(hint);

    const err = document.createElement('span');
    err.className = 'tf-error-text';
    group.appendChild(err);

    this.appendChild(group);

    this._group = group;
    this._labelEl = label;
    this._input = input;
    this._wrap = wrap;
    this._iconEl = iconEl;
    this._hintEl = hint;
    this._errorEl = err;
  }

  _update() {
    const labelText = this.getAttribute('label') || '';
    const placeholder = this.getAttribute('placeholder') || '';
    const value = this.getAttribute('value') || '';
    const hint = this.getAttribute('hint') || '';
    const error = this.getAttribute('error') || '';
    const type = this.getAttribute('type') || 'text';
    const icon = this.getAttribute('icon');
    const disabled = this.hasAttribute('disabled');

    // Slot "label" wygrywa nad atrybutem label gdy oba sa obecne.
    if (this._hasSlotLabel) {
      this._labelEl.style.display = '';
    } else {
      this._labelEl.textContent = labelText;
      this._labelEl.style.display = labelText ? '' : 'none';
    }

    this._input.placeholder = placeholder;
    if (document.activeElement !== this._input) this._input.value = value;
    this._input.type = type;
    this._input.disabled = disabled;

    // pass-through natywnych atrybutow do wewnetrznego <input>
    const autocomplete = this.getAttribute('autocomplete');
    if (autocomplete) this._input.setAttribute('autocomplete', autocomplete);
    else this._input.removeAttribute('autocomplete');

    const name = this.getAttribute('name');
    if (name) this._input.setAttribute('name', name);
    else this._input.removeAttribute('name');

    if (this.hasAttribute('required')) this._input.setAttribute('required', '');
    else this._input.removeAttribute('required');

    if (this.hasAttribute('autofocus') && document.activeElement !== this._input) {
      // autofocus dziala tylko przy pierwszym mount — kolejne re-mounty wymagaja recznego focus()
      queueMicrotask(() => this._input?.focus());
    }

    const baseClass = error ? 'tf-input tf-input-error' : 'tf-input';
    this._input.className = baseClass;

    if (icon) {
      this._iconEl.style.display = '';
      this._iconEl.querySelector('use').setAttribute('href', `#i-${icon}`);
      this._wrap.classList.add('tf-input-wrap-has-icon');
    } else {
      this._iconEl.style.display = 'none';
      this._wrap.classList.remove('tf-input-wrap-has-icon');
      // gdy brak ikony — usuwamy padding-left narzucony przez .tf-input-wrap
      this._input.style.paddingLeft = '14px';
    }
    if (icon) this._input.style.paddingLeft = '';

    this._hintEl.textContent = hint;
    this._hintEl.style.display = hint && !error ? '' : 'none';

    this._errorEl.textContent = error;
    this._errorEl.style.display = error ? '' : 'none';
  }

  _onInput() {
    this.setAttribute('value', this._input.value);
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

  _onFocus() {
    this._group.classList.add('tf-focused');
    // label flash — przywrocenie stanu nastapi po 220ms
    clearTimeout(this._focusTimer);
    this._focusTimer = setTimeout(() => {
      this._group.classList.remove('tf-focused');
    }, 220);
  }

  _onBlur() {
    this._group.classList.remove('tf-focused');
  }

  _onChildrenMutated() {
    // Jesli pojawil sie nowy slot="label" po mount — przenosimy go do labela.
    const slotLabelEl = this.querySelector(':scope > [slot="label"]');
    if (slotLabelEl) {
      slotLabelEl.removeAttribute('slot');
      this._labelEl.replaceChildren(slotLabelEl);
      this._hasSlotLabel = true;
      this._labelEl.style.display = '';
    }
  }
}

customElements.define('tf-input', TfInput);
export { TfInput };
