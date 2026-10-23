// =============================================================================
// Plik: tf-menu.js
// Opis: Komponent <tf-menu> — kontekstowe menu z elementami <tf-menu-item>
//       i <tf-menu-divider>. Shadow DOM (open). Animacja Apple-style:
//       scale 0.9 -> 1 + blur-in 8px -> 0 + stagger itemow co 25ms.
//       Click-outside zamyka. Metody .open()/.close() oraz atrybut "open".
//       Ustawienie `.anchor = element` przenosi panel na position:fixed i
//       ustawia go POD (albo, gdy brak miejsca, NAD) kotwica — kotwica nigdy
//       nie jest zaslonieta. Klik w kotwice nie zamyka menu przed wlasnym
//       przelaczeniem (kotwica jest wyzwalaczem). Bez `.anchor` panel dziala jak dotad.
//       Atrybut `heading` pokazuje nieklikalny naglowek nad pozycjami (o czym jest
//       menu), a atrybut `hint` pozycji dokleja pod etykieta krotkie wyjasnienie
//       (np. dlaczego pozycja jest wylaczona). Strzalki, Home/End i Enter/Spacja
//       obsluguja pozycje z klawiatury, gdy fokus jest w menu.
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
import { nextOverlayLayer } from './tf-window.js';

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

// Node.contains() stops at a shadow boundary, and a trigger often sits in one.
function holdsAcrossShadows(container, node) {
  for (let n = node; n; n = n.parentNode || n.host) {
    if (n === container) return true;
  }
  return false;
}

