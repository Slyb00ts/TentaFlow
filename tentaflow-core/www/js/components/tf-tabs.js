// =============================================================================
// File: tf-tabs.js — <tf-tabs> and <tf-tab> custom elements with horizontal
// overflow handling (fade overlays, chevron scroll buttons, touch/wheel swipe,
// auto-scroll on selection) and a FLIP-based active indicator.
//
// <tf-tabs> attributes: variant (solid|soft|underline|bar), value (an empty
//   value selects NO tab; a missing one falls back to the first),
//   layout (inline|stacked — stacked puts the icon ABOVE the label),
//   indicator (top|bottom — which edge the moving 2px rule rides; bar only),
//   safe-area (adds the iOS home-indicator inset + a 46px touch target, for a
//   bottom navigation bar).
//
// <tf-tab> attributes: label (overrides the light-DOM text), icon (sprite id),
//   count (trailing pill) + count-tone (hot), disabled, dirty (unsaved-content
//   dot after the label), dot + tone (leading status dot), marker + tone
//   (leading status LETTER, e.g. A/M/D/!), sub (second line under the label),
//   mono (monospace label), closable (trailing × emitting "tab-close"),
//   pinned (sticks to the strip's left edge while the rest scrolls),
//   nudge (amber "this is waiting for you" state), panel (id for aria-controls).
//   The leading slot holds exactly one marker, resolved dot > marker > icon.
//
// Events: "change" on <tf-tabs> (detail {value}), "tab-close" on <tf-tab>
//   (bubbles, cancelable; detail {id}) — closing is the host's decision, the
//   component never removes the tab itself.
// =============================================================================

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function safeIconName(value) {
  const text = String(value || '').trim();
  return /^[a-z0-9_-]{1,64}$/i.test(text) ? text : '';
}

// Tones shared by the leading marker and the count pill.
const TAB_TONES = new Set(['ok', 'warn', 'err', 'info', 'accent', 'muted', 'hot']);

function toneClass(prefix, raw) {
  const tone = String(raw || '').toLowerCase();
  return TAB_TONES.has(tone) ? ` ${prefix}--${tone}` : '';
}

class TfTab extends HTMLElement {
  static get observedAttributes() {
    return ['count', 'icon', 'disabled', 'label', 'dirty', 'dot', 'marker',
      'tone', 'sub', 'mono', 'count-tone', 'closable', 'panel'];
  }

  constructor() {
    super();
    this._btn = null;
    this._closeBtn = null;
    this._onClick = this._onClick.bind(this);
    this._onCloseClick = this._onCloseClick.bind(this);
  }

  connectedCallback() {
    if (!this._btn) this._build();
    this._update();
    // A tab appended to an already-built <tf-tabs> lands on the host, outside
    // the tablist. Telling the strip here keeps adoption synchronous — no
    // observer, no microtask, so the very next read sees a complete tab set.
    const host = this.parentElement;
    if (host && host.tagName === 'TF-TABS' && typeof host._adoptLateTab === 'function') {
      host._adoptLateTab();
    }
  }

  attributeChangedCallback() {
    if (this._btn) this._update();
  }

  _build() {
    const label = this.textContent;
    this.innerHTML = '';
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tf-tab';
    btn.dataset.tab = '';
    btn.dataset.tabId = this.id || '';
    btn.setAttribute('role', 'tab');
    btn.setAttribute('aria-selected', 'false');
    btn.addEventListener('click', this._onClick);
    btn._label = label;
    this.appendChild(btn);
    this._btn = btn;
  }

