// =============================================================================
// File: tf-list.js
// Description: <tf-list> — scrollable list with selectable items, severity
//              borders, trailing chips and icon/avatar support. Light DOM.
// Example: const list = document.querySelector('tf-list');
//          list.items = [{id:'1', title:'Server A', sub:'Online', severity:'success'}];
// =============================================================================

class TfList extends HTMLElement {
  static get observedAttributes() {
    return ['compact', 'selectable'];
  }

  constructor() {
    super();
    this._container = null;
    this._items = [];
    this._selectedIndex = -1;
  }

  connectedCallback() {
    if (!this._container) this._build();
    this._render();
  }

  attributeChangedCallback() {
    if (this._container) this._render();
  }

  set items(val) {
    this._items = Array.isArray(val) ? val : [];
    this._selectedIndex = -1;
    if (this._container) this._render();
  }

  get items() {
    return this._items;
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-list tf-scroll';
    el.addEventListener('click', (e) => this._onClick(e));
    this.appendChild(el);
    this._container = el;
  }

  _render() {
    const compact = this.hasAttribute('compact');
    const items = this._items;

    if (!items.length) {
      this._container.innerHTML = '';
      return;
    }

    const html = items.map((item, i) => {
      const sevCls = item.severity ? ` severity-${item.severity}` : '';
      const selCls = (this._selectedIndex === i) ? ' selected' : '';
      const compactCls = compact ? ' compact' : '';

      const iconHtml = item.icon
        ? `<div class="tf-list-item-icon"><svg width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-${item.icon}"/></svg></div>`
        : '';

      const subHtml = item.sub
        ? `<span class="tf-list-item-sub">${item.sub}</span>`
        : '';

      const chipHtml = item.chip
        ? `<span class="tf-chip ${item.chipTone || 'info'}">${item.chip}</span>`
        : '';

      return `<div class="tf-list-item${sevCls}${selCls}${compactCls}" data-index="${i}">${iconHtml}<div class="tf-list-item-body"><span class="tf-list-item-title">${item.title || ''}</span>${subHtml}</div>${chipHtml}</div>`;
    }).join('');

    this._container.innerHTML = html;
  }

  _onClick(e) {
    const row = e.target.closest('.tf-list-item');
    if (!row) return;
    const index = parseInt(row.dataset.index, 10);
    const item = this._items[index];
    if (!item) return;

    if (this.hasAttribute('selectable')) {
      this._selectedIndex = index;
      this._container.querySelectorAll('.tf-list-item').forEach((el, i) => {
        el.classList.toggle('selected', i === index);
      });
    }

    this.dispatchEvent(new CustomEvent('item-click', {
      bubbles: true,
      detail: { item, index },
    }));
  }
}

customElements.define('tf-list', TfList);
export { TfList };
