// =============================================================================
// Plik: tf-window.js
// Opis: Komponent <tf-window> — pelne okno z draggable/resizable, przyciskami
//       close/min/max/collapse, slotami body/footer/actions, wariantem
//       transparent (glass). Metoda statyczna TfWindow.open(opts) tworzy
//       okno programowo i zwraca Promise rozwiazujacy sie po finalnym
//       zamknieciu okna.
//
//       Eventy:
//         'action'        — cancelable; detail.action zawiera nazwe akcji.
//                           preventDefault() anuluje domyslne zamkniecie.
//         'close-request' — cancelable; emitowany PRZED zamknieciem.
//                           preventDefault() zatrzymuje close flow.
//
//       Metody: win.close(force=false) — wymusza zamkniecie, z force=true
//       pomija event close-request.
//
// Przyklad: async confirm save
//   win.addEventListener('action', async (e) => {
//     if (e.detail.action !== 'save') return;
//     e.preventDefault(); // zatrzymaj auto-close
//     try {
//       await api.save(...);
//       win.close(true); // zamknij po sukcesie
//     } catch (err) {
//       showError(err); // zostaw okno otwarte
//     }
//   });
// =============================================================================

import { adoptControlsInto, injectSpriteIntoShadow } from './shared-styles.js';
import { Sfx } from '/js/lib/sfx.js';

let _zCounter = 1000;

class TfWindow extends HTMLElement {
  static get observedAttributes() {
    return [
      'title', 'icon', 'buttons', 'draggable', 'resizable', 'transparent',
      'min-width', 'min-height', 'initial-x', 'initial-y', 'width', 'height',
    ];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._win = null;
    this._header = null;
    this._titleEl = null;
    this._iconEl = null;
    this._controlsEl = null;
    this._actionsEl = null;
    this._bodyEl = null;
    this._footerEl = null;
    this._resizeHandle = null;

    this._drag = null;
    this._resize = null;
    this._userMoved = false;
    this._centerMode = { x: true, y: true };
    this._resizeObserver = null;

    this._onControlClick = this._onControlClick.bind(this);
    this._onHeaderPointerDown = this._onHeaderPointerDown.bind(this);
    this._onPointerMove = this._onPointerMove.bind(this);
    this._onPointerUp = this._onPointerUp.bind(this);
    this._onResizePointerDown = this._onResizePointerDown.bind(this);
    this._onMinimizedClick = this._onMinimizedClick.bind(this);
    this._onFooterClick = this._onFooterClick.bind(this);
    this._onActionsClick = this._onActionsClick.bind(this);
    this._onPointerDownFront = this._onPointerDownFront.bind(this);
    this._onViewportResize = this._onViewportResize.bind(this);
    this._onWinResize = this._onWinResize.bind(this);
  }

  connectedCallback() {
    if (!this._win) this._build();
    this._applyAttrs();
    this._positionInitial();
    this._bringToFront();
    // animacja otwarcia
    this._win.classList.add('tf-window-opening');
    this._win.addEventListener('animationend', this._onOpenAnimEnd, { once: true });
    Sfx.play('window-open');

    // Observe inner-size changes (content fills in, body loads async, etc.)
    // and recenter while the user has not taken manual control.
    if (typeof ResizeObserver !== 'undefined') {
      this._resizeObserver = new ResizeObserver(this._onWinResize);
      this._resizeObserver.observe(this._win);
    }
    window.addEventListener('resize', this._onViewportResize);
    window.addEventListener('orientationchange', this._onViewportResize);
  }

  disconnectedCallback() {
    window.removeEventListener('pointermove', this._onPointerMove);
    window.removeEventListener('pointerup', this._onPointerUp);
    window.removeEventListener('resize', this._onViewportResize);
    window.removeEventListener('orientationchange', this._onViewportResize);
    if (this._resizeObserver) {
      this._resizeObserver.disconnect();
      this._resizeObserver = null;
    }
  }

  attributeChangedCallback() {
    if (this._win) this._applyAttrs();
  }