  _update() {
    const icon = safeIconName(this.getAttribute('icon'));
    const count = this.getAttribute('count');
    // The `label` attribute (reactive via setAttribute) overrides the text
    // captured from the light DOM at build time — mirrors tf-button/tf-chip.
    const label = this.hasAttribute('label')
      ? this.getAttribute('label')
      : this._btn._label;

    // Leading slot: exactly one of dot / letter / icon, in that priority. A file
    // tab shows its status letter where a session tab shows a state dot, and
    // both sit where a plain tab shows its icon — one slot, three fillings.
    const tone = this.getAttribute('tone');
    let leadHtml = '';
    if (this.hasAttribute('dot')) {
      leadHtml = `<span class="tf-tab-dot${toneClass('tf-tab-dot', tone)}" aria-hidden="true"></span>`;
    } else if (this.getAttribute('marker')) {
      leadHtml = `<span class="tf-tab-marker${toneClass('tf-tab-marker', tone)}" aria-hidden="true">`
        + `${escapeHtml(this.getAttribute('marker'))}</span>`;
    } else if (icon) {
      leadHtml = `<svg width="12" height="12" aria-hidden="true"><use href="#i-${icon}"/></svg>`;
    }

    const sub = this.getAttribute('sub');
    const labelCls = `tf-tab-label${this.hasAttribute('mono') ? ' tf-tab-label--mono' : ''}`;
    const labelHtml = sub
      ? `<span class="tf-tab-text"><span class="${labelCls}">${escapeHtml(label)}</span>`
        + `<span class="tf-tab-sub">${escapeHtml(sub)}</span></span>`
      : `<span class="${labelCls}">${escapeHtml(label)}</span>`;

    const countHtml = count
      ? `<span class="tf-tab-count${toneClass('tf-tab-count', this.getAttribute('count-tone'))}">`
        + `${escapeHtml(count)}</span>`
      : '';
    // Unsaved-content dot; sits between the label and the count so the label
    // keeps its position whether or not the tab is dirty.
    const dirty = this.hasAttribute('dirty');
    const dirtyHtml = dirty
      ? '<span class="tf-tab-dirty" aria-hidden="true"></span>'
      : '';
    this._btn.innerHTML = `${leadHtml}${labelHtml}${dirtyHtml}${countHtml}`;
    this._btn.classList.toggle('is-dirty', dirty);
    this._btn.dataset.tabId = this.id || '';
    const panel = this.getAttribute('panel');
    if (panel) this._btn.setAttribute('aria-controls', panel);
    else this._btn.removeAttribute('aria-controls');
    if (this.hasAttribute('disabled')) this._btn.setAttribute('disabled', '');
    else this._btn.removeAttribute('disabled');
    this._syncClose();
  }

  // The × is a SIBLING of the tab button, never a child: a button inside a
  // button is invalid and unreachable for assistive tech. CSS parks it over the
  // tab's right padding so it still reads as part of the tab.
  _syncClose() {
    const wanted = this.hasAttribute('closable');
    if (wanted && !this._closeBtn) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'tf-tab-close';
      btn.tabIndex = -1;
      btn.setAttribute('aria-label', this.getAttribute('close-label') || 'Close');
      btn.textContent = '×';
      btn.addEventListener('click', this._onCloseClick);
      this.appendChild(btn);
      this._closeBtn = btn;
    } else if (!wanted && this._closeBtn) {
      this._closeBtn.remove();
      this._closeBtn = null;
    }
    if (this._closeBtn) {
      this._closeBtn.disabled = this.hasAttribute('disabled');
    }
  }

  setActive(on) {
    this._btn.classList.toggle('active', !!on);
    this._btn.setAttribute('aria-selected', on ? 'true' : 'false');
  }

  _onClick() {
    if (this.hasAttribute('disabled')) return;
    this.dispatchEvent(new CustomEvent('tf-tab-click', {
      bubbles: true,
      detail: { id: this.id },
    }));
  }

  _onCloseClick(e) {
    // Never let the close reach the strip as a selection.
    e.stopPropagation();
    if (this.hasAttribute('disabled')) return;
    this.dispatchEvent(new CustomEvent('tab-close', {
      bubbles: true,
      cancelable: true,
      detail: { id: this.id },
    }));
  }
}
customElements.define('tf-tab', TfTab);

const CHEV_LEFT_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><polyline points="15 6 9 12 15 18"/></svg>';
const CHEV_RIGHT_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><polyline points="9 6 15 12 9 18"/></svg>';

