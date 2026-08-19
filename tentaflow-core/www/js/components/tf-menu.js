// =============================================================================
// Plik: tf-menu.js
// Opis: Komponent <tf-menu> — kontekstowe menu z elementami <tf-menu-item>
//       i <tf-menu-divider>. Shadow DOM (open). Animacja Apple-style:
//       scale 0.9 -> 1 + blur-in 8px -> 0 + stagger itemow co 25ms.
//       Click-outside zamyka. Metody .open()/.close() oraz atrybut "open".
//       Ustawienie `.anchor = element` przenosi panel na position:fixed i
//       ustawia go POD (albo, gdy brak miejsca, NAD) kotwica — kotwica nigdy
//       nie jest zaslonieta. Bez `.anchor` panel dziala jak dotad.
//       Atrybut `compact` zdejmuje 180px min-width i zweza wiersze — dla menu
//       kotwiczonego w tabeli, gdzie panel wisi nad rekordami. Tylko dla
//       wskaznika: zwezony wiersz jest ponizej celu dotykowego 44px.
// Przyklad:
//   <tf-menu placement="bottom-start">
//     <tf-menu-item action="edit" icon="edit">Edytuj</tf-menu-item>
//     <tf-menu-divider></tf-menu-divider>
//     <tf-menu-item action="delete" icon="trash" danger>Usun</tf-menu-item>
//   </tf-menu>
// =============================================================================

import { adoptControlsInto } from './shared-styles.js';
import { Sfx } from '/js/lib/sfx.js';

