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

  // Items come from addon/store data, so every value is treated as untrusted:
  // rows are built via createElement/textContent (never innerHTML with
  // interpolated strings) and class-bearing fields are reduced to a safe token.
  _render() {
    const compact = this.hasAttribute('compact');
    const items = this._items;

    if (!items.length) {
      this._container.replaceChildren();
      return;
    }

    const classToken = (v) => String(v).replace(/[^a-zA-Z0-9_-]/g, '');

    const frag = document.createDocumentFragment();
    items.forEach((item, i) => {
      const row = document.createElement('div');
      row.className = 'tf-list-item'
        + (item.severity ? ` severity-${classToken(item.severity)}` : '')
        + (this._selectedIndex === i ? ' selected' : '')
        + (compact ? ' compact' : '');
      row.dataset.index = String(i);

      if (item.icon) {
        const iconWrap = document.createElement('div');
        iconWrap.className = 'tf-list-item-icon';
        const SVG_NS = 'http://www.w3.org/2000/svg';
        const svg = document.createElementNS(SVG_NS, 'svg');
        svg.setAttribute('width', '16');
        svg.setAttribute('height', '16');
        svg.setAttribute('fill', 'none');
        svg.setAttribute('stroke', 'currentColor');
        svg.setAttribute('stroke-width', '2');
        svg.setAttribute('stroke-linecap', 'round');
        svg.setAttribute('stroke-linejoin', 'round');
        svg.setAttribute('aria-hidden', 'true');
        const use = document.createElementNS(SVG_NS, 'use');
        use.setAttribute('href', `#i-${item.icon}`);
        svg.appendChild(use);
        iconWrap.appendChild(svg);
        row.appendChild(iconWrap);
      }

      const body = document.createElement('div');
      body.className = 'tf-list-item-body';
      const title = document.createElement('span');
      title.className = 'tf-list-item-title';
      title.textContent = item.title || '';
      body.appendChild(title);
      if (item.sub) {
        const sub = document.createElement('span');
        sub.className = 'tf-list-item-sub';
        sub.textContent = item.sub;
        body.appendChild(sub);
      }
      row.appendChild(body);

      if (item.chip) {
        const chip = document.createElement('span');
        chip.className = `tf-chip ${item.chipTone ? classToken(item.chipTone) : 'info'}`;
        chip.textContent = item.chip;
        row.appendChild(chip);
      }

      frag.appendChild(row);
    });

    this._container.replaceChildren(frag);
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
