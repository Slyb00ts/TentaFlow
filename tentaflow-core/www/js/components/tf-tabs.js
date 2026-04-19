// =============================================================================
// Plik: tf-tabs.js
// Opis: Komponent <tf-tabs variant="solid|soft|underline"> oraz <tf-tab>.
//       Implementuje animowany wskaznik tla (solid/soft) lub podkreslenia
//       (underline) ktory slidem przesuwa sie miedzy tabami metoda FLIP.
// Przyklad:
//   <tf-tabs variant="solid" value="list">
//     <tf-tab id="list" count="24">Lista</tf-tab>
//     <tf-tab id="alias" count="7">Aliasy</tf-tab>
//   </tf-tabs>
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

class TfTabs extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'value'];
  }

  constructor() {
    super();
    this._root = null;
    this._indicator = null;
    this._resizeObs = null;
    this._onTabClick = this._onTabClick.bind(this);
    this._onResize = this._onResize.bind(this);
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._applyVariant();
    // pierwsze malowanie wskaznika po layoucie
    requestAnimationFrame(() => this._syncIndicator());
    this.addEventListener('tf-tab-click', this._onTabClick);
    if ('ResizeObserver' in window) {
      this._resizeObs = new ResizeObserver(this._onResize);
      this._resizeObs.observe(this);
    } else {
      window.addEventListener('resize', this._onResize);
    }
  }

  disconnectedCallback() {
    this.removeEventListener('tf-tab-click', this._onTabClick);
    if (this._resizeObs) this._resizeObs.disconnect();
    else window.removeEventListener('resize', this._onResize);
  }

  attributeChangedCallback(name) {
    if (!this._root) return;
    if (name === 'variant') this._applyVariant();
    if (name === 'value') this._syncActive();
  }

  get value() { return this.getAttribute('value'); }
  set value(v) { this.setAttribute('value', v); }

  _build() {
    // dzieci <tf-tab> pozostaja w light DOM tej kontrolki, ale opakowane
    // w box aby zachowac flexbox. Wrap wstawiamy do wewnatrz i przenosimy
    // <tf-tab> do niego.
    const tabs = Array.from(this.children).filter((c) => c.tagName === 'TF-TAB');
    const root = document.createElement('div');
    root.dataset.indicator = '';
    this.appendChild(root);
    tabs.forEach((t) => root.appendChild(t));

    // wskaznik — dla underline inny element, ale tez pozycjonowany absolutnie
    const indicator = document.createElement('span');
    indicator.className = 'tf-tab-indicator';
    root.appendChild(indicator);

    this._root = root;
    this._indicator = indicator;
  }

  _applyVariant() {
    const variant = (this.getAttribute('variant') || 'solid').toLowerCase();
    this._root.className = '';
    if (variant === 'underline') {
      this._root.classList.add('tf-tabs-underline');
      this._indicator.className = 'tf-tab-underline-bar';
    } else if (variant === 'soft') {
      this._root.classList.add('tf-tabs-soft');
      this._indicator.className = 'tf-tab-indicator';
    } else {
      this._root.classList.add('tf-tabs');
      this._indicator.className = 'tf-tab-indicator';
    }
    this._syncActive();
  }

  _getTabs() {
    return Array.from(this._root.querySelectorAll(':scope > tf-tab'));
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
    requestAnimationFrame(() => this._syncIndicator());
  }

  _syncIndicator() {
    const active = this._root.querySelector('tf-tab > .tf-tab.active');
    if (!active) {
      this._indicator.removeAttribute('data-ready');
      return;
    }
    const hostTab = active.parentElement;
    const rootRect = this._root.getBoundingClientRect();
    const tabRect = hostTab.getBoundingClientRect();
    const variant = (this.getAttribute('variant') || 'solid').toLowerCase();
    if (variant === 'underline') {
      // podkreslenie z marginesem 10px od krawedzi taba (zgodnie z mockupem)
      const padding = 10;
      const x = tabRect.left - rootRect.left + padding;
      const w = tabRect.width - padding * 2;
      this._indicator.style.transform = `translateX(${x}px)`;
      this._indicator.style.width = `${Math.max(0, w)}px`;
    } else {
      const x = tabRect.left - rootRect.left;
      const w = tabRect.width;
      this._indicator.style.transform = `translateX(${x}px)`;
      this._indicator.style.width = `${w}px`;
    }
    this._indicator.setAttribute('data-ready', '');
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
  }
}

customElements.define('tf-tabs', TfTabs);
export { TfTabs, TfTab };
