// =============================================================================
// File: tf-select.js — native select with optional full-width wrapped caption.
// Options remain on the real select; wrap-selected only mirrors the chosen text.
// Attributes: value, disabled, name, label, prefix, dot, hint, wrap-selected.
// Event: change with detail.value.
// =============================================================================

class TfSelect extends HTMLElement {
  static get observedAttributes() {
    return ['value', 'disabled', 'name', 'label', 'prefix', 'dot', 'hint', 'wrap-selected'];
  }

  constructor() {
    super();
    this._group = null;
    this._labelEl = null;
    this._prefixEl = null;
    this._selectedEl = null;
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
    this._updateSelected();
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
    // Move top-level options and groups in order; flattening loses group labels.
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

    const prefix = document.createElement('span');
    prefix.className = 'tf-select-prefix';
    prefix.setAttribute('aria-hidden', 'true');

    const selected = document.createElement('span');
    selected.className = 'tf-select-selected';
    selected.setAttribute('aria-hidden', 'true');

    wrap.appendChild(prefix);
    wrap.appendChild(selected);
    wrap.appendChild(select);
    group.appendChild(wrap);
    const hint = document.createElement('span');
    hint.className = 'tf-hint';
    group.appendChild(hint);
    this.appendChild(group);
    this._hintEl = hint;

    this._group = group;
    this._labelEl = label;
    this._prefixEl = prefix;
    this._selectedEl = selected;
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
    const hintText = this.getAttribute('hint') || '';
    this._hintEl.textContent = hintText;
    this._hintEl.style.display = hintText ? '' : 'none';
    this._wrap.classList.toggle('tf-select-wrap--wrap-selected', this.hasAttribute('wrap-selected'));
    this._updatePrefix();
    this._updateSelected();
  }

  _updateSelected() {
    this._selectedEl.textContent = this._select.selectedOptions[0]?.textContent || '';
  }

  // The prefix sits over the field's left padding, so the padding follows its
  // measured width; the accessible name carries the same words for readers
  // that do not see the decorative span.
  _updatePrefix() {
    const text = this.getAttribute('prefix') || '';
    const dot = String(this.getAttribute('dot') || '').toLowerCase();
    const tone = ['ok', 'warn', 'err'].includes(dot) ? dot : '';
    this._prefixEl.innerHTML = '';
    if (tone) {
      const d = document.createElement('span');
      d.className = `tf-select-dot tf-select-dot--${tone}`;
      this._prefixEl.appendChild(d);
    }
    if (text) this._prefixEl.appendChild(document.createTextNode(text));
    const on = Boolean(text || tone);
    this._wrap.classList.toggle('tf-select-wrap--prefix', on);
    if (text) this._select.setAttribute('aria-label', text);
    else if (this.hasAttribute('wrap-selected') && this.getAttribute('label')) this._select.setAttribute('aria-label', this.getAttribute('label'));
    else if (this._select.getAttribute('aria-label') && !this.hasAttribute('aria-label')) this._select.removeAttribute('aria-label');
    if (!on) { this._select.style.paddingLeft = ''; this._selectedEl.style.paddingLeft = ''; return; }
    const measure = () => {
      const w = this._prefixEl.offsetWidth;
      if (w > 0) {
        this._select.style.paddingLeft = `${w + 22}px`;
        this._selectedEl.style.paddingLeft = `${w + 22}px`;
      }
    };
    measure();
    if (typeof requestAnimationFrame === 'function') requestAnimationFrame(measure);
  }

  _onChange(e) {
    // Stop the native event so callers receive only the typed component event.
    e.stopPropagation();
    this.setAttribute('value', this._select.value);
    this._updateSelected();
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this._select.value },
    }));
  }
}

customElements.define('tf-select', TfSelect);
export { TfSelect };
