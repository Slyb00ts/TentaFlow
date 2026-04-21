// =============================================================================
// File: tf-tabs.js — <tf-tabs> and <tf-tab> custom elements with horizontal
// overflow handling (fade overlays, chevron scroll buttons, touch/wheel swipe,
// auto-scroll on selection) and a FLIP-based active indicator.
// =============================================================================

class TfTab extends HTMLElement {
  static get observedAttributes() {
    return ['count', 'icon', 'disabled'];
  }

  constructor() {
    super();
    this._btn = null;
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    if (!this._btn) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._btn) this._update();
  }

  _build() {
    const label = this.innerHTML;
    this.innerHTML = '';
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tf-tab';
    btn.dataset.tab = '';
    btn.dataset.tabId = this.id || '';
    btn.addEventListener('click', this._onClick);
    btn._label = label;
    this.appendChild(btn);
    this._btn = btn;
  }

  _update() {
    const icon = this.getAttribute('icon');
    const count = this.getAttribute('count');
    const iconHtml = icon
      ? `<svg width="12" height="12" aria-hidden="true"><use href="#i-${icon}"/></svg>`
      : '';
    const countHtml = count
      ? `<span class="tf-tab-count">${count}</span>`
      : '';
    this._btn.innerHTML = `${iconHtml}<span class="tf-tab-label">${this._btn._label}</span>${countHtml}`;
    this._btn.dataset.tabId = this.id || '';
    if (this.hasAttribute('disabled')) this._btn.setAttribute('disabled', '');
    else this._btn.removeAttribute('disabled');
  }

  setActive(on) {
    this._btn.classList.toggle('active', !!on);
  }

  _onClick() {
    if (this.hasAttribute('disabled')) return;
    this.dispatchEvent(new CustomEvent('tf-tab-click', {
      bubbles: true,
      detail: { id: this.id },
    }));
  }
}
customElements.define('tf-tab', TfTab);

const CHEV_LEFT_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><polyline points="15 6 9 12 15 18"/></svg>';
const CHEV_RIGHT_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><polyline points="9 6 15 12 9 18"/></svg>';