class TfMenuItem extends HTMLElement {
  static get observedAttributes() {
    return ['icon', 'danger', 'action', 'disabled', 'shortcut', 'label', 'hint'];
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
    const hint = this.getAttribute('hint');
    const textHtml = hint
      ? `<span class="tf-menu-item-text"><span>${escapeHtml(this._label)}</span><small class="tf-menu-item-hint">${escapeHtml(hint)}</small></span>`
      : `<span>${escapeHtml(this._label)}</span>`;
    if (hint) this._btn.setAttribute('title', hint);
    else this._btn.removeAttribute('title');
    this._btn.innerHTML = `${iconHtml}${textHtml}${shortcutHtml}`;
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
    return ['open', 'placement', 'heading'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._box = null;
    this._anchor = null;
    this._staggerTimers = [];
    this._headingEl = null;
    this._onDocClick = this._onDocClick.bind(this);
    this._onSelect = this._onSelect.bind(this);
    this._onKey = this._onKey.bind(this);
    this._onViewportChange = this._onViewportChange.bind(this);
    this._onAnchorKey = this._onAnchorKey.bind(this);
  }

  /** Element the panel must never cover (e.g. the table row it was opened from). */
  set anchor(el) {
    this._anchor?.removeEventListener('keydown', this._onAnchorKey);
    this._anchor = el instanceof Element ? el : null;
    this._anchor?.addEventListener('keydown', this._onAnchorKey);
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
    if (name === 'placement' || name === 'heading') this._update();
  }

  open() { this.setAttribute('open', ''); }
  close() { this.removeAttribute('open'); }
  toggle() { if (this.hasAttribute('open')) this.close(); else this.open(); }

  _build() {
    adoptControlsInto(this._shadow);
    const box = document.createElement('div');
    box.className = 'tf-menu';
    box.setAttribute('role', 'menu');
    const heading = document.createElement('div');
    heading.className = 'tf-menu-heading';
    heading.hidden = true;
    box.appendChild(heading);
    this._headingEl = heading;
    const slot = document.createElement('slot');
    box.appendChild(slot);
    this._shadow.appendChild(box);
    this._box = box;
    box.inert = true;
  }

  _update() {
    const placement = this.getAttribute('placement') || 'bottom-start';
    this._box.setAttribute('data-placement', placement);
    const heading = this.getAttribute('heading') || '';
    this._headingEl.textContent = heading;
    this._headingEl.hidden = !heading;
    const isOpen = this.hasAttribute('open');
    if (isOpen) {
      if (!this._wasOpen) this._box.style.zIndex = String(nextOverlayLayer());
      if (!this._wasOpen) Sfx.play('menu-open');
      this._wasOpen = true;
      this._box.inert = false;
      this._box.classList.add('open');
      this._position();
      this._bindViewport();
      this._applyStagger();
      this.dispatchEvent(new CustomEvent('open', { bubbles: true }));
    } else {
      if (this._wasOpen) Sfx.play('menu-close');
      const wasOpen = this._wasOpen;
      this._wasOpen = false;
      this._box.classList.remove('open');
      // Inert drops focus held inside the panel to <body>; hand it back to the
      // trigger. Focus that already moved elsewhere (outside click) is left alone.
      if (wasOpen && this._focusInside()) this._focusAnchor();
      // The fade-out keeps the panel visible for 0.24 s; inert takes it out of
      // hit-testing and the tab order at once so it cannot catch a click meant for
      // what lies underneath.
      this._box.inert = true;
      this._unbindViewport();
      this._clearStagger();
      this.dispatchEvent(new CustomEvent('close', { bubbles: true }));
    }
  }

  _focusInside() {
    let active = document.activeElement;
    while (active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;
    return !!active && active !== document.body && holdsAcrossShadows(this, active);
  }

  _focusAnchor() {
    const anchor = this._anchor;
    if (!anchor || !anchor.isConnected) return;
    const target = anchor.matches('tf-button') ? anchor.querySelector('button') ?? anchor : anchor;
    target.focus?.();
  }

  // With an anchor the panel leaves flow entirely: an absolutely positioned
  // dropdown inherits the containing block of whatever cell it sits in, which
  // is how it ended up drawn over its own row.
  _position() {
    const box = this._box;
    if (!this._anchor || !this._anchor.isConnected) {
      box.style.cssText = `z-index: ${box.style.zIndex};`;
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
    const room = placeAbove ? above : below;
    // A floor keeps a tiny gap from squeezing the list to nothing, but never
    // above what the viewport itself can hold: the panel scrolls instead.
    const floor = Math.min(120, Math.max(0, vh - 2 * margin));
    const fitted = Math.min(h, Math.max(room, floor));
    if (fitted < h) box.style.maxHeight = `${Math.floor(fitted)}px`;
    box.style.overflowY = fitted < h ? 'auto' : '';
    const wanted = placeAbove ? a.top - gap - fitted : a.bottom + gap;
    const top = Math.min(Math.max(margin, wanted), Math.max(margin, vh - margin - fitted));
    box.setAttribute('data-placement', placeAbove ? 'top-end' : 'bottom-end');

    const left = Math.min(Math.max(margin, a.right - w), vw - w - margin);
    box.style.top = `${Math.round(top)}px`;
    box.style.left = `${Math.round(Math.max(margin, left))}px`;
  }

  // A fixed panel does not travel with its anchor, so a scroll that moves the
  // anchor dismisses it rather than leaving it stranded over unrelated content.
  _bindViewport() {
    if (!this._anchor || this._viewportBound) return;
    this._viewportBound = true;
    window.addEventListener('scroll', this._onViewportChange, true);
    window.addEventListener('resize', this._onViewportChange);
    this._lastAnchorKey = this._anchorKey();
    this._watchAnchor();
  }

  _unbindViewport() {
    if (!this._viewportBound) return;
    this._viewportBound = false;
    cancelAnimationFrame(this._frame);
    this._frame = 0;
    window.removeEventListener('scroll', this._onViewportChange, true);
    window.removeEventListener('resize', this._onViewportChange);
  }

  // A scroll event is not composed, so a scroller inside a shadow root (a tf-window
  // body) never reaches the window listener. Following the anchor's own rect on
  // animation frames needs no knowledge of where it scrolls; it runs only while open.
  _watchAnchor() {
    this._frame = requestAnimationFrame(() => {
      if (!this._viewportBound) return;
      const key = this._anchorKey();
      if (key !== this._lastAnchorKey) {
        this._lastAnchorKey = key;
        if (this._anchor.isConnected && this._anchorVisible(this._anchor)) this._position();
        else { this.close(); return; }
      }
      this._watchAnchor();
    });
  }

  _anchorKey() {
    const r = this._anchor.getBoundingClientRect();
    return `${r.top}|${r.left}|${r.width}|${r.height}`;
  }

  _onViewportChange(event) {
    if (event.type !== 'scroll') { this.close(); return; }
    const source = event.target;
    // The panel scrolling inside itself moves nothing else.
    if (source instanceof Node && (source === this || this.contains(source))) return;
    const anchor = this._anchor;
    const document_ = source === document || source === document.documentElement || source === document.body;
    if (!document_ && !holdsAcrossShadows(source, anchor)) return;
    // Focusing the trigger can scroll its own overflow container by a few pixels
    // to bring it into view. That is not the reader leaving: follow the anchor
    // while it is still on screen and only dismiss once it is not.
    if (!document_ && this._anchorVisible(anchor)) { this._position(); return; }
    this.close();
  }

  _anchorOnScreen(anchor) {
    const r = anchor.getBoundingClientRect();
    return r.bottom > 0 && r.right > 0 && r.top < window.innerHeight && r.left < window.innerWidth;
  }

  // On screen AND not scrolled out of a clipping ancestor such as a window body,
  // where it would still be inside the viewport but no longer visible. The walk
  // follows the composed tree: a slotted trigger (tf-window content) reaches the
  // scroller inside the host's shadow root only through its slot, never through
  // parentNode.
  _anchorVisible(anchor) {
    if (!this._anchorOnScreen(anchor)) return false;
    const r = anchor.getBoundingClientRect();
    for (let n = anchor.assignedSlot || anchor.parentNode || anchor.host; n; n = n.assignedSlot || n.parentNode || n.host) {
      if (!(n instanceof Element)) continue;
      const overflow = getComputedStyle(n);
      if (overflow.overflowX === 'visible' && overflow.overflowY === 'visible') continue;
      const c = n.getBoundingClientRect();
      if (c.width === 0 || c.height === 0) continue;
      if (r.bottom <= c.top || r.top >= c.bottom || r.right <= c.left || r.left >= c.right) return false;
    }
    return true;
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
    // The anchor is the trigger: its own click toggles, so closing here as well
    // would let that click reopen the menu it just dismissed.
    if (this._anchor && path.includes(this._anchor)) return;
    this.close();
  }

  // Down/Up on a closed trigger opens the menu at its first/last enabled item,
  // as a menu button does. Only a button-like anchor qualifies: a table row would
  // open it from the arrows of any field inside it.
  _onAnchorKey(e) {
    if ((e.key !== 'ArrowDown' && e.key !== 'ArrowUp') || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey
      || e.defaultPrevented || this.hasAttribute('open') || !this._anchor.matches('button, tf-button, [role="button"]')) return;
    e.preventDefault();
    // The same event still bubbles to the document listener, which would move
    // focus on once more now that the menu is open.
    this._openedBy = e;
    this.open();
    const rows = this._enabledRows();
    (e.key === 'ArrowDown' ? rows[0] : rows.at(-1))?.focus();
  }

  _enabledRows() {
    return [...this.querySelectorAll(':scope > tf-menu-item > .tf-menu-item')]
      .filter((row) => !row.parentElement.hidden && !row.parentElement.hasAttribute('disabled'));
  }

  _onKey(e) {
    if (e === this._openedBy) return;
    if (!this.hasAttribute('open')) return;
    if (e.key === 'Escape') {
      e.stopPropagation();
      this.close();
      return;
    }
    if (!this.contains(document.activeElement)) return;
    if (e.key === 'Tab') { this.close(); return; }
    // A hidden item cannot take focus and a disabled one cannot act, so neither
    // is a stop: the arrows would otherwise end on the first of them.
    const all = [...this.querySelectorAll(':scope > tf-menu-item > .tf-menu-item')];
    const rows = all.filter((row) => !row.parentElement.hidden && !row.parentElement.hasAttribute('disabled'));
    if (rows.length === 0) return;
    const at = rows.indexOf(document.activeElement);
    const go = (i) => { e.preventDefault(); rows[(i + rows.length) % rows.length].focus(); };
    // Focus sits on a disabled item: the arrows continue from where it stands in
    // the list, to the next or the previous enabled item, instead of jumping to an end.
    if (at < 0) {
      const place = all.indexOf(document.activeElement);
      const order = (row) => all.indexOf(row);
      let target = null;
      if (e.key === 'ArrowDown') target = rows.find((row) => order(row) > place) ?? rows[0];
      else if (e.key === 'ArrowUp') target = rows.findLast((row) => order(row) < place) ?? rows.at(-1);
      else if (e.key === 'Home') target = rows[0];
      else if (e.key === 'End') target = rows.at(-1);
      if (target) { e.preventDefault(); target.focus(); }
      return;
    }
    if (e.key === 'ArrowDown') go(at + 1);
    else if (e.key === 'ArrowUp') go(at - 1);
    else if (e.key === 'Home') go(0);
    else if (e.key === 'End') go(rows.length - 1);
    else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      rows[at].click();
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