class TfTabs extends HTMLElement {
  static get observedAttributes() {
    return ['variant', 'value', 'layout', 'indicator'];
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
    this._onKeyDown = this._onKeyDown.bind(this);
    this._lastActiveId = null;
    this._adopting = false;
  }

  connectedCallback() {
    if (!this._scroller) this._build();
    this._applyVariant();
    requestAnimationFrame(() => {
      this._syncIndicator();
      this._updateFades();
    });
    this.addEventListener('tf-tab-click', this._onTabClick);
    this._scroller.addEventListener('keydown', this._onKeyDown);
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
    this._adoptStrayTabs();
  }

  disconnectedCallback() {
    this.removeEventListener('tf-tab-click', this._onTabClick);
    if (this._scroller) {
      this._scroller.removeEventListener('keydown', this._onKeyDown);
      this._scroller.removeEventListener('scroll', this._onScroll);
      this._scroller.removeEventListener('wheel', this._onWheel);
    }
    if (this._chevLeft) this._chevLeft.removeEventListener('click', this._onChevLeft);
    if (this._chevRight) this._chevRight.removeEventListener('click', this._onChevRight);
    if (this._resizeObs) this._resizeObs.disconnect();
    else window.removeEventListener('resize', this._onResize);
  }

  // Called by a <tf-tab> that connected straight onto the host.
  _adoptLateTab() {
    if (!this._scroller) return;   // not built yet — _build() will collect it
    if (this._adoptStrayTabs()) {
      this._syncActive();
      this._updateFades();
    }
  }

  // _build() only collects the tabs present at connect time. A tab appended
  // later lands on the HOST, as a sibling of the viewport — it renders (the host
  // is a flex container) but sits outside _getTabs(), so it gets no active
  // state, no indicator and no keyboard reach. Adoption is driven by the child's
  // connectedCallback and, as a backstop, by _getTabs() itself — both
  // synchronous, so no observer and no deferred state.
  _adoptStrayTabs() {
    if (this._adopting || !this._scroller) return false;
    const strays = [];
    for (const child of this.children) if (child.tagName === 'TF-TAB') strays.push(child);
    if (!strays.length) return false;
    this._adopting = true;
    try {
      // The indicator is the scroller's last child and must stay that way.
      for (const tab of strays) this._scroller.insertBefore(tab, this._indicator);
    } finally {
      this._adopting = false;
    }
    return true;
  }

  attributeChangedCallback(name) {
    if (!this._scroller) return;
    if (name === 'variant') this._applyVariant();
    if (name === 'value') this._syncActive();
    // layout drives the indicator inset (stacked bars underline only the middle),
    // indicator drives which edge it rides — both need a re-measure.
    if (name === 'layout' || name === 'indicator') this._syncIndicator();
  }

  get value() { return this.getAttribute('value'); }
  set value(v) {
    this.setAttribute('value', v);
    // Sync directly as well: attributeChangedCallback is not dispatched in
    // every DOM environment (e.g. happy-dom); _syncActive is idempotent.
    if (this._scroller) this._syncActive();
  }