  _build() {
    adoptControlsInto(this._shadow);
    injectSpriteIntoShadow(this._shadow);

    const win = document.createElement('div');
    win.className = 'tf-window';

    // header
    const header = document.createElement('div');
    header.className = 'tf-window-header';
    header.addEventListener('pointerdown', this._onHeaderPointerDown);

    const controls = document.createElement('div');
    controls.className = 'tf-window-controls';
    controls.addEventListener('click', this._onControlClick);

    const titleEl = document.createElement('div');
    titleEl.className = 'tf-window-title';
    const iconSvg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    iconSvg.classList.add('tf-window-icon');
    const useEl = document.createElementNS('http://www.w3.org/2000/svg', 'use');
    iconSvg.appendChild(useEl);
    const titleText = document.createElement('span');
    titleText.className = 'tf-window-title-text';
    titleEl.appendChild(iconSvg);
    titleEl.appendChild(titleText);

    const actionsEl = document.createElement('div');
    actionsEl.className = 'tf-window-actions';
    const actionsSlot = document.createElement('slot');
    actionsSlot.name = 'actions';
    actionsEl.appendChild(actionsSlot);
    actionsEl.addEventListener('click', this._onActionsClick);

    header.appendChild(controls);
    header.appendChild(titleEl);
    header.appendChild(actionsEl);

    // body
    const bodyEl = document.createElement('div');
    bodyEl.className = 'tf-window-body';
    const bodySlot = document.createElement('slot');
    bodySlot.name = 'body';
    bodyEl.appendChild(bodySlot);
    // fallback: default slot tez trafia do body
    const defaultSlot = document.createElement('slot');
    bodyEl.appendChild(defaultSlot);

    // footer
    const footerEl = document.createElement('div');
    footerEl.className = 'tf-window-footer';
    const footerSlot = document.createElement('slot');
    footerSlot.name = 'footer';
    footerEl.appendChild(footerSlot);
    footerEl.addEventListener('click', this._onFooterClick);

    // resize handle
    const resizeHandle = document.createElement('div');
    resizeHandle.className = 'tf-window-resize-handle';
    resizeHandle.addEventListener('pointerdown', this._onResizePointerDown);

    win.appendChild(header);
    win.appendChild(bodyEl);
    win.appendChild(footerEl);
    win.appendChild(resizeHandle);

    win.addEventListener('click', this._onMinimizedClick);
    win.addEventListener('pointerdown', this._onPointerDownFront);

    this._shadow.appendChild(win);

    this._win = win;
    this._header = header;
    this._titleEl = titleText;
    this._iconEl = useEl;
    this._iconSvg = iconSvg;
    this._controlsEl = controls;
    this._actionsEl = actionsEl;
    this._bodyEl = bodyEl;
    this._footerEl = footerEl;
    this._resizeHandle = resizeHandle;

    // ukryj footer gdy slot pusty (po pierwszym layoucie)
    footerSlot.addEventListener('slotchange', () => {
      const assigned = footerSlot.assignedElements();
      footerEl.style.display = assigned.length ? '' : 'none';
    });
  }

  _applyAttrs() {
    const title = this.getAttribute('title') || '';
    this._titleEl.textContent = title;

    const icon = this.getAttribute('icon');
    if (icon) {
      this._iconSvg.style.display = '';
      this._iconEl.setAttribute('href', `#i-${icon}`);
    } else {
      this._iconSvg.style.display = 'none';
    }

    const buttons = (this.getAttribute('buttons') || 'close,minimize,maximize')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    this._renderControls(buttons);

    if (this.hasAttribute('draggable')) this._win.setAttribute('data-draggable', '');
    else this._win.removeAttribute('data-draggable');

    if (this.hasAttribute('resizable')) this._win.setAttribute('data-resizable', '');
    else this._win.removeAttribute('data-resizable');

    if (this.hasAttribute('transparent')) {
      this._win.setAttribute('data-transparent', 'true');
    } else {
      this._win.removeAttribute('data-transparent');
    }

    const minW = this.getAttribute('min-width');
    const minH = this.getAttribute('min-height');
    if (minW) this._win.style.minWidth = `${parseInt(minW, 10)}px`;
    if (minH) this._win.style.minHeight = `${parseInt(minH, 10)}px`;

    const w = this.getAttribute('width');
    const h = this.getAttribute('height');
    if (w) this._win.style.width = `${parseInt(w, 10)}px`;
    if (h) this._win.style.height = `${parseInt(h, 10)}px`;
  }

