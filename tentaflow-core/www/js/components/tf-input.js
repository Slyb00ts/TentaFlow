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
    return ['label', 'placeholder', 'value', 'hint', 'error', 'type', 'icon', 'trailing-icon', 'prefix', 'suffix', 'disabled', 'autocomplete', 'autofocus', 'required', 'name', 'autocapitalize', 'autocorrect', 'spellcheck', 'inputmode', 'minlength', 'maxlength', 'pattern', 'multiline', 'rows', 'min', 'max', 'step'];
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
    this._trailingIconEl = null;
    this._prefixEl = null;
    this._suffixEl = null;
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

    const input = document.createElement(this.hasAttribute('multiline') ? 'textarea' : 'input');
    input.className = 'tf-input';
    input.addEventListener('input', this._onInput);
    input.addEventListener('change', this._onChange);
    input.addEventListener('focus', this._onFocus);
    input.addEventListener('blur', this._onBlur);

    const iconEl = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    iconEl.classList.add('tf-input-icon');
    iconEl.setAttribute('width', '16');
    iconEl.setAttribute('height', '16');
    iconEl.setAttribute('fill', 'none');
    iconEl.setAttribute('stroke', 'currentColor');
    iconEl.setAttribute('stroke-width', '2');
    iconEl.setAttribute('stroke-linecap', 'round');
    iconEl.setAttribute('stroke-linejoin', 'round');
    iconEl.setAttribute('aria-hidden', 'true');
    const useEl = document.createElementNS('http://www.w3.org/2000/svg', 'use');
    iconEl.appendChild(useEl);

    // Trailing icon (po prawej stronie inputa). Osobny <svg use> jak leading.
    const trailingIconEl = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    trailingIconEl.classList.add('tf-input-icon', 'tf-input-icon-trailing');
    trailingIconEl.setAttribute('width', '16');
    trailingIconEl.setAttribute('height', '16');
    trailingIconEl.setAttribute('fill', 'none');
    trailingIconEl.setAttribute('stroke', 'currentColor');
    trailingIconEl.setAttribute('stroke-width', '2');
    trailingIconEl.setAttribute('stroke-linecap', 'round');
    trailingIconEl.setAttribute('stroke-linejoin', 'round');
    trailingIconEl.setAttribute('aria-hidden', 'true');
    trailingIconEl.style.display = 'none';
    trailingIconEl.appendChild(
      document.createElementNS('http://www.w3.org/2000/svg', 'use')
    );

    // Adornments tekstowe — prefix przed inputem, suffix po nim.
    const prefixEl = document.createElement('span');
    prefixEl.className = 'tf-input-prefix';
    prefixEl.setAttribute('aria-hidden', 'true');
    prefixEl.style.display = 'none';

    const suffixEl = document.createElement('span');
    suffixEl.className = 'tf-input-suffix';
    suffixEl.setAttribute('aria-hidden', 'true');
    suffixEl.style.display = 'none';

    wrap.appendChild(prefixEl);
    wrap.appendChild(input);
    wrap.appendChild(suffixEl);
    wrap.appendChild(iconEl);
    wrap.appendChild(trailingIconEl);
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
    this._trailingIconEl = trailingIconEl;
    this._prefixEl = prefixEl;
    this._suffixEl = suffixEl;
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
    if (this._input.tagName !== 'TEXTAREA') this._input.type = type;
    this._input.disabled = disabled;
    if (this._input.tagName === 'TEXTAREA') {
      this._input.rows = Number(this.getAttribute('rows') || 4);
    }

    // pass-through natywnych atrybutow do wewnetrznego <input>
    const autocomplete = this.getAttribute('autocomplete');
    if (autocomplete) this._input.setAttribute('autocomplete', autocomplete);
    else this._input.removeAttribute('autocomplete');

    const name = this.getAttribute('name');
    if (name) this._input.setAttribute('name', name);
    else this._input.removeAttribute('name');

    // Pass-through dla atrybutow kontroli wprowadzania (mobilna klawiatura).
    for (const attr of ['autocapitalize', 'autocorrect', 'spellcheck', 'inputmode', 'minlength', 'maxlength', 'pattern', 'min', 'max', 'step']) {
      const v = this.getAttribute(attr);
      if (v !== null) this._input.setAttribute(attr, v);
      else this._input.removeAttribute(attr);
    }

    if (this.hasAttribute('required')) this._input.setAttribute('required', '');
    else this._input.removeAttribute('required');

    if (this.hasAttribute('autofocus') && document.activeElement !== this._input) {
      // autofocus dziala tylko przy pierwszym mount — kolejne re-mounty wymagaja recznego focus()
      queueMicrotask(() => this._input?.focus());
    }

    const baseClass = error ? 'tf-input tf-input-error' : 'tf-input';
    this._input.className = baseClass;
    // The wrap carries the field border/ring, so mirror error + disabled there.
    this._wrap.classList.toggle('tf-input-wrap-error', !!error);
    this._wrap.classList.toggle('tf-input-wrap-disabled', disabled);

    if (icon) {
      this._iconEl.style.display = '';
      this._iconEl.querySelector('use').setAttribute('href', `#i-${icon}`);
      this._wrap.classList.add('tf-input-wrap-has-icon');
    } else {
      this._iconEl.style.display = 'none';
      this._wrap.classList.remove('tf-input-wrap-has-icon');
    }

    const trailingIcon = this.getAttribute('trailing-icon');
    if (trailingIcon) {
      this._trailingIconEl.style.display = '';
      this._trailingIconEl.querySelector('use').setAttribute('href', `#i-${trailingIcon}`);
      this._wrap.classList.add('tf-input-wrap-has-trailing-icon');
    } else {
      this._trailingIconEl.style.display = 'none';
      this._wrap.classList.remove('tf-input-wrap-has-trailing-icon');
    }

    const prefix = this.getAttribute('prefix') || '';
    this._prefixEl.textContent = prefix;
    this._prefixEl.style.display = prefix ? '' : 'none';
    this._wrap.classList.toggle('tf-input-wrap-has-prefix', !!prefix);

    const suffix = this.getAttribute('suffix') || '';
    this._suffixEl.textContent = suffix;
    this._suffixEl.style.display = suffix ? '' : 'none';
    this._wrap.classList.toggle('tf-input-wrap-has-suffix', !!suffix);

    this._hintEl.textContent = hint;
    this._hintEl.style.display = hint && !error ? '' : 'none';

    this._errorEl.textContent = error;
    this._errorEl.style.display = error ? '' : 'none';
  }

  _onInput(e) {
    // Stop the inner <input>'s native "input" from bubbling past the host —
    // otherwise consumers listening on the host see two "input" events (the
    // native one has no detail, so handlers doing `e.detail.value` throw or
    // `e.detail?.value ?? ''` clobbers state).
    e?.stopPropagation();
    this.setAttribute('value', this._input.value);
    this.dispatchEvent(new CustomEvent('input', {
      bubbles: true,
      detail: { value: this._input.value },
    }));
  }

  _onChange(e) {
    e?.stopPropagation();
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
