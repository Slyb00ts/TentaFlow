// =============================================================================
// File: tf-multiselect.js
// Description: <tf-multiselect> — multi-select with chips display.
//   Attributes: placeholder, disabled, label, clearable, select-all,
//   max-selections, no-search.
//   Properties: .options (array of {value, label, icon, group, disabled});
//   icon may be a string or a DOM Element. .value (array of selected values).
//   Events: change (detail: {value: array of selected option values}).
// =============================================================================

class TfMultiselect extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'disabled', 'label', 'clearable', 'select-all', 'max-selections', 'no-search', 'aria-label'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._trigger = null;
    this._labelEl = null;
    this._clearBtn = null;
    this._selectAllBtn = null;
    this._optionsBox = null;
    this._chipsArea = null;
    this._popover = null;
    this._searchInput = null;
    this._optionEls = [];
    this._options = [];
    this._selected = [];
    this._isOpen = false;
    this._activeIdx = -1;
    // Stable per-instance prefix for option element ids (aria-activedescendant).
    this._uid = `tfms-${Math.random().toString(36).slice(2, 8)}`;
    this._onDocClick = this._onDocClick.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) {
      this._build();
      // Options/value assigned before the element was connected could not
      // render yet — materialize them now.
      if (this._options.length > 0) this._rebuildOptions();
    }
    this._updateTrigger();
    this._refreshAriaSelected();
    document.addEventListener('click', this._onDocClick);
  }

  disconnectedCallback() {
    document.removeEventListener('click', this._onDocClick);
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._updateTrigger();
  }

  get disabled() { return this.hasAttribute('disabled'); }

  set disabled(v) {
    if (v) this.setAttribute('disabled', '');
    else this.removeAttribute('disabled');
    if (this._wrap) this._updateTrigger();
  }

  get value() { return [...this._selected]; }

  set value(arr) {
    this._selected = Array.isArray(arr) ? [...arr] : [];
    this._refreshChips();
    this._refreshAriaSelected();
  }

  get options() { return this._options; }

  set options(arr) {
    this._options = Array.isArray(arr) ? arr : [];
    this._rebuildOptions();
    this._refreshChips();
    this._refreshAriaSelected();
  }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-multiselect';

    const labelEl = document.createElement('div');
    labelEl.className = 'tf-multiselect-label';
    labelEl.id = `${this._uid}-label`;
    labelEl.style.display = 'none';
    wrap.appendChild(labelEl);

    const trigger = document.createElement('div');
    trigger.className = 'tf-multiselect-trigger';
    trigger.setAttribute('role', 'combobox');
    trigger.setAttribute('aria-haspopup', 'listbox');
    trigger.setAttribute('aria-expanded', 'false');
    trigger.setAttribute('tabindex', '0');

    const chipsArea = document.createElement('span');
    chipsArea.className = 'tf-multiselect-chips';
    trigger.appendChild(chipsArea);

    const clearBtn = document.createElement('button');
    clearBtn.type = 'button';
    clearBtn.className = 'tf-multiselect-clear';
    clearBtn.setAttribute('aria-label', 'Clear all selections');
    clearBtn.setAttribute('tabindex', '-1');
    clearBtn.textContent = '×';
    clearBtn.hidden = true;
    clearBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (this.hasAttribute('disabled')) return;
      if (this._selected.length === 0) return;
      this._selected = [];
      this._refreshChips();
      this._refreshAriaSelected();
      this._emitChange();
    });
    trigger.appendChild(clearBtn);

    const caret = document.createElement('span');
    caret.className = 'tf-multiselect-caret';
    caret.setAttribute('aria-hidden', 'true');
    caret.textContent = '▾';
    trigger.appendChild(caret);

    trigger.addEventListener('click', (e) => {
      if (this.hasAttribute('disabled')) return;
      if (e.target === clearBtn) return;
      if (e.target.classList?.contains('tf-multiselect-chip-remove')) return;
      e.preventDefault();
      if (this._isOpen) this._close(); else this._open();
    });

    trigger.addEventListener('keydown', (e) => {
      if (this.hasAttribute('disabled')) return;
      switch (e.key) {
        case 'ArrowDown':
          e.preventDefault();
          if (!this._isOpen) this._open(); else this._moveActive(1);
          return;
        case 'ArrowUp':
          e.preventDefault();
          if (!this._isOpen) this._open(); else this._moveActive(-1);
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
        case ' ':
          e.preventDefault();
          if (!this._isOpen) this._open();
          else if (this._activeIdx >= 0) this._toggle(this._activeIdx);
          return;
        case 'Escape':
          if (this._isOpen) { e.preventDefault(); this._close(); }
          return;
        case 'Tab':
          if (this._isOpen) this._close();
          return;
      }
    });

    wrap.appendChild(trigger);

    const popover = document.createElement('div');
    popover.className = 'tf-multiselect-popover';
    popover.hidden = true;
    popover.setAttribute('role', 'listbox');
    popover.setAttribute('aria-multiselectable', 'true');

    const search = document.createElement('input');
    search.type = 'text';
    search.className = 'tf-multiselect-search';
    search.placeholder = 'Search...';
    search.addEventListener('input', () => this._filterOptions(search.value));
    search.addEventListener('keydown', (e) => {
      if (['ArrowDown', 'ArrowUp', 'Home', 'End', 'Enter', 'Escape', 'Tab'].includes(e.key)) {
        trigger.dispatchEvent(new KeyboardEvent('keydown', {
          key: e.key, bubbles: false, cancelable: true,
        }));
        e.preventDefault();
      }
    });
    popover.appendChild(search);
    this._searchInput = search;

    const selectAllBtn = document.createElement('button');
    selectAllBtn.type = 'button';
    selectAllBtn.className = 'tf-multiselect-select-all';
    selectAllBtn.hidden = true;
    selectAllBtn.addEventListener('click', (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (selectAllBtn.disabled || this.hasAttribute('disabled')) return;
      const mode = selectAllBtn.dataset.mode;
      if (mode === 'clear') {
        if (this._selected.length === 0) return;
        this._selected = [];
      } else if (mode === 'all') {
        this._selected = this._options.filter(o => !o.disabled).map(o => o.value);
      } else {
        return;
      }
      this._refreshChips();
      this._refreshAriaSelected();
      this._emitChange();
    });
    popover.appendChild(selectAllBtn);

    // Dedicated container so option/group rebuilds never touch the search
    // input or the select-all header.
    const optionsBox = document.createElement('div');
    optionsBox.className = 'tf-multiselect-options';
    optionsBox.setAttribute('role', 'presentation');
    popover.appendChild(optionsBox);

    wrap.appendChild(popover);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._trigger = trigger;
    this._labelEl = labelEl;
    this._clearBtn = clearBtn;
    this._selectAllBtn = selectAllBtn;
    this._optionsBox = optionsBox;
    this._chipsArea = chipsArea;
    this._popover = popover;
  }

  _maxSelections() {
    const raw = parseInt(this.getAttribute('max-selections') || '0', 10);
    return Number.isInteger(raw) && raw > 0 ? raw : null;
  }

  _updateTrigger() {
    const disabled = this.hasAttribute('disabled');
    if (disabled) {
      this._trigger.removeAttribute('tabindex');
      this._trigger.setAttribute('aria-disabled', 'true');
    } else {
      this._trigger.setAttribute('tabindex', '0');
      this._trigger.removeAttribute('aria-disabled');
    }
    const label = this.getAttribute('label') || '';
    this._labelEl.textContent = label;
    this._labelEl.style.display = label ? '' : 'none';
    if (label) this._trigger.setAttribute('aria-labelledby', this._labelEl.id);
    else this._trigger.removeAttribute('aria-labelledby');
    // The host aria-label must land on the focusable trigger to be announced.
    const ariaLabel = this.getAttribute('aria-label');
    if (ariaLabel) this._trigger.setAttribute('aria-label', ariaLabel);
    else this._trigger.removeAttribute('aria-label');
    if (this._searchInput) this._searchInput.hidden = this.hasAttribute('no-search');
    this._refreshChips();
    this._refreshAriaSelected();
  }

  _rebuildOptions() {
    if (!this._optionsBox) return;
    this._optionsBox.innerHTML = '';
    this._optionEls = [];
    const groups = new Map();

    for (let i = 0; i < this._options.length; i++) {
      const opt = this._options[i];
      let container = this._optionsBox;

      if (opt.group) {
        if (!groups.has(opt.group)) {
          const groupEl = document.createElement('div');
          groupEl.className = 'tf-multiselect-group';
          groupEl.setAttribute('role', 'group');
          const header = document.createElement('div');
          header.className = 'tf-multiselect-group-header';
          header.id = `${this._uid}-grp-${groups.size}`;
          header.textContent = opt.group;
          groupEl.setAttribute('aria-labelledby', header.id);
          groupEl.appendChild(header);
          this._optionsBox.appendChild(groupEl);
          groups.set(opt.group, groupEl);
        }
        container = groups.get(opt.group);
      }

      const el = document.createElement('div');
      el.className = 'tf-multiselect-option';
      el.setAttribute('role', 'option');
      el.setAttribute('data-idx', String(i));
      el.id = `${this._uid}-opt-${i}`;
      if (opt.disabled) {
        el.classList.add('tf-multiselect-option--disabled');
        el.setAttribute('aria-disabled', 'true');
      }

      const check = document.createElement('span');
      check.className = 'tf-multiselect-option-check';
      check.setAttribute('aria-hidden', 'true');
      el.appendChild(check);

      if (opt.icon) {
        const iconEl = document.createElement('span');
        iconEl.className = 'tf-multiselect-option-icon';
        // Icons may arrive as ready DOM nodes (e.g. an SVG built by an icon
        // renderer) — append them instead of stringifying.
        if (opt.icon && typeof opt.icon === 'object' && opt.icon.nodeType === 1) {
          iconEl.appendChild(opt.icon);
        } else {
          iconEl.textContent = opt.icon;
        }
        el.appendChild(iconEl);
      }

      const labelEl = document.createElement('span');
      labelEl.className = 'tf-multiselect-option-label';
      labelEl.textContent = opt.label || '';
      el.appendChild(labelEl);

      if (opt.description) {
        const descEl = document.createElement('span');
        descEl.className = 'tf-multiselect-option-description';
        descEl.textContent = opt.description;
        el.appendChild(descEl);
      }

      el.addEventListener('mousedown', (e) => {
        e.preventDefault();
        this._toggle(i);
      });

      container.appendChild(el);
      this._optionEls.push({ el, opt, idx: i, visible: true, check });
    }
  }

  _refreshChips() {
    if (!this._chipsArea) return;
    const disabled = this.hasAttribute('disabled');
    this._chipsArea.innerHTML = '';
    if (this._clearBtn) {
      this._clearBtn.hidden = !this.hasAttribute('clearable') || this._selected.length === 0;
      this._clearBtn.disabled = disabled;
    }
    if (this._selected.length === 0) {
      const ph = document.createElement('span');
      ph.className = 'tf-multiselect-placeholder';
      ph.textContent = this.getAttribute('placeholder') || '';
      this._chipsArea.appendChild(ph);
      return;
    }
    for (const val of this._selected) {
      const opt = this._options.find(o => o.value === val);
      const chip = document.createElement('span');
      chip.className = 'tf-multiselect-chip';

      const chipLabel = document.createElement('span');
      chipLabel.textContent = opt ? opt.label : val;
      chip.appendChild(chipLabel);

      const rm = document.createElement('button');
      rm.type = 'button';
      rm.className = 'tf-multiselect-chip-remove';
      rm.setAttribute('aria-label', `Remove ${chipLabel.textContent}`);
      rm.setAttribute('tabindex', '-1');
      rm.textContent = '×';
      rm.disabled = disabled;
      rm.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (this.hasAttribute('disabled')) return;
        this._removeValue(val);
      });
      chip.appendChild(rm);
      this._chipsArea.appendChild(chip);
    }
  }

  _refreshAriaSelected() {
    for (const n of this._optionEls) {
      const isSel = this._selected.includes(n.opt.value);
      n.el.classList.toggle('tf-multiselect-option--selected', isSel);
      n.check.textContent = isSel ? '✓' : '';
      if (isSel) n.el.setAttribute('aria-selected', 'true');
      else n.el.removeAttribute('aria-selected');
    }
    this._refreshSelectAll();
  }

  _refreshSelectAll() {
    const btn = this._selectAllBtn;
    if (!btn) return;
    if (!this.hasAttribute('select-all')) {
      btn.hidden = true;
      return;
    }
    btn.hidden = false;
    const enabled = this._options.filter(o => !o.disabled);
    if (this.hasAttribute('disabled') || enabled.length === 0) {
      btn.disabled = true;
      btn.textContent = 'Select all';
      btn.dataset.mode = 'noop';
      return;
    }
    const max = this._maxSelections();
    const allSelected = enabled.every(o => this._selected.includes(o.value));
    const anySelected = enabled.some(o => this._selected.includes(o.value));
    if (allSelected) {
      btn.disabled = false;
      btn.textContent = 'Clear all';
      btn.dataset.mode = 'clear';
    } else if (max != null && enabled.length > max) {
      // "Select all" cannot fit within max-selections — offer "Clear all"
      // when anything is selected, otherwise the button is a no-op.
      btn.disabled = !anySelected;
      btn.textContent = anySelected ? 'Clear all' : 'Select all';
      btn.dataset.mode = anySelected ? 'clear' : 'noop';
    } else {
      btn.disabled = false;
      btn.textContent = 'Select all';
      btn.dataset.mode = 'all';
    }
  }

  _filterOptions(query) {
    const q = query.trim().toLowerCase();
    for (const n of this._optionEls) {
      const label = (n.opt.label || '').toLowerCase();
      n.visible = q.length === 0 || label.includes(q);
      n.el.hidden = !n.visible;
    }
    // Hide group blocks whose options are all filtered out.
    if (this._optionsBox) {
      for (const groupEl of this._optionsBox.querySelectorAll('.tf-multiselect-group')) {
        const anyVisible = this._optionEls.some(n => n.visible && n.el.parentElement === groupEl);
        groupEl.hidden = !anyVisible;
      }
    }
    const cur = this._optionEls[this._activeIdx];
    if (!cur || !cur.visible || cur.opt.disabled) {
      const vis = this._visibleNodes();
      this._setActive(vis.length > 0 ? vis[0].idx : -1);
    }
  }

  _toggle(idx) {
    if (this.hasAttribute('disabled')) return;
    const opt = this._options[idx];
    if (!opt || opt.disabled) return;
    const pos = this._selected.indexOf(opt.value);
    if (pos >= 0) {
      this._selected.splice(pos, 1);
    } else {
      const max = this._maxSelections();
      if (max != null && this._selected.length >= max) return;
      this._selected.push(opt.value);
    }
    this._refreshChips();
    this._refreshAriaSelected();
    this._emitChange();
  }

  _removeValue(val) {
    const pos = this._selected.indexOf(val);
    if (pos >= 0) this._selected.splice(pos, 1);
    this._refreshChips();
    this._refreshAriaSelected();
    this._emitChange();
  }

  _emitChange() {
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: [...this._selected] },
    }));
  }

  _visibleNodes() {
    return this._optionEls.filter(n => n.visible && !n.opt.disabled);
  }

  _setActive(idx) {
    this._activeIdx = idx;
    for (const n of this._optionEls) {
      n.el.classList.toggle('tf-multiselect-option--active', n.idx === idx);
    }
    if (idx >= 0) {
      const n = this._optionEls[idx];
      if (n?.el.id) this._trigger.setAttribute('aria-activedescendant', n.el.id);
    } else {
      this._trigger.removeAttribute('aria-activedescendant');
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
    this._isOpen = true;
    this._popover.hidden = false;
    this._trigger.setAttribute('aria-expanded', 'true');
    this._wrap.classList.add('tf-multiselect--open');
    this._refreshAriaSelected();
    if (this._searchInput && !this._searchInput.hidden) {
      this._searchInput.value = '';
      this._filterOptions('');
      try { this._searchInput.focus(); } catch {}
    }
    const vis = this._visibleNodes();
    this._setActive(vis.length > 0 ? vis[0].idx : -1);
  }

  _close() {
    if (!this._isOpen) return;
    this._isOpen = false;
    this._popover.hidden = true;
    this._trigger.setAttribute('aria-expanded', 'false');
    this._wrap.classList.remove('tf-multiselect--open');
    this._setActive(-1);
    try { this._trigger.focus(); } catch {}
  }

  _onDocClick(e) {
    if (!this._isOpen) return;
    if (this.contains(e.target)) return;
    this._close();
  }
}

customElements.define('tf-multiselect', TfMultiselect);
export { TfMultiselect };
