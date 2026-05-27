// =============================================================================
// File: tf-command-palette.js
// Description: <tf-command-palette> — global search overlay (Cmd+K / Ctrl+K)
//              with grouped results, keyboard navigation, and search filtering.
//              Light DOM, singleton pattern via static open()/close().
// Example:
//   const pal = document.querySelector('tf-command-palette');
//   pal.items = [{id:'1', group:'Navigation', title:'Dashboard', shortcut:'G D'}];
//   TfCommandPalette.open();
// =============================================================================

class TfCommandPalette extends HTMLElement {
  constructor() {
    super();
    this._items = [];
    this._filtered = [];
    this._activeIdx = 0;
    this._visible = false;
    this._container = null;
    this._onKey = this._onKey.bind(this);
    this._onBackdrop = this._onBackdrop.bind(this);
  }

  connectedCallback() {
    if (!this._container) this._build();
    document.addEventListener('keydown', this._globalKey || (this._globalKey = (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        this._visible ? this._close() : this._open();
      }
    }));
  }

  disconnectedCallback() {
    if (this._globalKey) document.removeEventListener('keydown', this._globalKey);
  }

  set items(arr) {
    this._items = Array.isArray(arr) ? arr : [];
    this._filtered = [...this._items];
    if (this._visible) this._renderResults();
  }
  get items() { return this._items; }

  static open() {
    const el = document.querySelector('tf-command-palette');
    if (el) el._open();
  }

  static close() {
    const el = document.querySelector('tf-command-palette');
    if (el) el._close();
  }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-cmd-backdrop';
    wrap.style.display = 'none';
    wrap.addEventListener('mousedown', this._onBackdrop);

    wrap.innerHTML = `<div class="tf-cmd-palette">
      <div class="tf-cmd-search">
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
          <circle cx="7" cy="7" r="4.5"/><path d="M11 11l3 3"/>
        </svg>
        <input type="text" placeholder="Szukaj..." autocomplete="off" spellcheck="false" />
      </div>
      <div class="tf-cmd-results"></div>
    </div>`;

    this.appendChild(wrap);
    this._container = wrap;
    this._input = wrap.querySelector('input');
    this._resultsCont = wrap.querySelector('.tf-cmd-results');

    this._input.addEventListener('input', () => this._onSearch());
    this._input.addEventListener('keydown', this._onKey);
  }

  _open() {
    this._visible = true;
    this._activeIdx = 0;
    this._filtered = [...this._items];
    this._container.style.display = '';
    this._input.value = '';
    this._renderResults();
    requestAnimationFrame(() => {
      this._container.classList.add('open');
      this._input.focus();
    });
  }

  _close() {
    this._visible = false;
    this._container.classList.remove('open');
    this._container.classList.add('closing');
    setTimeout(() => {
      this._container.style.display = 'none';
      this._container.classList.remove('closing');
    }, 200);
  }

  _onBackdrop(e) {
    if (e.target === this._container) this._close();
  }

  _onSearch() {
    const q = this._input.value.toLowerCase().trim();
    this.dispatchEvent(new CustomEvent('search', { bubbles: true, detail: { query: q } }));
    if (!q) {
      this._filtered = [...this._items];
    } else {
      this._filtered = this._items.filter(it =>
        (it.title || '').toLowerCase().includes(q) ||
        (it.subtitle || '').toLowerCase().includes(q)
      );
    }
    this._activeIdx = 0;
    this._renderResults();
  }

  _onKey(e) {
    const count = this._filtered.length;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      this._activeIdx = (this._activeIdx + 1) % Math.max(count, 1);
      this._highlightActive();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      this._activeIdx = (this._activeIdx - 1 + Math.max(count, 1)) % Math.max(count, 1);
      this._highlightActive();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (this._filtered[this._activeIdx]) {
        this.dispatchEvent(new CustomEvent('select', { bubbles: true, detail: { item: this._filtered[this._activeIdx] } }));
        this._close();
      }
    } else if (e.key === 'Escape') {
      e.preventDefault();
      this._close();
    }
  }

  _highlightActive() {
    const items = this._resultsCont.querySelectorAll('.tf-cmd-item');
    items.forEach((el, i) => el.classList.toggle('active', i === this._activeIdx));
    if (items[this._activeIdx]) items[this._activeIdx].scrollIntoView({ block: 'nearest' });
  }

  _esc(s) {
    if (!s) return '';
    const d = document.createElement('span');
    d.textContent = s;
    return d.innerHTML;
  }

  _renderResults() {
    // Group items
    const groups = new Map();
    let flatIdx = 0;
    for (const it of this._filtered) {
      const g = it.group || '';
      if (!groups.has(g)) groups.set(g, []);
      groups.get(g).push({ ...it, _idx: flatIdx++ });
    }

    let html = '';
    for (const [group, items] of groups) {
      if (group) html += `<div class="tf-cmd-group">${this._esc(group)}</div>`;
      for (const it of items) {
        const activeCls = it._idx === this._activeIdx ? ' active' : '';
        const iconHtml = it.icon
          ? `<svg width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><use href="#i-${it.icon}"/></svg>`
          : '';
        const shortcut = it.shortcut
          ? `<span class="tf-kbd">${this._esc(it.shortcut)}</span>`
          : '';
        const subtitle = it.subtitle
          ? `<span class="tf-cmd-sub">${this._esc(it.subtitle)}</span>`
          : '';
        html += `<div class="tf-cmd-item${activeCls}" data-idx="${it._idx}">
          ${iconHtml}
          <div class="tf-cmd-item-text"><span>${this._esc(it.title)}</span>${subtitle}</div>
          ${shortcut}
        </div>`;
      }
    }

    if (!this._filtered.length) {
      html = '<div class="tf-cmd-empty">Brak wynikow</div>';
    }

    this._resultsCont.innerHTML = html;

    // Click handler for items
    this._resultsCont.querySelectorAll('.tf-cmd-item').forEach(el => {
      el.addEventListener('click', () => {
        const idx = parseInt(el.dataset.idx, 10);
        if (this._filtered[idx]) {
          this.dispatchEvent(new CustomEvent('select', { bubbles: true, detail: { item: this._filtered[idx] } }));
          this._close();
        }
      });
    });
  }
}

customElements.define('tf-command-palette', TfCommandPalette);
export { TfCommandPalette };