  _build() {
    // Collect existing <tf-tab> children then wrap them in viewport + scroller.
    const tabs = Array.from(this.children).filter((c) => c.tagName === 'TF-TAB');
    // Clear host of any stray non-tab content we are about to rebuild.
    tabs.forEach((t) => t.remove());

    const viewport = document.createElement('div');
    viewport.className = 'tf-tabs-viewport';

    const scroller = document.createElement('div');
    scroller.dataset.indicator = '';
    scroller.setAttribute('role', 'tablist');
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
    this._viewport.classList.remove('tf-tabs-variant-underline', 'tf-tabs-variant-soft',
      'tf-tabs-variant-solid', 'tf-tabs-variant-bar');
    if (variant === 'underline') {
      this._scroller.classList.add('tf-tabs-underline');
      this._viewport.classList.add('tf-tabs-variant-underline');
      this._indicator.className = 'tf-tab-underline-bar';
    } else if (variant === 'bar') {
      // Flat navigation strip: no card chrome, full-height cells, a moving 2px
      // gradient rule. One shape for the scene strip, the dock and the phone
      // bottom nav — they differ only by `layout`, `indicator` and `safe-area`.
      this._scroller.classList.add('tf-tabs-navbar');
      this._viewport.classList.add('tf-tabs-variant-bar');
      this._indicator.className = 'tf-tab-bar-line';
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
    this._adoptStrayTabs();
    return Array.from(this._scroller.querySelectorAll(':scope > tf-tab'));
  }

  _syncActive() {
    const value = this.getAttribute('value');
    const tabs = this._getTabs();
    if (!tabs.length) return;
    // An explicitly EMPTY `value` means "no tab is active": a strip whose tabs
    // address another object (the TentaNas fleet view, where the six tabs
    // belong to a node that is not selected yet) must not preselect one.
    // A missing attribute keeps the old default-to-first behaviour.
    let activeTab = value === '' ? null : tabs.find((t) => t.id === value);
    if (!activeTab && value !== '') {
      activeTab = tabs[0];
      if (activeTab && !this.hasAttribute('value')) {
        this.setAttribute('value', activeTab.id);
      }
    }
    tabs.forEach((t) => t.setActive(t === activeTab));
    // The entry animation belongs to an actual switch, not to every re-measure
    // (scroll and resize also run _syncIndicator).
    const activeId = activeTab ? activeTab.id : null;
    const switched = this._lastActiveId !== null && this._lastActiveId !== activeId;
    this._lastActiveId = activeId;
    if (switched) this._playIndicatorEnter();
    requestAnimationFrame(() => {
      this._syncIndicator();
      this._scrollActiveIntoView(activeTab);
    });
  }

  _playIndicatorEnter() {
    const el = this._indicator;
    el.classList.remove('is-entering');
    // Force a reflow so re-adding the class restarts the animation.
    void el.offsetWidth;
    el.classList.add('is-entering');
  }

  // Horizontal inset applied to both ends of the moving rule. Underline keeps
  // its historical 10px; a stacked bar underlines only the middle of the cell,
  // matching the mockup's left/right 24%.
  _indicatorInset(tabWidth) {
    const variant = (this.getAttribute('variant') || 'solid').toLowerCase();
    if (variant === 'underline') return 10;
    if (variant === 'bar' && (this.getAttribute('layout') || '').toLowerCase() === 'stacked') {
      return tabWidth * 0.24;
    }
    return 0;
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
    const inset = this._indicatorInset(tabRect.width);
    this._indicator.style.transform = `translateX(${offsetX + inset}px)`;
    this._indicator.style.width = `${Math.max(0, tabRect.width - inset * 2)}px`;
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
    this.value = id;
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: id },
    }));
  }

  // Arrow keys move focus along the strip; activation stays on click/Enter/Space
  // (manual activation), which the native <button> already handles. Tabs keep
  // their natural tab order — no roving tabindex — so Tab still walks every tab
  // exactly as it did before this handler existed.
  _onKeyDown(e) {
    const keys = ['ArrowRight', 'ArrowLeft', 'ArrowDown', 'ArrowUp', 'Home', 'End'];
    if (!keys.includes(e.key)) return;
    const current = e.target.closest ? e.target.closest('.tf-tab') : null;
    if (!current || !this._scroller.contains(current)) return;

    const buttons = this._getTabs()
      .filter((t) => !t.hasAttribute('disabled'))
      .map((t) => t.querySelector('.tf-tab'))
      .filter(Boolean);
    if (buttons.length < 2) return;
    const idx = buttons.indexOf(current);
    if (idx < 0) return;

    let next;
    if (e.key === 'Home') next = buttons[0];
    else if (e.key === 'End') next = buttons[buttons.length - 1];
    else {
      const step = (e.key === 'ArrowRight' || e.key === 'ArrowDown') ? 1 : -1;
      next = buttons[(idx + step + buttons.length) % buttons.length];
    }
    if (!next || next === current) return;
    e.preventDefault();
    next.focus();
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
