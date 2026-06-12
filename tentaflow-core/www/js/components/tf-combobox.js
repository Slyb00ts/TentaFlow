// =============================================================================
// File: tf-combobox.js
// Description: <tf-combobox> — autocomplete input with dropdown suggestions.
//   Attributes: placeholder, value, disabled, label, clearable, min-chars,
//   free-input.
//   Property: .options (array of {value, label, description, icon, group,
//   disabled}); icon may be a string or a DOM Element (e.g. an SVG node).
//   Events: change (detail: {value, label} — value/label are null on clear,
//   free-input commits carry {value: <raw text>, label: null, free: true}),
//   input (detail: {query}).
// =============================================================================

class TfCombobox extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'value', 'disabled', 'label', 'clearable', 'min-chars', 'free-input', 'aria-label'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._input = null;
    this._labelEl = null;
    this._clearBtn = null;
    this._popover = null;
    this._optionEls = [];
    this._options = [];
    this._activeIdx = -1;
    this._isOpen = false;
    // Stable per-instance prefix for option element ids (aria-activedescendant).
    this._uid = `tfcb-${Math.random().toString(36).slice(2, 8)}`;
    this._onInput = this._onInput.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onFocus = this._onFocus.bind(this);
    this._onDocClick = this._onDocClick.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) {
      this._build();
      // Options assigned before the element was connected could not render
      // yet (popover did not exist) — materialize them now.
      if (this._options.length > 0) this._rebuildOptions();
    }
    this._update();
    document.addEventListener('click', this._onDocClick);
  }

  disconnectedCallback() {
    document.removeEventListener('click', this._onDocClick);
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._update();
  }

  get value() {
    return this._input ? this._input.value : (this.getAttribute('value') || '');
  }

  set value(v) {
    const next = v ?? '';
    this.setAttribute('value', next);
    // Never clobber in-progress typing — mirror only when not focused.
    if (this._input && document.activeElement !== this._input) this._input.value = next;
    this._syncClear();
  }

  get disabled() { return this.hasAttribute('disabled'); }

  set disabled(v) {
    if (v) this.setAttribute('disabled', '');
    else this.removeAttribute('disabled');
    if (this._input) this._input.disabled = !!v;
    this._syncClear();
  }

  get options() { return this._options; }

  set options(arr) {
    this._options = Array.isArray(arr) ? arr : [];
    this._rebuildOptions();
  }

  focus() { this._input?.focus(); }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-combobox';

    const labelEl = document.createElement('span');
    labelEl.className = 'tf-combobox-label';
    labelEl.id = `${this._uid}-label`;
    labelEl.style.display = 'none';
    wrap.appendChild(labelEl);

    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'tf-combobox-input';
    input.setAttribute('role', 'combobox');
    input.setAttribute('aria-haspopup', 'listbox');
    input.setAttribute('aria-expanded', 'false');
    input.setAttribute('aria-autocomplete', 'list');
    input.addEventListener('input', this._onInput);
    input.addEventListener('keydown', this._onKeyDown);
    input.addEventListener('focus', this._onFocus);
    wrap.appendChild(input);

    const clearBtn = document.createElement('button');
    clearBtn.type = 'button';
    clearBtn.className = 'tf-combobox-clear';
    clearBtn.setAttribute('aria-label', 'Clear selection');
    clearBtn.textContent = '×';
    clearBtn.hidden = true;
    clearBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (this.hasAttribute('disabled')) return;
      this._input.value = '';
      this.setAttribute('value', '');
      this._close();
      this._syncClear();
      this.dispatchEvent(new CustomEvent('change', {
        bubbles: true,
        detail: { value: null, label: null },
      }));
    });
    wrap.appendChild(clearBtn);

    const popover = document.createElement('div');
    popover.className = 'tf-combobox-popover';
    popover.hidden = true;
    popover.setAttribute('role', 'listbox');
    wrap.appendChild(popover);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._labelEl = labelEl;
    this._clearBtn = clearBtn;
    this._input = input;
    this._popover = popover;
  }

  _update() {
    const placeholder = this.getAttribute('placeholder') || '';
    const value = this.getAttribute('value') || '';
    const disabled = this.hasAttribute('disabled');
    const label = this.getAttribute('label') || '';

    this._input.placeholder = placeholder;
    if (document.activeElement !== this._input) this._input.value = value;
    this._input.disabled = disabled;
    this._labelEl.textContent = label;
    this._labelEl.style.display = label ? '' : 'none';
    // The focusable element is the inner <input>; mirror the accessible name
    // onto it (visible label wins over a host aria-label).
    const ariaLabel = this.getAttribute('aria-label') || '';
    if (label) {
      this._input.setAttribute('aria-labelledby', this._labelEl.id);
      this._input.removeAttribute('aria-label');
    } else if (ariaLabel) {
      this._input.setAttribute('aria-label', ariaLabel);
      this._input.removeAttribute('aria-labelledby');
    } else {
      this._input.removeAttribute('aria-labelledby');
      this._input.removeAttribute('aria-label');
    }
    this._syncClear();
  }

  _minChars() {
    const raw = parseInt(this.getAttribute('min-chars') || '0', 10);
    return Number.isInteger(raw) && raw > 0 ? raw : 0;
  }

  _syncClear() {
    if (!this._clearBtn || !this._input) return;
    const show = this.hasAttribute('clearable') && this._input.value.length > 0;
    this._clearBtn.hidden = !show;
    this._clearBtn.disabled = this.hasAttribute('disabled');
  }

  _rebuildOptions() {
    if (!this._popover) return;
    this._popover.innerHTML = '';
    this._optionEls = [];
    const groups = new Map();

    for (let i = 0; i < this._options.length; i++) {
      const opt = this._options[i];
      let container = this._popover;

      if (opt.group) {
        if (!groups.has(opt.group)) {
          const groupEl = document.createElement('div');
          groupEl.className = 'tf-combobox-group';
          const header = document.createElement('div');
          header.className = 'tf-combobox-group-header';
          header.textContent = opt.group;
          groupEl.appendChild(header);
          this._popover.appendChild(groupEl);
          groups.set(opt.group, groupEl);
        }
        container = groups.get(opt.group);
      }

      const el = document.createElement('div');
      el.className = 'tf-combobox-option';
      el.setAttribute('role', 'option');
      el.setAttribute('data-idx', String(i));
      el.id = `${this._uid}-opt-${i}`;
      if (opt.disabled) {
        el.classList.add('tf-combobox-option--disabled');
        el.setAttribute('aria-disabled', 'true');
      }

      if (opt.icon) {
        const iconEl = document.createElement('span');
        iconEl.className = 'tf-combobox-option-icon';
        // Icons may arrive as ready DOM nodes (e.g. SVG built by an icon
        // renderer) — append them instead of stringifying.
        if (opt.icon && typeof opt.icon === 'object' && opt.icon.nodeType === 1) {
          iconEl.appendChild(opt.icon);
        } else {
          iconEl.textContent = opt.icon;
        }
        el.appendChild(iconEl);
      }

      const labelEl = document.createElement('span');
      labelEl.className = 'tf-combobox-option-label';
      labelEl.textContent = opt.label || '';
      el.appendChild(labelEl);

      if (opt.description) {
        const descEl = document.createElement('span');
        descEl.className = 'tf-combobox-option-desc';
        descEl.textContent = opt.description;
        el.appendChild(descEl);
      }

      el.addEventListener('mousedown', (e) => {
        e.preventDefault();
        this._commitOption(i);
      });

      container.appendChild(el);
      this._optionEls.push({ el, opt, idx: i, visible: true });
    }
  }

  _filterOptions(query) {
    const q = query.trim().toLowerCase();
    for (const n of this._optionEls) {
      const label = (n.opt.label || '').toLowerCase();
      const desc = (n.opt.description || '').toLowerCase();
      n.visible = q.length === 0 || label.includes(q) || desc.includes(q);
      n.el.hidden = !n.visible;
    }
    // Reset active when current becomes hidden.
    const cur = this._optionEls[this._activeIdx];
    if (!cur || !cur.visible) {
      const vis = this._visibleNodes();
      this._setActive(vis.length > 0 ? vis[0].idx : -1);
    }
  }

  _visibleNodes() {
    return this._optionEls.filter(n => n.visible && !n.opt.disabled);
  }

  _setActive(idx) {
    this._activeIdx = idx;
    for (const n of this._optionEls) {
      n.el.classList.toggle('active', n.idx === idx);
    }
    if (idx >= 0) {
      const n = this._optionEls[idx];
      if (n?.el.id) this._input.setAttribute('aria-activedescendant', n.el.id);
      if (n?.el.scrollIntoView) {
        try { n.el.scrollIntoView({ block: 'nearest' }); } catch {}
      }
    } else {
      this._input.removeAttribute('aria-activedescendant');
    }
  }

  _moveActive(dir) {
    const vis = this._visibleNodes();
    if (vis.length === 0) return;
    let curPos = vis.findIndex(n => n.idx === this._activeIdx);
    let nextPos = curPos < 0
      ? (dir > 0 ? 0 : vis.length - 1)
      : (curPos + dir + vis.length) % vis.length;
    this._setActive(vis[nextPos].idx);
  }

  _open() {
    if (this._isOpen || this.hasAttribute('disabled')) return;
    // Gate every open path (input, focus, arrow keys): never open with no
    // options, and respect the min-chars search threshold.
    if (this._optionEls.length === 0) return;
    const minChars = this._minChars();
    if (minChars > 0 && this._input.value.length < Math.max(minChars, 1)) return;
    this._isOpen = true;
    this._popover.hidden = false;
    this._input.setAttribute('aria-expanded', 'true');
    this._wrap.classList.add('tf-combobox--open');
    const vis = this._visibleNodes();
    this._setActive(vis.length > 0 ? vis[0].idx : -1);
  }

  _close() {
    if (!this._isOpen) return;
    this._isOpen = false;
    this._popover.hidden = true;
    this._input.setAttribute('aria-expanded', 'false');
    this._wrap.classList.remove('tf-combobox--open');
    this._activeIdx = -1;
    for (const n of this._optionEls) n.el.classList.remove('active');
  }

  _commitOption(idx) {
    if (idx < 0 || idx >= this._options.length) return;
    const opt = this._options[idx];
    if (opt.disabled) return;
    this._input.value = opt.label || '';
    this.setAttribute('value', opt.label || '');
    this._close();
    this._syncClear();
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: opt.value, label: opt.label },
    }));
  }

  _commitFreeText() {
    const text = this._input.value;
    if (text.length === 0) return;
    this.setAttribute('value', text);
    this._close();
    this._syncClear();
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: text, label: null, free: true },
    }));
  }

  _onInput() {
    const q = this._input.value;
    this.setAttribute('value', q);
    this._filterOptions(q);
    const minChars = this._minChars();
    if (minChars > 0 && q.length < minChars) {
      // Below the search threshold the popover must stay closed.
      if (this._isOpen) this._close();
    } else if (!this._isOpen && q.length >= Math.max(minChars, 1) && this._optionEls.length > 0) {
      this._open();
    }
    this._syncClear();
    this.dispatchEvent(new CustomEvent('input', {
      bubbles: true,
      detail: { query: q },
    }));
  }

  _onKeyDown(e) {
    if (this.hasAttribute('disabled')) return;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        if (!this._isOpen) this._open();
        else this._moveActive(1);
        return;
      case 'ArrowUp':
        e.preventDefault();
        if (!this._isOpen) this._open();
        else this._moveActive(-1);
        return;
      case 'Home': {
        if (!this._isOpen) return;
        e.preventDefault();
        const vis = this._visibleNodes();
        if (vis.length > 0) this._setActive(vis[0].idx);
        return;
      }
      case 'End': {
        if (!this._isOpen) return;
        e.preventDefault();
        const vis = this._visibleNodes();
        if (vis.length > 0) this._setActive(vis[vis.length - 1].idx);
        return;
      }
      case 'Enter':
        e.preventDefault();
        if (this._isOpen && this._activeIdx >= 0) {
          this._commitOption(this._activeIdx);
        } else if (this.hasAttribute('free-input')) {
          this._commitFreeText();
        }
        return;
      case 'Escape':
        if (this._isOpen) { e.preventDefault(); this._close(); }
        return;
      case 'Tab':
        if (this._isOpen) this._close();
        return;
    }
  }

  _onFocus() {
    if (this.hasAttribute('disabled') || this._optionEls.length === 0) return;
    if (this._input.value.length < this._minChars()) return;
    this._open();
  }

  _onDocClick(e) {
    if (!this._isOpen) return;
    if (this.contains(e.target)) return;
    this._close();
  }
}

customElements.define('tf-combobox', TfCombobox);
export { TfCombobox };