  _renderControls(actions) {
    const symbolMap = {
      close: 'x',
      minimize: 'min',
      maximize: 'max',
      collapse: 'collapse',
    };
    const labelMap = {
      close: 'Zamknij',
      minimize: 'Minimalizuj',
      maximize: 'Maksymalizuj',
      collapse: 'Zwin',
    };
    const html = actions.map((a) => {
      const sym = symbolMap[a];
      if (!sym) return '';
      return `<button class="tf-window-control" type="button" data-action="${a}" title="${labelMap[a]}" aria-label="${labelMap[a]}"><svg><use href="#i-${sym}"/></svg></button>`;
    }).join('');
    this._controlsEl.innerHTML = html;
  }

  _positionInitial() {
    if (this._positioned) return;
    this._positioned = true;

    const ix = this.getAttribute('initial-x');
    const iy = this.getAttribute('initial-y');

    this._centerMode = {
      x: ix == null || ix === 'center',
      y: iy == null || iy === 'center',
    };

    // Immediate placement using best-known dimensions. For axes with an
    // explicit numeric position, honour it; for centered axes compute
    // against the current layout (may still be stale if content loads
    // async — ResizeObserver will correct it on next layout).
    if (!this._centerMode.x) {
      const left = parseInt(ix, 10);
      this._win.style.left = `${Number.isFinite(left) ? left : 12}px`;
    }
    if (!this._centerMode.y) {
      const top = parseInt(iy, 10);
      this._win.style.top = `${Number.isFinite(top) ? top : 12}px`;
    }

    // Wait for layout so offsetWidth/Height reflect final size, then center.
    requestAnimationFrame(() => this._recenterIfNeeded());
  }

  // Recenter the window on centered axes unless the user has moved it.
  // Uses current rendered size so it works even when content loads async.
  _recenterIfNeeded() {
    if (!this._win || !this.isConnected) return;
    if (this._userMoved) return;
    if (this._win.classList.contains('maximized') || this._win.classList.contains('minimized')) return;

    // Use offset metrics — they ignore CSS transforms, so the open-animation
    // scale/translate does not distort centering math.
    const w = this._win.offsetWidth;
    const h = this._win.offsetHeight;
    if (!w || !h) return;

    const pad = 8;
    if (this._centerMode.x) {
      const left = Math.max(pad, Math.floor((window.innerWidth - w) / 2));
      this._win.style.left = `${left}px`;
    }
    if (this._centerMode.y) {
      const top = Math.max(pad, Math.floor((window.innerHeight - h) / 2));
      this._win.style.top = `${top}px`;
    }
  }

  _onWinResize() {
    // Inner window size changed (content flowed in, resized layout).
    this._recenterIfNeeded();
  }

  _onViewportResize() {
    this._recenterIfNeeded();
  }

  _bringToFront() {
    _zCounter += 1;
    this._win.style.zIndex = String(_zCounter);
  }

  // ========== controls ==========

  _onControlClick(e) {
    const btn = e.target.closest('.tf-window-control');
    if (!btn) return;
    e.stopPropagation();
    const action = btn.dataset.action;

    // minimize/maximize/collapse to tylko zmiana stanu — nigdy nie zamykaja
    if (action === 'minimize' || action === 'maximize' || action === 'collapse') {
      this._emitAction(action, 'controls');
      const clsMap = { minimize: 'minimized', maximize: 'maximized', collapse: 'collapsed' };
      this._toggleClass(clsMap[action]);
      return;
    }

    if (action === 'close') {
      const evt = this._emitAction('close', 'controls');
      if (evt.defaultPrevented) return;
      this.close();
    }
  }

  // =============================================================================
  // Emituje cancelable event 'action' i zwraca go, aby wolajacy mogl sprawdzic
  // defaultPrevented.
  // =============================================================================
  _emitAction(action, source) {
    const evt = new CustomEvent('action', {
      bubbles: true,
      cancelable: true,
      detail: { action, source },
    });
    this.dispatchEvent(evt);
    return evt;
  }

  _onMinimizedClick(e) {
    if (!this._win.classList.contains('minimized')) return;
    if (e.composedPath().some((el) => el.classList && el.classList.contains('tf-window-control'))) return;
    this._win.classList.remove('minimized');
    this._bringToFront();
  }

  _onPointerDownFront() {
    this._bringToFront();
  }

  _toggleClass(cls) {
    this._win.classList.toggle(cls);
    if (cls === 'maximized') {
      this._win.classList.remove('collapsed', 'minimized');
    }
    if (cls === 'minimized') {
      this._win.classList.remove('collapsed');
    }
  }

