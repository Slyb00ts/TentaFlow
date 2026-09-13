// =============================================================================
// File: tf-filter-chips.js
// Description: <tf-filter-chips> — inline filter bar with toggleable chip
//              buttons, single or multi select mode. Light DOM.
//              The `scroll` attribute keeps every chip on ONE horizontally
//              swipeable line instead of wrapping — on a phone a wrapping bar
//              of five filters costs three rows of vertical space. A scrolling
//              row also publishes `data-overflow` (none|start|end|both) on its
//              container so the edge fade in controls.css can say that there is
//              more to swipe to; a chip cut flush at the frame reads as a
//              rendering fault, not as an invitation.
// Example: const fc = document.querySelector('tf-filter-chips');
//          fc.filters = [{id:'all', label:'All', active:true}, {id:'open', label:'Open'}];
// =============================================================================

class TfFilterChips extends HTMLElement {
  static get observedAttributes() {
    return ['mode', 'clearable'];
  }

  constructor() {
    super();
    this._container = null;
    this._filters = [];
    this._observer = null;
    // The markup last written into the container, so an identical render can
    // skip the write entirely (see _render).
    this._renderedHtml = null;
    this._syncOverflow = this._syncOverflow.bind(this);
  }

  connectedCallback() {
    // A re-attached element still has its container — the container moved with
    // it — so `_build` is skipped, but `disconnectedCallback` dropped the
    // ResizeObserver. Without re-observing here a bar that is detached and put
    // back (a tab switch that re-parents its toolbar) never tracks its width
    // again, and the edge fade freezes on whatever the last resize said.
    if (!this._container) this._build();
    else this._observe();
    this._render();
  }

  disconnectedCallback() {
    if (this._observer) {
      this._observer.disconnect();
      this._observer = null;
    }
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
    this._renderedHtml = null;
    const el = document.createElement('div');
    el.className = 'tf-filter-chips';
    el.addEventListener('click', (e) => this._onClick(e));
    el.addEventListener('scroll', this._syncOverflow, { passive: true });
    this.appendChild(el);
    this._container = el;
    this._observe();
  }

  /// Starts tracking the container's width, unless it already is. Called both
  /// when the container is first built and when the element is re-attached.
  _observe() {
    if (this._observer || !this._container || typeof ResizeObserver === 'undefined') return;
    this._observer = new ResizeObserver(this._syncOverflow);
    this._observer.observe(this._container);
  }

  // Which way the row can still travel. Read straight from layout rather than
  // from the filter count: whether five chips overflow depends on the label
  // lengths of the active locale, not on how many there are.
  _syncOverflow() {
    const el = this._container;
    if (!el) return;
    if (!this.hasAttribute('scroll')) {
      el.removeAttribute('data-overflow');
      return;
    }
    const max = el.scrollWidth - el.clientWidth;
    const start = el.scrollLeft > 1;
    const end = el.scrollLeft < max - 1;
    el.setAttribute('data-overflow', start && end ? 'both' : start ? 'start' : end ? 'end' : 'none');
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

    const clearHtml = this.hasAttribute('clearable')
      ? '<button class="tf-filter-chips__clear" type="button" aria-label="Wyczyść filtry">×</button>'
      : '';

    // An identical render is a no-op. Callers on a poll chain hand the same
    // filters back every few seconds (TentaNas rebuilds the disk bar every
    // 5 s because the labels carry live counts), and rewriting innerHTML
    // destroys and recreates every chip: the button under the cursor loses
    // its hover and :active styling mid-press, and a click that lands in that
    // window is delivered to a node already detached from the document.
    // Comparing the markup we are about to write is enough — this element is
    // the only writer of its container.
    const next = html + clearHtml;
    if (this._renderedHtml !== next) {
      this._renderedHtml = next;
      this._container.innerHTML = next;
    }
    // Still re-read the overflow state: the row may have been resized or
    // scrolled since the last render even when the chips themselves did not
    // change, and this only reads layout and writes a data attribute.
    this._syncOverflow();
  }

  _onClick(e) {
    if (e.target.closest('.tf-filter-chips__clear')) {
      this.dispatchEvent(new CustomEvent('clear', { bubbles: true }));
      return;
    }
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
