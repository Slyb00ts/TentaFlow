// =============================================================================
// Plik: tf-pin-input.js
// Opis: Komponent <tf-pin-input> — OTP-style input dla 6-cyfrowego PIN (mesh
//       pairing confirm). Auto-advance miedzy polami, paste rozrzuca cyfry,
//       backspace cofa focus, autocomplete="one-time-code" dla mobile OTP.
//       Styling: JetBrains Mono 28px, split 3+3 z separatorem, akcent indigo.
// Przyklad: <tf-pin-input length="6" autofocus></tf-pin-input>
//           el.addEventListener('complete', e => submit(e.detail.value));
// =============================================================================

class TfPinInput extends HTMLElement {
  static get observedAttributes() {
    return ['length', 'group-size', 'disabled', 'error', 'success', 'autofocus'];
  }

  constructor() {
    super();
    this._cells = [];
    this._root = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._sync();
    if (this.hasAttribute('autofocus')) {
      queueMicrotask(() => this._cells[0]?.focus());
    }
  }

  attributeChangedCallback() {
    if (this._root) this._sync();
  }

  get value() {
    return this._cells.map(c => c.value).join('');
  }
  set value(v) {
    const digits = String(v || '').replace(/\D/g, '').slice(0, this._length());
    this._cells.forEach((c, i) => {
      c.value = digits[i] || '';
      c.classList.toggle('filled', !!digits[i]);
    });
  }

  get complete() {
    return this.value.length === this._length();
  }

  focus() {
    const firstEmpty = this._cells.find(c => !c.value) || this._cells[0];
    firstEmpty?.focus();
  }

  clear() {
    this._cells.forEach(c => {
      c.value = '';
      c.classList.remove('filled');
    });
    this._cells[0]?.focus();
  }

  _length() {
    const n = parseInt(this.getAttribute('length') || '6', 10);
    return Number.isFinite(n) && n > 0 && n <= 12 ? n : 6;
  }

  _groupSize() {
    const g = parseInt(this.getAttribute('group-size') || '3', 10);
    return Number.isFinite(g) && g > 0 ? g : 0;
  }

  _build() {
    const length = this._length();
    const groupSize = this._groupSize();

    this.classList.add('tf-pin-input');

    this._cells = [];
    const groups = [];
    let currentGroup = document.createElement('div');
    currentGroup.className = 'tf-pin-group';

    for (let i = 0; i < length; i++) {
      if (groupSize > 0 && i > 0 && i % groupSize === 0) {
        groups.push(currentGroup);
        const sep = document.createElement('span');
        sep.className = 'tf-pin-sep';
        sep.textContent = '·';
        sep.setAttribute('aria-hidden', 'true');
        groups.push(sep);
        currentGroup = document.createElement('div');
        currentGroup.className = 'tf-pin-group';
      }
      const cell = document.createElement('input');
      cell.className = 'tf-pin-cell';
      cell.type = 'text';
      cell.maxLength = 1;
      cell.inputMode = 'numeric';
      cell.autocomplete = i === 0 ? 'one-time-code' : 'off';
      cell.setAttribute('aria-label', `PIN cyfra ${i + 1} z ${length}`);
      cell.addEventListener('input', (e) => this._onInput(e, i));
      cell.addEventListener('keydown', (e) => this._onKeyDown(e, i));
      cell.addEventListener('paste', (e) => this._onPaste(e));
      cell.addEventListener('focus', () => cell.select());
      this._cells.push(cell);
      currentGroup.appendChild(cell);
    }
    groups.push(currentGroup);

    this.innerHTML = '';
    this.setAttribute('role', 'group');
    groups.forEach(g => this.appendChild(g));
    this._root = true;
  }

  _sync() {
    const disabled = this.hasAttribute('disabled');
    const err = this.hasAttribute('error');
    const ok = this.hasAttribute('success');
    this._cells.forEach(c => {
      c.disabled = disabled;
      c.classList.toggle('error', err);
      c.classList.toggle('success', ok);
    });
  }

  _onInput(e, idx) {
    const cell = this._cells[idx];
    const raw = cell.value;
    if (raw && !/^\d$/.test(raw)) {
      cell.value = '';
      return;
    }
    cell.classList.toggle('filled', !!cell.value);
    if (cell.value && idx < this._cells.length - 1) {
      this._cells[idx + 1].focus();
    }
    this._emitInput();
    if (this.complete) this._emitComplete();
  }

  _onKeyDown(e, idx) {
    const cell = this._cells[idx];
    if (e.key === 'Backspace') {
      if (!cell.value && idx > 0) {
        e.preventDefault();
        const prev = this._cells[idx - 1];
        prev.value = '';
        prev.classList.remove('filled');
        prev.focus();
        this._emitInput();
      }
    } else if (e.key === 'ArrowLeft' && idx > 0) {
      e.preventDefault();
      this._cells[idx - 1].focus();
    } else if (e.key === 'ArrowRight' && idx < this._cells.length - 1) {
      e.preventDefault();
      this._cells[idx + 1].focus();
    } else if (e.key === 'Enter' && this.complete) {
      this.dispatchEvent(new CustomEvent('submit', {
        detail: { value: this.value },
        bubbles: true,
      }));
    }
  }

  _onPaste(e) {
    e.preventDefault();
    const text = (e.clipboardData || window.clipboardData)?.getData('text') || '';
    const digits = text.replace(/\D/g, '').slice(0, this._cells.length);
    if (!digits) return;
    this._cells.forEach((c, i) => {
      c.value = digits[i] || '';
      c.classList.toggle('filled', !!digits[i]);
    });
    const focusIdx = Math.min(digits.length, this._cells.length - 1);
    this._cells[focusIdx]?.focus();
    this._emitInput();
    if (this.complete) this._emitComplete();
  }

  _emitInput() {
    this.dispatchEvent(new CustomEvent('input', {
      detail: { value: this.value, complete: this.complete },
      bubbles: true,
    }));
  }

  _emitComplete() {
    this.dispatchEvent(new CustomEvent('complete', {
      detail: { value: this.value },
      bubbles: true,
    }));
    this.dispatchEvent(new Event('change', { bubbles: true }));
  }
}

if (!customElements.get('tf-pin-input')) {
  customElements.define('tf-pin-input', TfPinInput);
}

export { TfPinInput };