  // =============================================================================
  // Publiczne zamkniecie. force=true pomija event close-request.
  // =============================================================================
  close(force = false) {
    if (this._closing) return;
    if (!force) {
      const evt = new CustomEvent('close-request', {
        bubbles: true,
        cancelable: true,
      });
      this.dispatchEvent(evt);
      if (evt.defaultPrevented) return;
    }
    this._closing = true;
    Sfx.play('window-close');
    this._win.classList.add('tf-window-closing');
    setTimeout(() => {
      this.remove();
    }, 240);
  }

  // ========== drag ==========

  _onHeaderPointerDown(e) {
    if (!this.hasAttribute('draggable')) return;
    if (e.target.closest('.tf-window-control') || e.target.closest('button')) return;
    if (this._win.classList.contains('maximized') || this._win.classList.contains('minimized')) return;

    const r = this._win.getBoundingClientRect();
    this._drag = {
      startX: e.clientX,
      startY: e.clientY,
      origX: r.left,
      origY: r.top,
      pointerId: e.pointerId,
    };
    this._bringToFront();
    try { this._header.setPointerCapture(e.pointerId); } catch (_) { /* ignored */ }
    window.addEventListener('pointermove', this._onPointerMove);
    window.addEventListener('pointerup', this._onPointerUp);
  }

  _onResizePointerDown(e) {
    if (!this.hasAttribute('resizable')) return;
    if (this._win.classList.contains('maximized') || this._win.classList.contains('minimized')) return;
    e.stopPropagation();
    this._resize = {
      startX: e.clientX,
      startY: e.clientY,
      origW: this._win.offsetWidth,
      origH: this._win.offsetHeight,
      pointerId: e.pointerId,
    };
    try { this._resizeHandle.setPointerCapture(e.pointerId); } catch (_) { /* ignored */ }
    window.addEventListener('pointermove', this._onPointerMove);
    window.addEventListener('pointerup', this._onPointerUp);
  }

  _onPointerMove(e) {
    if (this._drag) {
      const nx = this._drag.origX + (e.clientX - this._drag.startX);
      const ny = this._drag.origY + (e.clientY - this._drag.startY);
      this._win.style.left = `${nx}px`;
      this._win.style.top = `${ny}px`;
      this._userMoved = true;
      return;
    }
    if (this._resize) {
      this._userMoved = true;
      const minW = parseInt(this.getAttribute('min-width'), 10) || 320;
      const minH = parseInt(this.getAttribute('min-height'), 10) || 200;
      const nw = Math.max(minW, this._resize.origW + (e.clientX - this._resize.startX));
      const nh = Math.max(minH, this._resize.origH + (e.clientY - this._resize.startY));
      this._win.style.width = `${nw}px`;
      this._win.style.height = `${nh}px`;
    }
  }

  _onPointerUp() {
    this._drag = null;
    this._resize = null;
    window.removeEventListener('pointermove', this._onPointerMove);
    window.removeEventListener('pointerup', this._onPointerUp);
  }

  // ========== action dispatch from footer/actions slot ==========

  _onFooterClick(e) {
    const actionEl = e.target.closest('[data-action]');
    if (!actionEl) return;
    const action = actionEl.getAttribute('data-action');
    this._emitAction(action, 'footer');
  }

  _onActionsClick(e) {
    const actionEl = e.target.closest('[data-action]');
    if (!actionEl) return;
    const action = actionEl.getAttribute('data-action');
    this._emitAction(action, 'actions');
  }
}

customElements.define('tf-window', TfWindow);

// =============================================================================
// Statyczny helper TfWindow.open({...}) — tworzy okno, zwraca Promise.
// Rozwiazuje { action, detail } po pierwszym evencie "action".
// =============================================================================

