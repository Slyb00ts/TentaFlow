// =============================================================================
// File: tf-mention-input.js
// Description: <tf-mention-input> — textarea with an @-style mention trigger.
//   Typing one of the configured trigger characters opens a suggestion popover
//   fed by the host; selecting a suggestion inserts the trigger + label at the
//   caret. The component owns caret/popover mechanics and emits search/mention
//   events; it never resolves suggestions itself.
//   Attributes: placeholder, disabled.
//   Property: .triggers (array of single-char trigger strings, default ['@']);
//   .suggestions (array of {id, label} the host pushes after a 'search').
//   Events: search (detail: {trigger, query}), mention (detail: {id, label,
//   trigger}), change (detail: {value}). All bubble.
// =============================================================================

class TfMentionInput extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'disabled', 'value'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._textarea = null;
    this._popover = null;
    this._triggers = ['@'];
    this._suggestions = [];
    this._activeIdx = -1;
    this._isOpen = false;
    // The trigger char position + query span currently being edited.
    this._triggerStart = -1;
    this._activeTrigger = null;
    this._uid = `tfmi-${Math.random().toString(36).slice(2, 8)}`;
    this._onInput = this._onInput.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onBlur = this._onBlur.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    if (name === 'value') {
      if (document.activeElement !== this._textarea) this._textarea.value = newVal || '';
      return;
    }
    this._update();
  }

  get value() {
    return this._textarea ? this._textarea.value : (this.getAttribute('value') || '');
  }

  set value(v) {
    const next = v ?? '';
    this.setAttribute('value', next);
    if (this._textarea && document.activeElement !== this._textarea) {
      this._textarea.value = next;
    }
  }

  get triggers() { return this._triggers.slice(); }

  set triggers(arr) {
    const list = Array.isArray(arr)
      ? arr.map((s) => String(s)).filter((s) => s.length === 1)
      : [];
    this._triggers = list.length > 0 ? list : ['@'];
  }

  get suggestions() { return this._suggestions.slice(); }

  set suggestions(arr) {
    this._suggestions = Array.isArray(arr)
      ? arr.map((s) => ({ id: String(s.id), label: String(s.label) }))
      : [];
    if (this._isOpen) this._renderPopover();
  }

  get disabled() { return this.hasAttribute('disabled'); }

  set disabled(v) {
    if (v) this.setAttribute('disabled', '');
    else this.removeAttribute('disabled');
    if (this._textarea) this._textarea.disabled = !!v;
  }

  focus() { this._textarea?.focus(); }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-mention-input';

    const textarea = document.createElement('textarea');
    textarea.className = 'tf-mention-input-area';
    textarea.setAttribute('aria-autocomplete', 'list');
    textarea.setAttribute('aria-expanded', 'false');
    textarea.addEventListener('input', this._onInput);
    textarea.addEventListener('keydown', this._onKeyDown);
    textarea.addEventListener('blur', this._onBlur);
    wrap.appendChild(textarea);

    const popover = document.createElement('div');
    popover.className = 'tf-mention-input-popover';
    popover.setAttribute('role', 'listbox');
    popover.hidden = true;
    wrap.appendChild(popover);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._textarea = textarea;
    this._popover = popover;
  }

  _update() {
    const placeholder = this.getAttribute('placeholder') || '';
    const disabled = this.hasAttribute('disabled');
    const value = this.getAttribute('value') || '';
    this._textarea.placeholder = placeholder;
    this._textarea.disabled = disabled;
    if (document.activeElement !== this._textarea) this._textarea.value = value;
    const ariaLabel = this.getAttribute('aria-label') || '';
    if (ariaLabel) this._textarea.setAttribute('aria-label', ariaLabel);
    else this._textarea.removeAttribute('aria-label');
  }

  /// Scans backwards from the caret for an active trigger char that is not
  /// broken by whitespace. Returns {trigger, start, query} or null.
  _detectTrigger() {
    const caret = this._textarea.selectionStart ?? this._textarea.value.length;
    const text = this._textarea.value;
    for (let i = caret - 1; i >= 0; i--) {
      const ch = text[i];
      if (ch === ' ' || ch === '\n' || ch === '\t') return null;
      if (this._triggers.includes(ch)) {
        // A trigger only counts at start-of-line or after whitespace.
        if (i > 0) {
          const prev = text[i - 1];
          if (prev !== ' ' && prev !== '\n' && prev !== '\t') return null;
        }
        return { trigger: ch, start: i, query: text.slice(i + 1, caret) };
      }
    }
    return null;
  }

  _onInput() {
    this.setAttribute('value', this._textarea.value);
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._textarea.value },
    }));
    if (this.hasAttribute('disabled')) return;
    const det = this._detectTrigger();
    if (!det) {
      this._close();
      return;
    }
    this._triggerStart = det.start;
    this._activeTrigger = det.trigger;
    this._open();
    this.dispatchEvent(new CustomEvent('search', {
      bubbles: true,
      detail: { trigger: det.trigger, query: det.query },
    }));
  }

  _open() {
    if (!this._isOpen) {
      this._isOpen = true;
      this._popover.hidden = false;
      this._textarea.setAttribute('aria-expanded', 'true');
    }
    this._renderPopover();
  }

  _close() {
    if (!this._isOpen) return;
    this._isOpen = false;
    this._popover.hidden = true;
    this._textarea.setAttribute('aria-expanded', 'false');
    this._activeIdx = -1;
    this._triggerStart = -1;
    this._activeTrigger = null;
  }

  _renderPopover() {
    this._popover.innerHTML = '';
    if (this._suggestions.length === 0) {
      this._activeIdx = -1;
      return;
    }
    if (this._activeIdx < 0 || this._activeIdx >= this._suggestions.length) {
      this._activeIdx = 0;
    }
    for (let i = 0; i < this._suggestions.length; i++) {
      const s = this._suggestions[i];
      const el = document.createElement('div');
      el.className = 'tf-mention-input-option';
      el.setAttribute('role', 'option');
      el.id = `${this._uid}-opt-${i}`;
      el.textContent = s.label;
      if (i === this._activeIdx) el.classList.add('active');
      el.addEventListener('mousedown', (e) => {
        e.preventDefault();
        this._select(i);
      });
      this._popover.appendChild(el);
    }
    const active = this._popover.children[this._activeIdx];
    if (active) this._textarea.setAttribute('aria-activedescendant', active.id);
  }

  _moveActive(dir) {
    if (this._suggestions.length === 0) return;
    const n = this._suggestions.length;
    this._activeIdx = (this._activeIdx + dir + n) % n;
    this._renderPopover();
  }

  _select(idx) {
    if (this.hasAttribute('disabled')) return;
    const s = this._suggestions[idx];
    if (!s || this._triggerStart < 0) return;
    const text = this._textarea.value;
    const caret = this._textarea.selectionStart ?? text.length;
    const before = text.slice(0, this._triggerStart);
    const after = text.slice(caret);
    const inserted = `${this._activeTrigger}${s.label} `;
    const trigger = this._activeTrigger;
    this._textarea.value = before + inserted + after;
    const newCaret = before.length + inserted.length;
    this._textarea.selectionStart = newCaret;
    this._textarea.selectionEnd = newCaret;
    this.setAttribute('value', this._textarea.value);
    this._close();
    this.dispatchEvent(new CustomEvent('mention', {
      bubbles: true,
      detail: { id: s.id, label: s.label, trigger },
    }));
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._textarea.value },
    }));
  }

  _onKeyDown(e) {
    if (!this._isOpen) return;
    switch (e.key) {
      case 'ArrowDown': e.preventDefault(); this._moveActive(1); return;
      case 'ArrowUp': e.preventDefault(); this._moveActive(-1); return;
      case 'Enter':
        if (this._activeIdx >= 0 && this._suggestions.length > 0) {
          e.preventDefault();
          this._select(this._activeIdx);
        }
        return;
      case 'Escape': e.preventDefault(); this._close(); return;
    }
  }

  _onBlur() { this._close(); }
}

customElements.define('tf-mention-input', TfMentionInput);
export { TfMentionInput };
