// =============================================================================
// File: tf-tag-input.js
// Description: <tf-tag-input> — multi-value input rendering committed values as
//   removable chips followed by a free-text entry. Adding a tag is triggered by
//   Enter, by a configured separator key, or by blur; removing happens via a
//   chip × button or Backspace on the empty entry.
//   Attributes: placeholder, disabled, max-tags, dedupe.
//   Property: .tags (array of strings — the committed chip values), .separators
//   (array of single-char separator strings, default [',']).
//   Events: add (detail: {tag}), remove (detail: {tag, index}), change
//   (detail: {tags}). All bubble. Chips reuse <tf-chip removable>.
// =============================================================================

class TfTagInput extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'disabled', 'max-tags', 'dedupe'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._chipHost = null;
    this._input = null;
    this._tags = [];
    this._separators = [','];
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onBlur = this._onBlur.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._update();
    this._renderTags();
  }

  attributeChangedCallback(oldName, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._update();
  }

  get tags() { return this._tags.slice(); }

  set tags(arr) {
    this._tags = Array.isArray(arr) ? arr.map((t) => String(t)) : [];
    this._renderTags();
  }

  get separators() { return this._separators.slice(); }

  set separators(arr) {
    const list = Array.isArray(arr)
      ? arr.map((s) => String(s)).filter((s) => s.length > 0)
      : [];
    // Only single characters work as keypress separators; multi-char tokens are
    // ignored here (the renderer still trims/splits on paste-style input).
    this._separators = list.length > 0 ? list : [','];
  }

  get disabled() { return this.hasAttribute('disabled'); }

  set disabled(v) {
    if (v) this.setAttribute('disabled', '');
    else this.removeAttribute('disabled');
    if (this._input) this._input.disabled = !!v;
  }

  focus() { this._input?.focus(); }

  _maxTags() {
    const raw = parseInt(this.getAttribute('max-tags') || '', 10);
    return Number.isInteger(raw) && raw > 0 ? raw : null;
  }

  _dedupe() { return this.hasAttribute('dedupe'); }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-tag-input';

    const chipHost = document.createElement('span');
    chipHost.className = 'tf-tag-input-chips';
    wrap.appendChild(chipHost);

    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'tf-tag-input-entry';
    input.setAttribute('role', 'textbox');
    input.addEventListener('keydown', this._onKeyDown);
    input.addEventListener('blur', this._onBlur);
    wrap.appendChild(input);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._chipHost = chipHost;
    this._input = input;
  }

  _update() {
    const placeholder = this.getAttribute('placeholder') || '';
    const disabled = this.hasAttribute('disabled');
    this._input.placeholder = placeholder;
    this._input.disabled = disabled;
    const ariaLabel = this.getAttribute('aria-label') || '';
    if (ariaLabel) this._input.setAttribute('aria-label', ariaLabel);
    else this._input.removeAttribute('aria-label');
  }

  _renderTags() {
    if (!this._chipHost) return;
    this._chipHost.innerHTML = '';
    for (let i = 0; i < this._tags.length; i++) {
      const tag = this._tags[i];
      const chip = document.createElement('tf-chip');
      chip.setAttribute('variant', 'tag');
      chip.setAttribute('tone', 'neutral');
      if (!this.hasAttribute('disabled')) chip.setAttribute('removable', '');
      chip.setAttribute('label', tag);
      chip.addEventListener('remove', () => this._removeAt(i));
      this._chipHost.appendChild(chip);
    }
  }

  /// Attempts to commit `raw` as a new tag, honouring max-tags + dedupe. Returns
  /// the trimmed value that was added, or null when nothing was added.
  _commit(raw) {
    if (this.hasAttribute('disabled')) return null;
    const value = String(raw).trim();
    if (value.length === 0) return null;
    if (this._dedupe() && this._tags.includes(value)) return null;
    const max = this._maxTags();
    if (max != null && this._tags.length >= max) return null;
    this._tags.push(value);
    this._renderTags();
    this.dispatchEvent(new CustomEvent('add', { bubbles: true, detail: { tag: value } }));
    this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { tags: this.tags } }));
    return value;
  }

  _removeAt(index) {
    if (this.hasAttribute('disabled')) return;
    if (index < 0 || index >= this._tags.length) return;
    const [tag] = this._tags.splice(index, 1);
    this._renderTags();
    this.dispatchEvent(new CustomEvent('remove', { bubbles: true, detail: { tag, index } }));
    this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { tags: this.tags } }));
  }

  _onKeyDown(e) {
    if (this.hasAttribute('disabled')) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      if (this._commit(this._input.value) != null) this._input.value = '';
      return;
    }
    if (e.key === 'Backspace' && this._input.value.length === 0 && this._tags.length > 0) {
      e.preventDefault();
      this._removeAt(this._tags.length - 1);
      return;
    }
    if (e.key.length === 1 && this._separators.includes(e.key)) {
      e.preventDefault();
      if (this._commit(this._input.value) != null) this._input.value = '';
    }
  }

  _onBlur() {
    if (this.hasAttribute('disabled')) return;
    if (this._input.value.trim().length === 0) return;
    if (this._commit(this._input.value) != null) this._input.value = '';
  }
}

customElements.define('tf-tag-input', TfTagInput);
export { TfTagInput };
