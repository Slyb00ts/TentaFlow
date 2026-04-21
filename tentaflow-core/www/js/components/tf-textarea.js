// =============================================================================
// File: tf-textarea.js — <tf-textarea> multi-line input, light DOM, auto-grow.
// Supports: label, placeholder, value, hint, error, rows, disabled, autogrow.
// Reflects .value to attribute and emits "input"/"change".
// Example: <tf-textarea placeholder="Zapytaj o cokolwiek..." autogrow rows="2">
// =============================================================================

class TfTextarea extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'placeholder', 'value', 'hint', 'error', 'rows', 'disabled', 'autogrow', 'maxlength'];
  }

  constructor() {
    super();
    this._group = null;
    this._textarea = null;
    this._labelEl = null;
    this._hintEl = null;
    this._errorEl = null;
    this._onInput = this._onInput.bind(this);
    this._onChange = this._onChange.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
  }

  connectedCallback() {
    if (!this._group) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._group) return;
    if (name === 'value' && this._textarea && this._textarea.value !== (newVal || '')) {
      this._textarea.value = newVal || '';
      this._autogrow();
    }
    this._update();
  }

  get value() { return this._textarea ? this._textarea.value : (this.getAttribute('value') || ''); }
  set value(v) {
    if (this._textarea) {
      this._textarea.value = v ?? '';
      this._autogrow();
    }
    this.setAttribute('value', v ?? '');
  }

  focus() { this._textarea?.focus(); }

  _build() {
    this.innerHTML = '';
    const group = document.createElement('div');
    group.className = 'tf-input-group tf-textarea-group';

    const label = document.createElement('span');
    label.className = 'tf-label';
    group.appendChild(label);

    const ta = document.createElement('textarea');
    ta.className = 'tf-input tf-textarea';
    ta.addEventListener('input', this._onInput);
    ta.addEventListener('change', this._onChange);
    ta.addEventListener('keydown', this._onKeyDown);
    group.appendChild(ta);

    const hint = document.createElement('span');
    hint.className = 'tf-hint';
    group.appendChild(hint);

    const err = document.createElement('span');
    err.className = 'tf-error-text';
    group.appendChild(err);

    this.appendChild(group);

    this._group = group;
    this._labelEl = label;
    this._textarea = ta;
    this._hintEl = hint;
    this._errorEl = err;
  }

  _update() {
    const labelText = this.getAttribute('label') || '';
    const placeholder = this.getAttribute('placeholder') || '';
    const value = this.getAttribute('value') || '';
    const hint = this.getAttribute('hint') || '';
    const error = this.getAttribute('error') || '';
    const rows = this.getAttribute('rows') || '2';
    const disabled = this.hasAttribute('disabled');
    const maxlen = this.getAttribute('maxlength');

    this._labelEl.textContent = labelText;
    this._labelEl.style.display = labelText ? '' : 'none';

    this._textarea.placeholder = placeholder;
    if (document.activeElement !== this._textarea) this._textarea.value = value;
    this._textarea.rows = parseInt(rows, 10) || 2;
    this._textarea.disabled = disabled;
    if (maxlen !== null) this._textarea.setAttribute('maxlength', maxlen);
    else this._textarea.removeAttribute('maxlength');

    this._textarea.className = error ? 'tf-input tf-textarea tf-input-error' : 'tf-input tf-textarea';

    this._hintEl.textContent = hint;
    this._hintEl.style.display = hint && !error ? '' : 'none';
    this._errorEl.textContent = error;
    this._errorEl.style.display = error ? '' : 'none';

    this._autogrow();
  }

  _autogrow() {
    if (!this.hasAttribute('autogrow') || !this._textarea) return;
    // Auto-size to content up to CSS max-height; min via rows attr.
    this._textarea.style.height = 'auto';
    this._textarea.style.height = `${this._textarea.scrollHeight}px`;
  }

  _onInput() {
    this.setAttribute('value', this._textarea.value);
    this._autogrow();
    this.dispatchEvent(new CustomEvent('input', {
      bubbles: true,
      detail: { value: this._textarea.value },
    }));
  }

  _onChange() {
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._textarea.value },
    }));
  }

  _onKeyDown(e) {
    // Re-emit keydown so feature modules can implement Ctrl+Enter = send.
    this.dispatchEvent(new CustomEvent('tf-keydown', {
      bubbles: true,
      detail: { key: e.key, ctrlKey: e.ctrlKey, metaKey: e.metaKey, shiftKey: e.shiftKey, original: e },
    }));
  }
}

customElements.define('tf-textarea', TfTextarea);
export { TfTextarea };
