// =============================================================================
// File: tf-multiselect.js
// Description: <tf-multiselect> — multi-select with chips display.
//   Attributes: placeholder, disabled.
//   Properties: .options (array of {value, label, icon}), .value (array of
//   selected values).
//   Events: change (detail: {value: string[]}).
// =============================================================================

class TfMultiselect extends HTMLElement {
  static get observedAttributes() {
    return ['placeholder', 'disabled'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._trigger = null;
    this._chipsArea = null;
    this._popover = null;
    this._searchInput = null;
    this._optionEls = [];
    this._options = [];
    this._selected = [];
    this._isOpen = false;
    this._activeIdx = -1;
    this._onDocClick = this._onDocClick.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._updateTrigger();
    document.addEventListener('click', this._onDocClick);
  }

  disconnectedCallback() {
    document.removeEventListener('click', this._onDocClick);
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    this._updateTrigger();
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
  }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-multiselect';

    const trigger = document.createElement('div');
    trigger.className = 'tf-multiselect-trigger';
    trigger.setAttribute('role', 'combobox');
    trigger.setAttribute('aria-haspopup', 'listbox');
    trigger.setAttribute('aria-expanded', 'false');
    trigger.setAttribute('tabindex', '0');

    const chipsArea = document.createElement('span');
    chipsArea.className = 'tf-multiselect-chips';
    trigger.appendChild(chipsArea);

    const caret = document.createElement('span');
    caret.className = 'tf-multiselect-caret';
    caret.setAttribute('aria-hidden', 'true');
    caret.textContent = '▾';
    trigger.appendChild(caret);

    trigger.addEventListener('click', (e) => {
      if (this.hasAttribute('disabled')) return;
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
      if (['ArrowDown', 'ArrowUp', 'Enter', 'Escape', 'Tab'].includes(e.key)) {
        trigger.dispatchEvent(new KeyboardEvent('keydown', {
          key: e.key, bubbles: false, cancelable: true,
        }));
        e.preventDefault();
      }
    });
    popover.appendChild(search);
    this._searchInput = search;

    wrap.appendChild(popover);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._trigger = trigger;
    this._chipsArea = chipsArea;
    this._popover = popover;
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
    this._refreshChips();
  }

  _rebuildOptions() {
    // Remove old option elements (keep search input).
    const oldOpts = this._popover.querySelectorAll('[role="option"]');
    oldOpts.forEach(el => el.remove());
    this._optionEls = [];

    for (let i = 0; i < this._options.length; i++) {
      const opt = this._options[i];
      const el = document.createElement('div');
      el.className = 'tf-multiselect-option';
      el.setAttribute('role', 'option');
      el.setAttribute('data-idx', String(i));

      const check = document.createElement('span');
      check.className = 'tf-multiselect-option-check';
      check.setAttribute('aria-hidden', 'true');
      el.appendChild(check);

      if (opt.icon) {
        const iconEl = document.createElement('span');
        iconEl.className = 'tf-multiselect-option-icon';
        iconEl.textContent = opt.icon;
        el.appendChild(iconEl);
      }

      const labelEl = document.createElement('span');
      labelEl.className = 'tf-multiselect-option-label';
      labelEl.textContent = opt.label || '';
      el.appendChild(labelEl);

      el.addEventListener('mousedown', (e) => {
        e.preventDefault();
        this._toggle(i);
      });

      this._popover.appendChild(el);
      this._optionEls.push({ el, opt, idx: i, visible: true, check });
    }
  }

  _refreshChips() {
    this._chipsArea.innerHTML = '';
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
      rm.addEventListener('click', (e) => {
        e.preventDefault();
        e.stopPropagation();
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
  }

  _filterOptions(query) {
    const q = query.trim().toLowerCase();
    for (const n of this._optionEls) {
      const label = (n.opt.label || '').toLowerCase();
      n.visible = q.length === 0 || label.includes(q);
      n.el.hidden = !n.visible;
    }
  }

  _toggle(idx) {
    if (this.hasAttribute('disabled')) return;
    const opt = this._options[idx];
    if (!opt) return;
    const pos = this._selected.indexOf(opt.value);
    if (pos >= 0) {
      this._selected.splice(pos, 1);
    } else {
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
    return this._optionEls.filter(n => n.visible);
  }

  _setActive(idx) {
    this._activeIdx = idx;
    for (const n of this._optionEls) {
      n.el.classList.toggle('tf-multiselect-option--active', n.idx === idx);
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
    if (this._searchInput) {
      this._searchInput.value = '';
      this._filterOptions('');
      try { this._searchInput.focus(); } catch {}
    }
  }

  _close() {
    if (!this._isOpen) return;
    this._isOpen = false;
    this._popover.hidden = true;
    this._trigger.setAttribute('aria-expanded', 'false');
    this._wrap.classList.remove('tf-multiselect--open');
    this._activeIdx = -1;
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