TfWindow.open = function openWindow(opts = {}) {
  const {
    title = '',
    icon = null,
    body = '',
    footer = '',
    buttons = 'close',
    draggable = true,
    resizable = false,
    transparent = false,
    minWidth = 320,
    minHeight = 200,
    width = null,
    height = null,
    initialX = 'center',
    initialY = 'center',
    closeOnAction = true,
    modal = false,
  } = opts;

  return new Promise((resolve) => {
    let backdrop = null;
    if (modal) {
      backdrop = document.createElement('div');
      backdrop.className = 'tf-window-backdrop';
      document.body.appendChild(backdrop);
    }

    const win = document.createElement('tf-window');
    win.setAttribute('title', title);
    if (icon) win.setAttribute('icon', icon);
    win.setAttribute('buttons', buttons);
    if (draggable) win.setAttribute('draggable', '');
    if (resizable) win.setAttribute('resizable', '');
    if (transparent) win.setAttribute('transparent', '');
    win.setAttribute('min-width', String(minWidth));
    win.setAttribute('min-height', String(minHeight));
    if (width) win.setAttribute('width', String(width));
    if (height) win.setAttribute('height', String(height));
    win.setAttribute('initial-x', String(initialX));
    win.setAttribute('initial-y', String(initialY));

    const bodyWrap = document.createElement('div');
    bodyWrap.slot = 'body';
    if (typeof body === 'string') bodyWrap.innerHTML = body;
    else if (body instanceof Node) bodyWrap.appendChild(body);
    win.appendChild(bodyWrap);

    if (footer) {
      const footWrap = document.createElement('div');
      footWrap.slot = 'footer';
      if (typeof footer === 'string') footWrap.innerHTML = footer;
      else if (footer instanceof Node) footWrap.appendChild(footer);
      win.appendChild(footWrap);
    }

    let resolved = false;
    let lastAction = null;

    const finalize = () => {
      if (resolved) return;
      resolved = true;
      if (backdrop && backdrop.isConnected) backdrop.remove();
      resolve({ action: lastAction });
    };

    // Finalne zamkniecie niezaleznie od zrodla (close z header, close(true) itp.)
    win.addEventListener('close-request', () => {
      // pozwalamy zamknieciu isc dalej; finalize nastapi po odlaczeniu z DOM
    });

    // Obserwujemy faktyczne odlaczenie aby rozwiazac Promise dopiero po close.
    const mo = new MutationObserver(() => {
      if (!win.isConnected) {
        mo.disconnect();
        finalize();
      }
    });
    mo.observe(document.body, { childList: true, subtree: true });

    win.addEventListener('action', (e) => {
      const action = e.detail?.action;
      if (!action) return;
      lastAction = action;

      // min/max/collapse — nigdy nie zamykaja
      if (action === 'minimize' || action === 'maximize' || action === 'collapse') {
        return;
      }

      if (!closeOnAction) return;

      // close i cancel — domyslne zamkniecie, chyba ze konsument wywolal preventDefault.
      // Inne akcje tez zamykaja domyslnie (save, confirm, delete), ale konsument moze
      // zatrzymac przez e.preventDefault() i samodzielnie wywolac win.close(true).
      if (e.defaultPrevented) return;

      // 'close' z naglowka juz wyemitowal close-request przez _onControlClick -> close().
      // W footer/actions uzytkownik klika tf-button[data-action=close] — tu musimy
      // samodzielnie wywolac close (ale tylko jesli okno jeszcze zyje).
      if (win.isConnected && !win._closing) {
        win.close();
      }
    });

    document.body.appendChild(win);
  });
};

// Confirm dialog helper — oparty o TfWindow.open
TfWindow.confirm = function confirmDialog({
  title = 'Potwierdz',
  message = '',
  description = '',
  confirmLabel = 'Potwierdz',
  cancelLabel = 'Anuluj',
  danger = false,
  icon = null,
} = {}) {
  const bodyHtml = `
    <p style="color: var(--tf-text-2); font-size: 13px;">${message}</p>
    ${description ? `<p style="color: var(--tf-text-3); font-size: 12px;">${description}</p>` : ''}
  `;
  const confirmVariant = danger ? 'danger-solid' : 'primary';
  const footerHtml = `
    <tf-button variant="ghost" data-action="cancel">${cancelLabel}</tf-button>
    <tf-button variant="${confirmVariant}"${danger ? ' icon="trash"' : ''} data-action="confirm">${confirmLabel}</tf-button>
  `;
  return TfWindow.open({
    title,
    icon,
    body: bodyHtml,
    footer: footerHtml,
    buttons: 'close',
    draggable: true,
    resizable: false,
    minWidth: 380,
    minHeight: 160,
    width: 420,
    modal: true,
  }).then((result) => result.action === 'confirm');
};

export { TfWindow };
export const openTfWindow = TfWindow.open;
export const tfConfirm = TfWindow.confirm;