class TfTabs extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'value'];
  }

  constructor() {
    super();
    this._viewport = null;
    this._scroller = null;   // inner strip (former _root), carries variant class
    this._indicator = null;
    this._fadeLeft = null;
    this._fadeRight = null;
    this._chevLeft = null;
    this._chevRight = null;
    this._resizeObs = null;
    this._onTabClick = this._onTabClick.bind(this);
    this._onResize = this._onResize.bind(this);
    this._onScroll = this._onScroll.bind(this);
    this._onWheel = this._onWheel.bind(this);
    this._onChevLeft = this._onChevLeft.bind(this);
    this._onChevRight = this._onChevRight.bind(this);
  }

  connectedCallback() {
    if (!this._scroller) this._build();
    this._applyVariant();
    requestAnimationFrame(() => {
      this._syncIndicator();
      this._updateFades();
    });
    this.addEventListener('tf-tab-click', this._onTabClick);
    this._scroller.addEventListener('scroll', this._onScroll, { passive: true });
    this._scroller.addEventListener('wheel', this._onWheel, { passive: false });
    this._chevLeft.addEventListener('click', this._onChevLeft);
    this._chevRight.addEventListener('click', this._onChevRight);
    if ('ResizeObserver' in window) {
      this._resizeObs = new ResizeObserver(this._onResize);
      this._resizeObs.observe(this);
      this._resizeObs.observe(this._scroller);
    } else {
      window.addEventListener('resize', this._onResize);
    }
  }

  disconnectedCallback() {
    this.removeEventListener('tf-tab-click', this._onTabClick);
    if (this._scroller) {
      this._scroller.removeEventListener('scroll', this._onScroll);
      this._scroller.removeEventListener('wheel', this._onWheel);
    }
    if (this._chevLeft) this._chevLeft.removeEventListener('click', this._onChevLeft);
    if (this._chevRight) this._chevRight.removeEventListener('click', this._onChevRight);
    if (this._resizeObs) this._resizeObs.disconnect();
    else window.removeEventListener('resize', this._onResize);
  }

  attributeChangedCallback(name) {
    if (!this._scroller) return;
    if (name === 'variant') this._applyVariant();
    if (name === 'value') this._syncActive();
  }

  get value() { return this.getAttribute('value'); }
  set value(v) { this.setAttribute('value', v); }

  _build() {
    // Collect existing <tf-tab> children then wrap them in viewport + scroller.
    const tabs = Array.from(this.children).filter((c) => c.tagName === 'TF-TAB');
    // Clear host of any stray non-tab content we are about to rebuild.
    tabs.forEach((t) => t.remove());

    const viewport = document.createElement('div');
    viewport.className = 'tf-tabs-viewport';

    const scroller = document.createElement('div');
    scroller.dataset.indicator = '';
    tabs.forEach((t) => scroller.appendChild(t));

    // FLIP indicator lives inside the scroller so it shares its scroll offset.
    const indicator = document.createElement('span');
    indicator.className = 'tf-tab-indicator';
    scroller.appendChild(indicator);

    viewport.appendChild(scroller);
    this.appendChild(viewport);

    const fadeLeft = document.createElement('div');
    fadeLeft.className = 'tf-tabs-fade tf-tabs-fade-left';
    fadeLeft.setAttribute('aria-hidden', 'true');
    const fadeRight = document.createElement('div');
    fadeRight.className = 'tf-tabs-fade tf-tabs-fade-right';
    fadeRight.setAttribute('aria-hidden', 'true');

    const chevLeft = document.createElement('button');
    chevLeft.type = 'button';
    chevLeft.className = 'tf-tabs-chev tf-tabs-chev-left';
    chevLeft.setAttribute('aria-label', 'Scroll left');
    chevLeft.setAttribute('tabindex', '-1');
    chevLeft.innerHTML = CHEV_LEFT_SVG;

    const chevRight = document.createElement('button');
    chevRight.type = 'button';
    chevRight.className = 'tf-tabs-chev tf-tabs-chev-right';
    chevRight.setAttribute('aria-label', 'Scroll right');
    chevRight.setAttribute('tabindex', '-1');
    chevRight.innerHTML = CHEV_RIGHT_SVG;

    this.appendChild(fadeLeft);
    this.appendChild(fadeRight);
    this.appendChild(chevLeft);
    this.appendChild(chevRight);

    this._viewport = viewport;
    this._scroller = scroller;
    this._indicator = indicator;
    this._fadeLeft = fadeLeft;
    this._fadeRight = fadeRight;
    this._chevLeft = chevLeft;
    this._chevRight = chevRight;
  }

  _applyVariant() {
    const variant = (this.getAttribute('variant') || 'solid').toLowerCase();
    this._scroller.className = '';
    this._viewport.classList.remove('tf-tabs-variant-underline', 'tf-tabs-variant-soft', 'tf-tabs-variant-solid');
    if (variant === 'underline') {
      this._scroller.classList.add('tf-tabs-underline');
      this._viewport.classList.add('tf-tabs-variant-underline');
      this._indicator.className = 'tf-tab-underline-bar';
    } else if (variant === 'soft') {
      this._scroller.classList.add('tf-tabs-soft');
      this._viewport.classList.add('tf-tabs-variant-soft');
      this._indicator.className = 'tf-tab-indicator';
    } else {
      this._scroller.classList.add('tf-tabs');
      this._viewport.classList.add('tf-tabs-variant-solid');
      this._indicator.className = 'tf-tab-indicator';
    }
    this._scroller.dataset.indicator = '';
    this._syncActive();
  }

  _getTabs() {
    return Array.from(this._scroller.querySelectorAll(':scope > tf-tab'));
  }

  _syncActive() {
    const value = this.getAttribute('value');
    const tabs = this._getTabs();
    if (!tabs.length) return;
    let activeTab = tabs.find((t) => t.id === value);
    if (!activeTab) {
      activeTab = tabs[0];
      if (activeTab && !this.hasAttribute('value')) {
        this.setAttribute('value', activeTab.id);
      }
    }
    tabs.forEach((t) => t.setActive(t === activeTab));
    requestAnimationFrame(() => {
      this._syncIndicator();
      this._scrollActiveIntoView(activeTab);
    });
  }

  _syncIndicator() {
    const active = this._scroller.querySelector('tf-tab > .tf-tab.active');
    if (!active) {
      this._indicator.removeAttribute('data-ready');
      return;
    }
    const hostTab = active.parentElement;
    // Measure within the scroller's content box so the indicator tracks the
    // tab correctly regardless of scrollLeft.
    const scrollerRect = this._scroller.getBoundingClientRect();
    const tabRect = hostTab.getBoundingClientRect();
    const offsetX = (tabRect.left - scrollerRect.left) + this._scroller.scrollLeft;
    const variant = (this.getAttribute('variant') || 'solid').toLowerCase();
    if (variant === 'underline') {
      const padding = 10;
      this._indicator.style.transform = `translateX(${offsetX + padding}px)`;
      this._indicator.style.width = `${Math.max(0, tabRect.width - padding * 2)}px`;
    } else {
      this._indicator.style.transform = `translateX(${offsetX}px)`;
      this._indicator.style.width = `${tabRect.width}px`;
    }
    this._indicator.setAttribute('data-ready', '');
  }

  _scrollActiveIntoView(tab) {
    if (!tab) return;
    const s = this._scroller;
    const tabEl = tab.querySelector('.tf-tab') || tab;
    const tabRect = tabEl.getBoundingClientRect();
    const sRect = s.getBoundingClientRect();
    const margin = 24;
    if (tabRect.left < sRect.left + margin) {
      s.scrollBy({ left: tabRect.left - sRect.left - margin, behavior: 'smooth' });
    } else if (tabRect.right > sRect.right - margin) {
      s.scrollBy({ left: tabRect.right - sRect.right + margin, behavior: 'smooth' });
    }
  }

  _updateFades() {
    const s = this._scroller;
    if (!s) return;
    const hasLeft = s.scrollLeft > 4;
    const hasRight = s.scrollLeft + s.clientWidth < s.scrollWidth - 4;
    this._fadeLeft.classList.toggle('visible', hasLeft);
    this._fadeRight.classList.toggle('visible', hasRight);
    this._chevLeft.classList.toggle('visible', hasLeft);
    this._chevRight.classList.toggle('visible', hasRight);
    this._chevLeft.setAttribute('tabindex', hasLeft ? '0' : '-1');
    this._chevRight.setAttribute('tabindex', hasRight ? '0' : '-1');
  }

  _onTabClick(e) {
    const id = e.detail?.id;
    if (!id || id === this.getAttribute('value')) return;
    this.setAttribute('value', id);
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: id },
    }));
  }

  _onResize() {
    this._syncIndicator();
    this._updateFades();
  }

  _onScroll() {
    this._syncIndicator();
    this._updateFades();
  }

  _onWheel(e) {
    // Translate dominant-vertical wheel into horizontal scroll so mouse users
    // on non-touch devices can swipe the tab strip with a regular wheel.
    if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) {
      e.preventDefault();
      this._scroller.scrollBy({ left: e.deltaY, behavior: 'auto' });
    }
  }

  _onChevLeft() {
    this._scroller.scrollBy({ left: -this._scroller.clientWidth * 0.6, behavior: 'smooth' });
  }

  _onChevRight() {
    this._scroller.scrollBy({ left: this._scroller.clientWidth * 0.6, behavior: 'smooth' });
  }
}

customElements.define('tf-tabs', TfTabs);
export { TfTabs, TfTab };
