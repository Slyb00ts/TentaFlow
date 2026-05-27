// =============================================================================
// File: tf-filter-chips.js
// Description: <tf-filter-chips> — inline filter bar with toggleable chip
//              buttons, single or multi select mode. Light DOM.
// Example: const fc = document.querySelector('tf-filter-chips');
//          fc.filters = [{id:'all', label:'All', active:true}, {id:'open', label:'Open'}];
// =============================================================================

class TfFilterChips extends HTMLElement {
  static get observedAttributes() {
    return ['mode'];
  }

  constructor() {
    super();
    this._container = null;
    this._filters = [];
  }

  connectedCallback() {
    if (!this._container) this._build();
    this._render();
  }

  attributeChangedCallback() {
    if (this._container) this._render();
  }

  set filters(val) {
    this._filters = Array.isArray(val) ? val.map(f => ({ ...f })) : [];
    if (this._container) this._render();
  }

  get filters() {
    return this._filters;
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-filter-chips';
    el.addEventListener('click', (e) => this._onClick(e));
    this.appendChild(el);
    this._container = el;
  }

  _render() {
    const html = this._filters.map((f, i) => {
      const activeCls = f.active ? ' active' : '';
      const iconHtml = f.icon
        ? `<svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-${f.icon}"/></svg>`
        : '';
      const countHtml = (f.count != null)
        ? `<span class="tf-filter-chip-count">${f.count}</span>`
        : '';
      return `<button class="tf-filter-chip${activeCls}" data-index="${i}" type="button">${iconHtml}<span>${f.label || ''}</span>${countHtml}</button>`;
    }).join('');

    this._container.innerHTML = html;
  }

  _onClick(e) {
    const btn = e.target.closest('.tf-filter-chip');
    if (!btn) return;
    const index = parseInt(btn.dataset.index, 10);
    const mode = this.getAttribute('mode') || 'single';

    if (mode === 'single') {
      this._filters.forEach((f, i) => { f.active = (i === index); });
    } else {
      this._filters[index].active = !this._filters[index].active;
    }

    this._render();

    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: {
        id: this._filters[index].id,
        active: this._filters[index].active,
        filters: this._filters,
      },
    }));
  }
}

customElements.define('tf-filter-chips', TfFilterChips);
export { TfFilterChips };