function escapeHtml(s) {
  if (s === null || s === undefined) return '';
  return String(s)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function safeIconName(value) {
  const text = String(value || '').trim();
  return /^[a-z0-9_-]{1,64}$/i.test(text) ? text : '';
}

class TfMenuItem extends HTMLElement {
  static get observedAttributes() {
    return ['icon', 'danger', 'action', 'disabled', 'shortcut', 'label'];
  }

  constructor() {
    super();
    this._btn = null;
    this._label = '';
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
    // A `label` attribute (set programmatically, e.g. by the SDK table row-actions
    // renderer) is authoritative and timing-safe; textContent is the fallback for
    // plain HTML usage like `<tf-menu-item>Edytuj</tf-menu-item>`.
    this._label = this.getAttribute('label') ?? this.textContent;
    this.innerHTML = '';
    // w Shadow DOM rodzica — tu budujemy w light DOM itemu,
    // rodzic przeniesie go do shadow slot gdy otwiera
    const el = document.createElement('div');
    el.className = 'tf-menu-item';
    el.setAttribute('role', 'menuitem');
    el.tabIndex = 0;
    el.addEventListener('click', this._onClick);
    this.appendChild(el);
    this._btn = el;
  }

  _update() {
    // Keep the label in sync when the attribute is updated after build.
    const labelAttr = this.getAttribute('label');
    if (labelAttr != null) this._label = labelAttr;
    const icon = safeIconName(this.getAttribute('icon'));
    const shortcut = this.getAttribute('shortcut');
    const danger = this.hasAttribute('danger');
    const disabled = this.hasAttribute('disabled');
    this._btn.classList.toggle('danger', danger);
    this._btn.classList.toggle('disabled', disabled);
    this._btn.setAttribute('aria-disabled', disabled ? 'true' : 'false');
    const iconHtml = icon
      ? `<svg width="14" height="14" aria-hidden="true"><use href="#i-${icon}"/></svg>`
      : '';
    const shortcutHtml = shortcut
      ? `<span class="tf-menu-item-shortcut">${escapeHtml(shortcut)}</span>`
      : '';
    this._btn.innerHTML = `${iconHtml}<span>${escapeHtml(this._label)}</span>${shortcutHtml}`;
  }

  _onClick(e) {
    if (this.hasAttribute('disabled')) {
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    const action = this.getAttribute('action') || '';
    this.dispatchEvent(new CustomEvent('tf-menu-select', {
      bubbles: true,
      composed: true,
      detail: { action, item: this },
    }));
  }
}
customElements.define('tf-menu-item', TfMenuItem);

class TfMenuDivider extends HTMLElement {
  connectedCallback() {
    if (!this.firstElementChild) {
      const el = document.createElement('div');
      el.className = 'tf-menu-divider';
      this.appendChild(el);
    }
  }
}
customElements.define('tf-menu-divider', TfMenuDivider);

class TfMenu extends HTMLElement {
  static get observedAttributes() {
    return ['open', 'placement'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._box = null;
    this._anchor = null;
    this._staggerTimers = [];
    this._onDocClick = this._onDocClick.bind(this);
    this._onSelect = this._onSelect.bind(this);
    this._onKey = this._onKey.bind(this);
    this._onViewportChange = this._onViewportChange.bind(this);
  }

  /** Element the panel must never cover (e.g. the table row it was opened from). */
  set anchor(el) {
    this._anchor = el instanceof Element ? el : null;
    if (this.hasAttribute('open')) this._position();
  }

  get anchor() { return this._anchor; }

  connectedCallback() {
    if (!this._box) this._build();
    this._update();
    document.addEventListener('pointerdown', this._onDocClick, true);
    document.addEventListener('keydown', this._onKey);
    this.addEventListener('tf-menu-select', this._onSelect);
  }

  disconnectedCallback() {
    document.removeEventListener('pointerdown', this._onDocClick, true);
    document.removeEventListener('keydown', this._onKey);
    this.removeEventListener('tf-menu-select', this._onSelect);
    this._unbindViewport();
    this._clearStagger();
  }

  attributeChangedCallback(name) {
    if (!this._box) return;
    if (name === 'open') this._update();
    if (name === 'placement') this._update();
  }

  open() { this.setAttribute('open', ''); }
  close() { this.removeAttribute('open'); }
  toggle() { if (this.hasAttribute('open')) this.close(); else this.open(); }

  _build() {
    adoptControlsInto(this._shadow);
    const box = document.createElement('div');
    box.className = 'tf-menu';
    box.setAttribute('role', 'menu');
    const slot = document.createElement('slot');
    box.appendChild(slot);
    this._shadow.appendChild(box);
    this._box = box;
  }

  _update() {
    const placement = this.getAttribute('placement') || 'bottom-start';
    this._box.setAttribute('data-placement', placement);
    const isOpen = this.hasAttribute('open');
    if (isOpen) {
      if (!this._wasOpen) Sfx.play('menu-open');
      this._wasOpen = true;
      this._box.classList.add('open');
      this._position();
      this._bindViewport();
      this._applyStagger();
      this.dispatchEvent(new CustomEvent('open', { bubbles: true }));
    } else {
      if (this._wasOpen) Sfx.play('menu-close');
      this._wasOpen = false;
      this._box.classList.remove('open');
      this._unbindViewport();
      this._clearStagger();
      this.dispatchEvent(new CustomEvent('close', { bubbles: true }));
    }
  }

  // With an anchor the panel leaves flow entirely: an absolutely positioned
  // dropdown inherits the containing block of whatever cell it sits in, which
  // is how it ended up drawn over its own row.
  _position() {
    const box = this._box;
    if (!this._anchor || !this._anchor.isConnected) {
      box.style.cssText = '';
      return;
    }
    const a = this._anchor.getBoundingClientRect();
    box.style.position = 'fixed';
    box.style.top = '0px';
    box.style.left = '0px';
    box.style.right = 'auto';
    box.style.bottom = 'auto';
    box.style.maxHeight = '';
    // Measure with the panel already laid out at its final width.
    const w = box.offsetWidth;
    const h = box.offsetHeight;
    const gap = 6;
    const margin = 8;
    const vw = window.innerWidth;
    const vh = window.innerHeight;

    const below = vh - a.bottom - gap - margin;
    const above = a.top - gap - margin;
    // Below the anchor by default; flipping up only when it does not fit and
    // there is genuinely more room the other way keeps the reading order.
    const placeAbove = h > below && above > below;
    const top = placeAbove ? Math.max(margin, a.top - gap - h) : a.bottom + gap;
    box.setAttribute('data-placement', placeAbove ? 'top-end' : 'bottom-end');
    const room = placeAbove ? above : below;
    if (h > room) box.style.maxHeight = `${Math.max(120, room)}px`;
    box.style.overflowY = h > room ? 'auto' : '';

    const left = Math.min(Math.max(margin, a.right - w), vw - w - margin);
    box.style.top = `${Math.round(top)}px`;
    box.style.left = `${Math.round(Math.max(margin, left))}px`;
  }

  // A fixed panel does not travel with its anchor, so a scroll or resize
  // dismisses it rather than leaving it stranded over unrelated content.
  _bindViewport() {
    if (!this._anchor || this._viewportBound) return;
    this._viewportBound = true;
    window.addEventListener('scroll', this._onViewportChange, true);
    window.addEventListener('resize', this._onViewportChange);
  }

  _unbindViewport() {
    if (!this._viewportBound) return;
    this._viewportBound = false;
    window.removeEventListener('scroll', this._onViewportChange, true);
    window.removeEventListener('resize', this._onViewportChange);
  }

  _onViewportChange() {
    this.close();
  }

  _applyStagger() {
    const items = Array.from(this.querySelectorAll(':scope > tf-menu-item'));
    this._clearStagger();
    items.forEach((it, i) => {
      const delay = i * 25;
      // delay jest ustawiany inline; CSS rozni sie dla open/close
      it.style.transitionDelay = `${delay}ms`;
    });
  }

  _clearStagger() {
    this._staggerTimers.forEach((t) => clearTimeout(t));
    this._staggerTimers = [];
    const items = Array.from(this.querySelectorAll(':scope > tf-menu-item'));
    items.forEach((it) => { it.style.transitionDelay = '0ms'; });
  }

  _onDocClick(e) {
    if (!this.hasAttribute('open')) return;
    const path = e.composedPath();
    if (path.includes(this)) return;
    this.close();
  }

  _onKey(e) {
    if (!this.hasAttribute('open')) return;
    if (e.key === 'Escape') {
      e.stopPropagation();
      this.close();
    }
  }

  _onSelect(e) {
    this.close();
    this.dispatchEvent(new CustomEvent('action', {
      bubbles: true,
      detail: { action: e.detail.action },
    }));
  }
}

customElements.define('tf-menu', TfMenu);
export { TfMenu, TfMenuItem, TfMenuDivider };
