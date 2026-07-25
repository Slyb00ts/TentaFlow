// =============================================================================
// File: tf-kanban.js — drag & drop task board
// Description: <tf-kanban> — column board with draggable cards (Projects →
//   Tasks "Board" view). Dragging is built on pointer events + setPointerCapture
//   (the same mechanism as tf-window): HTML5 drag-and-drop is unusable here
//   because it never fires on touch and its drag image cannot be styled.
//   A press becomes a drag only after a 6 px threshold, so a click still opens
//   the card. While dragging only two nodes move: a fixed-position ghost clone
//   and a placeholder marking the insertion point — the board itself is never
//   re-rendered, which keeps 200+ cards smooth.
//   Keyboard is a first-class path: Space grabs/drops, arrows move the card
//   between columns and positions, Escape cancels; column counts and grab state
//   are announced through an aria-live region.
//   The component is i18n-agnostic — every user-visible string comes from the
//   host through `labels` (English fallbacks only).
//
//   Attributes: empty-text, card-min-width, dense, readonly.
//   Properties: columns, cards, readOnly, labels.
//   Methods   : setCards(cards), patchCard(id, partial), revertMove(cardId).
//   Events    : "card-move"   {cardId, from, to, index} — emitted BEFORE the
//                 host persists anything; the board already shows the new
//                 position, the host calls revertMove(cardId) when the server
//                 rejects the change.
//               "card-open"   {cardId}
//               "card-menu"   {cardId, actionId}
//               "column-add"  {columnId}
//
// Example:
//   const b = document.createElement('tf-kanban');
//   b.columns = [{ id: 'todo', label: 'To do', accent: 'info', limit: 5 }];
//   b.cards = [{ id: 'DEF-1', column: 'todo', title: 'Export fails',
//                badge: 'defect', badgeKind: 'danger', badgeIcon: 'alert',
//                meta: [{ text: 'high', tone: 'warning' }],
//                footer: { left: { icon: 'clock', text: '25.07' }, right: 'AK' } }];
//   b.addEventListener('card-move', (e) => persist(e.detail));
// =============================================================================

import { adoptControlsInto, injectSpriteIntoShadow } from './shared-styles.js';
import './tf-menu.js';

const SVG_NS = 'http://www.w3.org/2000/svg';

const DRAG_THRESHOLD = 6;        // px of pointer travel before a press is a drag
const EDGE_ZONE = 56;            // px from a scroll edge where auto-scroll kicks in
const EDGE_SPEED = 22;           // px per frame at the very edge
const DEFAULT_CARD_MIN_WIDTH = 248;
const MENU_REOPEN_GUARD_MS = 300; // click-after-outside-close guard for the kebab

const DEFAULT_LABELS = {
  empty: 'No cards',
  dragHint: 'Press Space to pick up the card, arrow keys to move it, Space to drop, Escape to cancel.',
  countLabel: '{n}',
  limitLabel: '{n}/{limit}',
  cardsLabel: '{n} cards',
  limitExceeded: 'WIP limit exceeded: {n} of {limit}',
  addCard: 'Add card',
  cardMenu: 'Card actions',
  grabbed: '{title} picked up. {column}, position {index} of {count}.',
  moved: '{column}, position {index} of {count}.',
  dropped: '{title} dropped in {column}, position {index}.',
  cancelled: 'Move cancelled.',
};

// Tone name -> design token. Anything outside this map falls back to neutral,
// so a stray value from the host can never inject arbitrary CSS.
const TONES = {
  neutral: 'var(--k-text-3)',
  accent: 'var(--k-accent-1)',
  accent2: 'var(--k-accent-2)',
  info: 'var(--k-info)',
  success: 'var(--k-success)',
  warning: 'var(--k-warning)',
  danger: 'var(--k-danger)',
};

const HEX_RE = /^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const ICON_RE = /^[a-z0-9_-]{1,64}$/i;

function fmt(tpl, vars) {
  return String(tpl ?? '').replace(/\{(\w+)\}/g, (m, k) => (
    vars && vars[k] !== undefined ? String(vars[k]) : m
  ));
}

// Accepts a tone name or a plain hex colour; everything else yields null so the
// caller keeps the default styling instead of writing an unvalidated value.
function resolveColor(value) {
  const v = String(value ?? '').trim();
  if (!v) return null;
  if (TONES[v]) return TONES[v];
  if (HEX_RE.test(v)) return v;
  return null;
}

function iconSvg(name) {
  const v = String(name ?? '').trim();
  if (!ICON_RE.test(v)) return null;
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.classList.add('ico');
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#i-${v}`);
  svg.appendChild(use);
  return svg;
}

// ---------------------------------------------------------------------------
// Hit testing (pure, unit-tested)
// ---------------------------------------------------------------------------

// Column under the pointer. `rects` is [{id, left, right}] in board order.
// Columns tile horizontally, so only x matters; a pointer dragged past the
// first/last column snaps to it instead of losing the drop target.
export function columnAtPoint(rects, x) {
  if (!Array.isArray(rects) || rects.length === 0) return null;
  for (const r of rects) {
    if (x >= r.left && x <= r.right) return r.id;
  }
  let best = null;
  let bestDist = Infinity;
  for (const r of rects) {
    const dist = x < r.left ? r.left - x : x - r.right;
    if (dist < bestDist) { bestDist = dist; best = r.id; }
  }
  return best;
}

// Insertion index for a pointer at `y` over a column holding `rects`
// ([{top, bottom}] in visual order, dragged card excluded). The result is the
// number of cards that stay above the drop point.
export function insertIndexAt(rects, y) {
  if (!Array.isArray(rects) || rects.length === 0) return 0;
  let index = 0;
  for (const r of rects) {
    const mid = r.top + (r.bottom - r.top) / 2;
    if (y > mid) index += 1;
    else break;
  }
  return index;
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const STYLE = `
:host {
  display: block;
  /* Local aliases so the component works with either token family and still
     renders standalone (tests, embeds) without the dashboard stylesheets. */
  --k-bg-2: var(--tf-bg-2, var(--bg-2, #0a0e22));
  --k-bg-3: var(--tf-bg-3, var(--bg-3, #131736));
  --k-bg-card: var(--tf-bg-card, var(--bg-card, #141836));
  --k-bg-card-hover: var(--tf-bg-card-hover, var(--bg-card-hover, #1a1f45));
  --k-border: var(--tf-border, var(--border, #1f2548));
  --k-border-hover: var(--tf-border-hover, var(--border-hover, #2f3668));
  --k-text: var(--tf-text, var(--text, #f5f6ff));
  --k-text-2: var(--tf-text-2, var(--text-2, #a0a8c8));
  --k-text-3: var(--tf-text-3, var(--text-3, #6a7196));
  --k-accent-1: var(--tf-accent-1, var(--accent-1, #6366f1));
  --k-accent-2: var(--tf-accent-2, var(--accent-2, #a78bfa));
  --k-success: var(--tf-success, var(--success, #22c55e));
  --k-warning: var(--tf-warning, var(--warning, #f59e0b));
  --k-danger: var(--tf-danger, var(--danger, #ef4444));
  --k-info: var(--tf-info, var(--info, #60a5fa));
  --k-radius: var(--tf-radius, var(--radius, 10px));
  --k-radius-sm: var(--tf-radius-sm, var(--radius-sm, 6px));
  --k-shadow-lg: var(--tf-shadow-lg, var(--shadow-lg, 0 16px 48px rgba(0,0,0,0.7)));
  --k-col-w: ${DEFAULT_CARD_MIN_WIDTH}px;
  --k-gap: 12px;
  --k-card-pad: 10px 12px;
}
* { box-sizing: border-box; }
.ico {
  width: 14px; height: 14px; flex: none;
  stroke: currentColor; stroke-width: 1.75;
  stroke-linecap: round; stroke-linejoin: round; fill: none;
}

.board {
  display: flex;
  align-items: stretch;
  gap: var(--k-gap);
  height: 100%;
  min-height: 220px;
  overflow-x: auto;
  overflow-y: hidden;
  padding-bottom: 4px;
  scrollbar-width: thin;
}
:host([dense]) { --k-gap: 8px; --k-card-pad: 7px 9px; }

.col {
  display: flex;
  flex-direction: column;
  flex: 1 0 var(--k-col-w);
  min-width: var(--k-col-w);
  max-width: 100%;
  min-height: 0;
  background: var(--k-bg-2);
  border: 1px solid var(--k-border);
  border-radius: var(--k-radius);
  padding: 10px;
}
.col.drop-target { border-color: var(--k-accent-1); }
.col.over-limit { border-color: var(--k-warning); }

.col-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 2px 4px 10px;
  font-size: 12px;
  font-weight: 700;
  color: var(--k-text);
}
.col-left { display: flex; align-items: center; gap: 8px; min-width: 0; }
.col-dot {
  width: 8px; height: 8px; border-radius: 50%; flex: none;
  background: var(--k-col-accent, var(--k-text-3));
  box-shadow: 0 0 8px var(--k-col-accent, transparent);
}
.col-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.cnt {
  font-size: 10px; font-weight: 700; padding: 1px 7px; border-radius: 8px;
  background: var(--k-bg-3); color: var(--k-text-2);
  font-variant-numeric: tabular-nums;
}
.col.over-limit .cnt { background: color-mix(in srgb, var(--k-warning) 20%, transparent); color: var(--k-warning); }
.col-add {
  width: 24px; height: 24px; flex: none; padding: 0;
  display: inline-flex; align-items: center; justify-content: center;
  border-radius: var(--k-radius-sm); border: 1px solid var(--k-border);
  background: var(--k-bg-3); color: var(--k-text-3); cursor: pointer;
  transition: color .12s, border-color .12s;
}
.col-add:hover { color: var(--k-accent-2); border-color: var(--k-accent-1); }
.col-add:focus-visible { outline: 2px solid var(--k-accent-1); outline-offset: 1px; }
.col-add .ico { width: 13px; height: 13px; }

.col-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
  flex: 1;
  min-height: 60px;
  overflow-y: auto;
  overflow-x: hidden;
  overscroll-behavior: contain;
  scrollbar-width: thin;
  padding: 1px;
}

.col-empty {
  text-align: center; color: var(--k-text-3); font-size: 11px;
  padding: 18px 8px; border: 1px dashed var(--k-border);
  border-radius: var(--k-radius-sm);
}
.col-empty[hidden] { display: none; }

.card {
  position: relative;
  flex: none;
  background: var(--k-bg-card);
  border: 1px solid var(--k-border);
  border-left: 3px solid var(--k-card-accent, var(--k-border));
  border-radius: var(--k-radius-sm);
  padding: var(--k-card-pad);
  cursor: grab;
  touch-action: none;
  /* A press on a card starts a drag, so native text selection would only ever
     produce stray highlights across the board. */
  -webkit-user-select: none;
  user-select: none;
  transition: border-color .12s, background .12s, box-shadow .12s;
}
.card:hover { border-color: var(--k-border-hover); background: var(--k-bg-card-hover); }
.card:focus-visible { outline: 2px solid var(--k-accent-1); outline-offset: 1px; }
.card.disabled { cursor: not-allowed; opacity: .72; }
:host([readonly]) .card { cursor: pointer; }
.card.is-dragging { display: none; }
.card.is-grabbed {
  border-color: var(--k-accent-1);
  box-shadow: 0 0 0 2px var(--k-accent-1), var(--k-shadow-lg);
}
.board.dragging .card { transition: none; }
.board.dragging .card:hover { background: var(--k-bg-card); border-color: var(--k-border); }

.card-head {
  display: flex; align-items: center; justify-content: space-between;
  gap: 8px; margin-bottom: 6px; min-height: 22px;
}
.badge {
  display: inline-flex; align-items: center; gap: 6px;
  font-size: 11px; font-weight: 700; padding: 3px 9px; border-radius: 12px;
  color: var(--k-badge, var(--k-text-2));
  background: color-mix(in srgb, var(--k-badge, var(--k-text-3)) 14%, transparent);
  border: 1px solid color-mix(in srgb, var(--k-badge, var(--k-text-3)) 34%, transparent);
  max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.badge .ico { width: 12px; height: 12px; flex: none; }
.card-menu-btn {
  width: 24px; height: 24px; flex: none; padding: 0; line-height: 1;
  display: inline-flex; align-items: center; justify-content: center;
  border: 1px solid transparent; border-radius: var(--k-radius-sm);
  background: transparent; color: var(--k-text-3);
  font-size: 15px; cursor: pointer;
}
.card-menu-btn:hover { color: var(--k-text); background: var(--k-bg-3); }
.card-menu-btn:focus-visible { outline: 2px solid var(--k-accent-1); outline-offset: 1px; }

.card-title {
  margin: 0 0 6px; font-size: 12.5px; font-weight: 600; line-height: 1.35;
  color: var(--k-text); overflow-wrap: anywhere;
}
.card-meta { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
.card-meta:empty, .card-foot:empty { display: none; }
.bit {
  display: inline-flex; align-items: center; gap: 5px;
  font-size: 11px; color: var(--k-bit, var(--k-text-2));
}
.bit .ico { width: 12px; height: 12px; color: currentColor; }
.bit .dot {
  width: 6px; height: 6px; border-radius: 50%;
  background: currentColor; box-shadow: 0 0 6px currentColor;
}
.bit.toned {
  font-weight: 800; font-size: 10px; text-transform: uppercase;
  letter-spacing: .03em; padding: 3px 9px; border-radius: 12px;
  background: color-mix(in srgb, var(--k-bit) 15%, transparent);
}
.card-foot { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.foot-side { display: flex; align-items: center; gap: 8px; min-width: 0; }
.avatar {
  width: 22px; height: 22px; border-radius: 50%; flex: none;
  display: inline-flex; align-items: center; justify-content: center;
  font-size: 9px; font-weight: 700; color: #fff;
  background: var(--tf-gradient-accent, var(--gradient-accent, linear-gradient(135deg, #6366f1, #a78bfa)));
}

.placeholder {
  flex: none;
  border: 1px dashed var(--k-accent-1);
  border-radius: var(--k-radius-sm);
  background: color-mix(in srgb, var(--k-accent-1) 10%, transparent);
}
.placeholder.warn { border-color: var(--k-warning); background: color-mix(in srgb, var(--k-warning) 10%, transparent); }

.drag-layer { position: fixed; inset: 0; pointer-events: none; z-index: 90; }
.drag-layer:empty { display: none; }
.ghost {
  position: fixed; left: 0; top: 0; margin: 0;
  pointer-events: none; opacity: .96;
  box-shadow: var(--k-shadow-lg);
  border-color: var(--k-accent-1);
  background: var(--k-bg-card-hover);
  cursor: grabbing;
  will-change: transform;
}
.menu-layer { position: fixed; z-index: 95; }
.menu-layer:empty { display: none; }

.sr-only {
  position: absolute; width: 1px; height: 1px; margin: -1px; padding: 0;
  overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; border: 0;
}

@media (max-width: 720px) {
  .board { scroll-snap-type: x proximity; }
  .col { scroll-snap-align: start; }
}
`;

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export class TfKanban extends HTMLElement {
  static get observedAttributes() {
    return ['empty-text', 'card-min-width', 'dense', 'readonly'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });

    this._columns = [];
    this._cards = [];
    this._byId = new Map();          // card id -> card object
    this._cardEls = new Map();       // card id -> element
    this._colEls = new Map();        // column id -> { root, body, count, empty, add, label }
    this._labels = { ...DEFAULT_LABELS };

    this._drag = null;               // pointer drag state
    this._grab = null;               // keyboard grab state
    this._lastMove = null;           // last committed move, for revertMove()
    this._menu = null;               // { cardId, el }
    this._menuClosedFor = null;
    this._menuClosedAt = 0;
    this._suppressClick = false;
    this._rectCache = null;

    this._onPointerDown = this._onPointerDown.bind(this);
    this._onPointerMove = this._onPointerMove.bind(this);
    this._onPointerUp = this._onPointerUp.bind(this);
    this._onPointerCancel = this._onPointerCancel.bind(this);
    this._onDragKey = this._onDragKey.bind(this);
    this._onClick = this._onClick.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
    this._tick = this._tick.bind(this);
  }

  connectedCallback() {
    if (!this._built) {
      this._built = true;
      this._build();
      adoptControlsInto(this._shadow);
      injectSpriteIntoShadow(this._shadow);
      this._render();
    }
    this._applyAttrs();
  }

  disconnectedCallback() {
    this._cancelDrag();
    this._closeMenu();
    window.removeEventListener('pointermove', this._onPointerMove);
    window.removeEventListener('pointerup', this._onPointerUp);
    window.removeEventListener('pointercancel', this._onPointerCancel);
    window.removeEventListener('keydown', this._onDragKey, true);
  }

  attributeChangedCallback(name) {
    if (!this._built) return;
    this._applyAttrs();
    if (name === 'empty-text') this._updateColumnStates();
  }

  // ------------------------------------------------------------- properties

  get columns() { return this._columns.slice(); }
  set columns(list) {
    this._columns = (Array.isArray(list) ? list : [])
      .filter((c) => c && c.id !== undefined && c.id !== null)
      .map((c) => ({ ...c, id: String(c.id) }));
    if (this._built) this._render();
  }

  get cards() { return this._cards.slice(); }
  set cards(list) { this.setCards(list); }

  get readOnly() { return this.hasAttribute('readonly'); }
  set readOnly(v) {
    if (v) this.setAttribute('readonly', '');
    else this.removeAttribute('readonly');
  }

  get labels() { return this._labels; }
  set labels(dict) {
    this._labels = { ...DEFAULT_LABELS, ...(dict || {}) };
    if (this._built) this._render();
  }

  // ---------------------------------------------------------------- methods

  // Replaces the whole card set. Card elements are rebuilt; the drag/grab in
  // flight is cancelled because its DOM anchors disappear.
  setCards(list) {
    this._cancelDrag();
    this._cancelGrab(false);
    this._cards = (Array.isArray(list) ? list : [])
      .filter((c) => c && c.id !== undefined && c.id !== null)
      .map((c) => ({ ...c, id: String(c.id) }));
    this._byId = new Map(this._cards.map((c) => [c.id, c]));
    this._lastMove = null;
    if (this._built) this._render();
  }

  // Merges `partial` into one card and re-renders just that card element.
  patchCard(id, partial) {
    const key = String(id);
    const card = this._byId.get(key);
    if (!card || !partial) return;
    const movedColumn = partial.column !== undefined && String(partial.column) !== card.column;
    Object.assign(card, partial, { id: key });
    if (card.column !== undefined) card.column = String(card.column);
    const el = this._cardEls.get(key);
    if (!el) return;
    if (movedColumn) {
      const target = this._colEls.get(card.column);
      if (target) target.body.insertBefore(el, target.empty);
      else el.remove();
      this._syncOrderFromDom();
      this._updateColumnStates();
    }
    this._fillCard(el, card);
  }

  // Undoes the last committed move (server rejected it). Silent: no card-move.
  revertMove(cardId) {
    const key = String(cardId);
    const move = this._lastMove;
    if (!move || move.cardId !== key) return false;
    this._lastMove = null;
    return this._applyMove(key, move.from, move.fromIndex);
  }

  // -------------------------------------------------------------- build/DOM

  _build() {
    const style = document.createElement('style');
    style.textContent = STYLE;
    this._shadow.appendChild(style);

    const board = document.createElement('div');
    board.className = 'board';
    board.setAttribute('part', 'board');
    board.addEventListener('pointerdown', this._onPointerDown);
    board.addEventListener('click', this._onClick);
    board.addEventListener('keydown', this._onKeyDown);
    this._board = board;
    this._shadow.appendChild(board);

    const hint = document.createElement('div');
    hint.className = 'sr-only';
    hint.id = 'tf-kanban-hint';
    this._hint = hint;
    this._shadow.appendChild(hint);

    const live = document.createElement('div');
    live.className = 'sr-only';
    live.setAttribute('aria-live', 'polite');
    live.setAttribute('aria-atomic', 'true');
    this._live = live;
    this._shadow.appendChild(live);

    const dragLayer = document.createElement('div');
    dragLayer.className = 'drag-layer';
    dragLayer.setAttribute('aria-hidden', 'true');
    this._dragLayer = dragLayer;
    this._shadow.appendChild(dragLayer);

    const menuLayer = document.createElement('div');
    menuLayer.className = 'menu-layer';
    this._menuLayer = menuLayer;
    this._shadow.appendChild(menuLayer);
  }

  _applyAttrs() {
    const w = parseInt(this.getAttribute('card-min-width') || '', 10);
    this.style.setProperty('--k-col-w', `${Number.isFinite(w) && w > 80 ? w : DEFAULT_CARD_MIN_WIDTH}px`);
    if (this._hint) this._hint.textContent = this._labels.dragHint;
    if (this.readOnly) {
      this._cancelDrag();
      this._cancelGrab(false);
    }
    this._colEls.forEach((col) => {
      if (col.add) col.add.hidden = this.readOnly;
    });
  }

  _render() {
    this._board.textContent = '';
    this._colEls.clear();
    this._cardEls.clear();

    const byColumn = new Map();
    for (const card of this._cards) {
      const col = String(card.column ?? '');
      if (!byColumn.has(col)) byColumn.set(col, []);
      byColumn.get(col).push(card);
    }

    for (const column of this._columns) {
      const root = document.createElement('section');
      root.className = 'col';
      root.dataset.col = column.id;
      root.setAttribute('part', 'column');
      root.setAttribute('role', 'group');
      const accent = resolveColor(column.accent);
      if (accent) root.style.setProperty('--k-col-accent', accent);

      const head = document.createElement('header');
      head.className = 'col-head';

      const left = document.createElement('div');
      left.className = 'col-left';
      const dot = document.createElement('span');
      dot.className = 'col-dot';
      const label = document.createElement('span');
      label.className = 'col-label';
      label.textContent = String(column.label ?? column.id);
      const count = document.createElement('span');
      count.className = 'cnt';
      left.append(dot, label, count);

      const add = document.createElement('button');
      add.type = 'button';
      add.className = 'col-add';
      add.dataset.add = column.id;
      add.setAttribute('aria-label', this._labels.addCard);
      add.title = this._labels.addCard;
      const plus = iconSvg('plus');
      if (plus) add.appendChild(plus);
      else add.textContent = '+';
      add.hidden = this.readOnly;

      head.append(left, add);

      const body = document.createElement('div');
      body.className = 'col-body';
      body.dataset.col = column.id;
      body.setAttribute('role', 'list');

      const empty = document.createElement('div');
      empty.className = 'col-empty';
      empty.textContent = this.getAttribute('empty-text') || this._labels.empty;

      root.append(head, body);
      this._board.appendChild(root);
      this._colEls.set(column.id, { root, body, count, empty, add, label: String(column.label ?? column.id) });

      for (const card of byColumn.get(column.id) || []) {
        const el = this._createCardEl(card);
        body.appendChild(el);
        this._cardEls.set(card.id, el);
      }
      body.appendChild(empty);
    }

    this._updateColumnStates();
  }

  _createCardEl(card) {
    const el = document.createElement('article');
    el.className = 'card';
    el.dataset.id = card.id;
    el.setAttribute('part', 'card');
    el.setAttribute('role', 'listitem');
    el.tabIndex = 0;
    el.setAttribute('aria-describedby', 'tf-kanban-hint');
    this._fillCard(el, card);
    return el;
  }

  _fillCard(el, card) {
    el.textContent = '';
    el.classList.toggle('disabled', !!card.disabled);
    if (card.disabled) el.setAttribute('aria-disabled', 'true');
    else el.removeAttribute('aria-disabled');

    const accent = resolveColor(card.accent);
    if (accent) el.style.setProperty('--k-card-accent', accent);
    else el.style.removeProperty('--k-card-accent');

    const head = document.createElement('div');
    head.className = 'card-head';

    if (card.badge) {
      const badge = document.createElement('span');
      badge.className = 'badge';
      const tone = TONES[String(card.badgeKind ?? '')] || resolveColor(card.badgeKind);
      if (tone) badge.style.setProperty('--k-badge', tone);
      const ico = iconSvg(card.badgeIcon);
      if (ico) badge.appendChild(ico);
      const text = document.createElement('span');
      text.textContent = String(card.badge);
      badge.appendChild(text);
      head.appendChild(badge);
    } else {
      head.appendChild(document.createElement('span'));
    }

    if (Array.isArray(card.menu) && card.menu.length) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'card-menu-btn';
      btn.dataset.menu = card.id;
      btn.setAttribute('aria-label', this._labels.cardMenu);
      btn.title = this._labels.cardMenu;
      // The icon sprite has no ellipsis symbol, so the glyph is the trigger
      // (same choice as the SDK table row-actions menu).
      btn.textContent = '⋯';
      head.appendChild(btn);
    }
    el.appendChild(head);

    const title = document.createElement('h4');
    title.className = 'card-title';
    title.textContent = String(card.title ?? '');
    el.appendChild(title);

    const meta = document.createElement('div');
    meta.className = 'card-meta';
    this._appendBits(meta, card.meta);
    if (meta.childElementCount) el.appendChild(meta);

    const footer = card.footer || null;
    if (footer && (footer.left || footer.right)) {
      const foot = document.createElement('div');
      foot.className = 'card-foot';
      const leftSide = document.createElement('div');
      leftSide.className = 'foot-side';
      this._appendBits(leftSide, footer.left);
      const rightSide = document.createElement('div');
      rightSide.className = 'foot-side';
      this._appendBits(rightSide, footer.right, true);
      foot.append(leftSide, rightSide);
      el.appendChild(foot);
    }
  }

  // Renders a string | {icon,text,tone,avatar} | array of those into `host`.
  _appendBits(host, value, rightSide = false) {
    if (value === null || value === undefined || value === '') return;
    const list = Array.isArray(value) ? value : [value];
    for (const raw of list) {
      if (raw === null || raw === undefined || raw === '') continue;
      const item = typeof raw === 'object' ? raw : { text: raw };
      if (item.avatar || (rightSide && item.kind === 'avatar')) {
        const av = document.createElement('span');
        av.className = 'avatar';
        av.textContent = String(item.text ?? item.avatar ?? '');
        if (item.title) av.title = String(item.title);
        host.appendChild(av);
        continue;
      }
      const bit = document.createElement('span');
      bit.className = 'bit';
      const tone = TONES[String(item.tone ?? '')] || resolveColor(item.tone);
      if (tone) {
        bit.style.setProperty('--k-bit', tone);
        if (!item.icon) bit.classList.add('toned');
      }
      const ico = iconSvg(item.icon);
      if (ico) bit.appendChild(ico);
      else if (tone) {
        const dot = document.createElement('span');
        dot.className = 'dot';
        bit.appendChild(dot);
      }
      const text = document.createElement('span');
      text.textContent = String(item.text ?? '');
      bit.appendChild(text);
      if (item.title) bit.title = String(item.title);
      host.appendChild(bit);
    }
  }

  // Column counters, WIP-limit warning and empty placeholders.
  _updateColumnStates() {
    const emptyText = this.getAttribute('empty-text') || this._labels.empty;
    this._colEls.forEach((col, id) => {
      const cards = this._visibleCards(col.body);
      const n = cards.length;
      const limit = this._columnLimit(id);
      col.count.textContent = limit
        ? fmt(this._labels.limitLabel, { n, limit })
        : fmt(this._labels.countLabel, { n });
      const over = !!limit && n > limit;
      col.root.classList.toggle('over-limit', over);
      col.count.title = over ? fmt(this._labels.limitExceeded, { n, limit }) : '';
      col.empty.textContent = emptyText;
      col.empty.hidden = n > 0 || !!this._drag;
      col.root.setAttribute('aria-label', `${col.label}, ${fmt(this._labels.cardsLabel, { n })}`);
      col.body.setAttribute('aria-label', col.label);
    });
  }

  _columnLimit(columnId) {
    const col = this._columns.find((c) => c.id === columnId);
    const limit = col ? Number(col.limit) : NaN;
    return Number.isFinite(limit) && limit > 0 ? limit : 0;
  }

  // Cards currently laid out in a column body (dragged source excluded).
  _visibleCards(body) {
    return Array.from(body.children).filter(
      (el) => el.classList.contains('card') && !el.classList.contains('is-dragging'),
    );
  }

  // Rebuilds the model order from the DOM after a move so `cards` mirrors the UI.
  _syncOrderFromDom() {
    const out = [];
    const seen = new Set();
    this._colEls.forEach((col) => {
      for (const el of col.body.children) {
        if (!el.classList.contains('card')) continue;
        const card = this._byId.get(el.dataset.id);
        if (card && !seen.has(card.id)) { out.push(card); seen.add(card.id); }
      }
    });
    for (const card of this._cards) {
      if (!seen.has(card.id)) { out.push(card); seen.add(card.id); }
    }
    this._cards = out;
  }

  // Places a card at `index` of `columnId` (model + DOM). Returns true on success.
  _applyMove(cardId, columnId, index) {
    const el = this._cardEls.get(cardId);
    const card = this._byId.get(cardId);
    const col = this._colEls.get(columnId);
    if (!el || !card || !col) return false;
    const siblings = this._visibleCards(col.body).filter((n) => n !== el);
    const before = siblings[index] || col.empty;
    col.body.insertBefore(el, before);
    card.column = columnId;
    this._syncOrderFromDom();
    this._updateColumnStates();
    return true;
  }

  _cardPosition(cardId) {
    const el = this._cardEls.get(cardId);
    if (!el || !el.parentElement) return null;
    const columnId = el.parentElement.dataset.col;
    const index = this._visibleCards(el.parentElement).indexOf(el);
    return { columnId, index };
  }

  _emit(type, detail) {
    this.dispatchEvent(new CustomEvent(type, { detail, bubbles: true, composed: true }));
  }

  _announce(text) {
    if (!this._live) return;
    // Re-setting identical text would not re-trigger the live region.
    this._live.textContent = '';
    this._live.textContent = text;
  }

  // --------------------------------------------------------------- pointer

  _onPointerDown(e) {
    if (e.button !== undefined && e.button !== 0) return;
    const path = e.composedPath();
    if (path.some((n) => n instanceof HTMLElement && (n.classList?.contains('card-menu-btn') || n.classList?.contains('col-add')))) return;
    if (path.some((n) => n instanceof HTMLElement && n.tagName === 'TF-MENU')) return;
    const el = path.find((n) => n instanceof HTMLElement && n.classList?.contains('card'));
    if (!el || el.classList.contains('ghost')) return;
    const card = this._byId.get(el.dataset.id);
    if (!card || card.disabled || this.readOnly) return;
    if (this._grab) return;

    this._drag = {
      pointerId: e.pointerId,
      el,
      cardId: card.id,
      startX: e.clientX,
      startY: e.clientY,
      x: e.clientX,
      y: e.clientY,
      started: false,
      from: el.parentElement.dataset.col,
      fromIndex: this._visibleCards(el.parentElement).indexOf(el),
      raf: 0,
    };
    window.addEventListener('pointermove', this._onPointerMove);
    window.addEventListener('pointerup', this._onPointerUp);
    window.addEventListener('pointercancel', this._onPointerCancel);
    window.addEventListener('keydown', this._onDragKey, true);
  }

  _onPointerMove(e) {
    const d = this._drag;
    if (!d || e.pointerId !== d.pointerId) return;
    d.x = e.clientX;
    d.y = e.clientY;
    if (!d.started) {
      if (Math.abs(d.x - d.startX) < DRAG_THRESHOLD && Math.abs(d.y - d.startY) < DRAG_THRESHOLD) return;
      this._startDrag(e);
    }
    e.preventDefault();
    if (!d.raf) d.raf = requestAnimationFrame(this._tick);
  }

  _startDrag(e) {
    const d = this._drag;
    const rect = d.el.getBoundingClientRect();
    d.grabX = d.startX - rect.left;
    d.grabY = d.startY - rect.top;
    d.started = true;

    // Capture on the board (not the card): the source card is hidden during the
    // drag and a hidden capture target makes some engines drop the capture.
    try { this._board.setPointerCapture(e.pointerId); } catch (_) { /* ignored */ }

    const ghost = d.el.cloneNode(true);
    ghost.classList.add('ghost');
    ghost.classList.remove('is-dragging');
    ghost.removeAttribute('tabindex');
    ghost.setAttribute('aria-hidden', 'true');
    ghost.style.width = `${rect.width}px`;
    ghost.style.height = `${rect.height}px`;
    this._dragLayer.appendChild(ghost);
    d.ghost = ghost;

    const placeholder = document.createElement('div');
    placeholder.className = 'placeholder';
    placeholder.style.height = `${rect.height}px`;
    d.placeholder = placeholder;
    d.el.parentElement.insertBefore(placeholder, d.el);
    d.el.classList.add('is-dragging');
    d.target = { column: d.from, index: d.fromIndex };

    this._board.classList.add('dragging');
    this._colEls.get(d.from)?.root.classList.add('drop-target');
    this._rectCache = null;
    this._updateColumnStates();
    this._moveGhost();
  }

  _tick() {
    const d = this._drag;
    if (!d || !d.started) return;
    d.raf = 0;
    this._moveGhost();
    this._updateDropTarget();
    if (this._autoScroll(d.x, d.y)) {
      // Keep ticking while the board is scrolling under a still pointer.
      d.raf = requestAnimationFrame(this._tick);
    }
  }

  _moveGhost() {
    const d = this._drag;
    if (!d || !d.ghost) return;
    const x = d.x - d.grabX;
    const y = d.y - d.grabY;
    d.ghost.style.transform = `translate3d(${Math.round(x)}px, ${Math.round(y)}px, 0) rotate(1.4deg)`;
  }

  _updateDropTarget() {
    const d = this._drag;
    const colRects = [];
    this._colEls.forEach((col, id) => {
      const r = col.root.getBoundingClientRect();
      colRects.push({ id, left: r.left, right: r.right });
    });
    const columnId = columnAtPoint(colRects, d.x);
    if (!columnId) return;
    const col = this._colEls.get(columnId);
    if (!col) return;

    const rects = this._cardRects(columnId, col);
    const index = insertIndexAt(rects, d.y);
    if (d.target && d.target.column === columnId && d.target.index === index) return;

    if (d.target && d.target.column !== columnId) {
      this._colEls.get(d.target.column)?.root.classList.remove('drop-target');
      col.root.classList.add('drop-target');
    }
    d.target = { column: columnId, index };

    const cards = this._visibleCards(col.body);
    const before = cards[index] || col.empty;
    col.body.insertBefore(d.placeholder, before);
    // Inserting the placeholder shifts every following card.
    this._rectCache = null;

    const limit = this._columnLimit(columnId);
    const incoming = columnId === d.from ? 0 : 1;
    d.placeholder.classList.toggle('warn', !!limit && cards.length + incoming > limit);
  }

  // Card rects of one column, cached until the layout that produced them moves
  // (scroll or placeholder re-insert). Without this a 50-card column would be
  // measured on every animation frame.
  _cardRects(columnId, col) {
    const key = `${columnId}:${col.body.scrollTop}:${this._board.scrollLeft}:${this._board.scrollTop}`;
    if (this._rectCache && this._rectCache.key === key) return this._rectCache.rects;
    const rects = this._visibleCards(col.body).map((el) => {
      const r = el.getBoundingClientRect();
      return { top: r.top, bottom: r.bottom };
    });
    this._rectCache = { key, rects };
    return rects;
  }

  // Scrolls the board horizontally and the hovered column vertically when the
  // pointer sits near an edge. Returns true while it is actually scrolling.
  _autoScroll(x, y) {
    let scrolling = false;
    const boardRect = this._board.getBoundingClientRect();
    if (this._board.scrollWidth > this._board.clientWidth + 1) {
      const dx = this._edgeDelta(x, boardRect.left, boardRect.right);
      if (dx) {
        const before = this._board.scrollLeft;
        this._board.scrollLeft += dx;
        scrolling = scrolling || this._board.scrollLeft !== before;
      }
    }
    const target = this._drag?.target?.column;
    const col = target ? this._colEls.get(target) : null;
    if (col && col.body.scrollHeight > col.body.clientHeight + 1) {
      const r = col.body.getBoundingClientRect();
      const dy = this._edgeDelta(y, r.top, r.bottom);
      if (dy) {
        const before = col.body.scrollTop;
        col.body.scrollTop += dy;
        scrolling = scrolling || col.body.scrollTop !== before;
      }
    }
    if (scrolling) this._rectCache = null;
    return scrolling;
  }

  _edgeDelta(pos, min, max) {
    if (pos < min + EDGE_ZONE) {
      const depth = Math.min(EDGE_ZONE, min + EDGE_ZONE - pos);
      return -Math.ceil((depth / EDGE_ZONE) * EDGE_SPEED);
    }
    if (pos > max - EDGE_ZONE) {
      const depth = Math.min(EDGE_ZONE, pos - (max - EDGE_ZONE));
      return Math.ceil((depth / EDGE_ZONE) * EDGE_SPEED);
    }
    return 0;
  }

  _onPointerUp(e) {
    const d = this._drag;
    if (!d || e.pointerId !== d.pointerId) return;
    if (!d.started) { this._endPointerSession(); return; }

    const body = d.placeholder.parentElement;
    const children = Array.from(body.children);
    const index = children
      .slice(0, children.indexOf(d.placeholder))
      .filter((el) => el.classList.contains('card') && !el.classList.contains('is-dragging'))
      .length;
    const columnId = body.dataset.col;

    this._teardownDrag();
    const from = d.from;
    const fromIndex = d.fromIndex;
    this._endPointerSession();

    // A drop on the original slot is not a move — stay silent.
    if (columnId === from && index === fromIndex) {
      this._updateColumnStates();
      return;
    }
    this._applyMove(d.cardId, columnId, index);
    this._lastMove = { cardId: d.cardId, from, fromIndex, to: columnId, toIndex: index };
    this._emit('card-move', { cardId: d.cardId, from, to: columnId, index });
  }

  _onPointerCancel(e) {
    if (!this._drag || e.pointerId !== this._drag.pointerId) return;
    this._cancelDrag();
  }

  _onDragKey(e) {
    if (!this._drag || e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    this._cancelDrag();
    this._announce(this._labels.cancelled);
  }

  _cancelDrag() {
    const d = this._drag;
    if (!d) return;
    if (d.started) {
      this._teardownDrag();
      // Return the card to the exact slot it was picked up from.
      this._applyMove(d.cardId, d.from, d.fromIndex);
    }
    this._endPointerSession();
  }

  // Removes ghost/placeholder and restores the source card (shared by drop+cancel).
  _teardownDrag() {
    const d = this._drag;
    if (!d || !d.started) return;
    d.ghost?.remove();
    d.placeholder?.remove();
    d.el.classList.remove('is-dragging');
    this._board.classList.remove('dragging');
    this._colEls.forEach((col) => col.root.classList.remove('drop-target'));
    this._suppressClick = true;
    setTimeout(() => { this._suppressClick = false; }, 0);
  }

  _endPointerSession() {
    const d = this._drag;
    if (d) {
      if (d.raf) cancelAnimationFrame(d.raf);
      try { this._board.releasePointerCapture(d.pointerId); } catch (_) { /* ignored */ }
    }
    this._drag = null;
    this._rectCache = null;
    window.removeEventListener('pointermove', this._onPointerMove);
    window.removeEventListener('pointerup', this._onPointerUp);
    window.removeEventListener('pointercancel', this._onPointerCancel);
    window.removeEventListener('keydown', this._onDragKey, true);
    this._updateColumnStates();
  }

  // ------------------------------------------------------------------ click

  _onClick(e) {
    const path = e.composedPath();
    const add = path.find((n) => n instanceof HTMLElement && n.classList?.contains('col-add'));
    if (add) {
      this._emit('column-add', { columnId: add.dataset.add });
      return;
    }
    const menuBtn = path.find((n) => n instanceof HTMLElement && n.classList?.contains('card-menu-btn'));
    if (menuBtn) {
      e.stopPropagation();
      this._toggleCardMenu(menuBtn.dataset.menu, menuBtn);
      return;
    }
    if (this._suppressClick || this._drag?.started) return;
    const el = path.find((n) => n instanceof HTMLElement && n.classList?.contains('card'));
    if (el && el.dataset.id) this._emit('card-open', { cardId: el.dataset.id });
  }

  _toggleCardMenu(cardId, btn) {
    // The document-level outside-click handler of tf-menu closes the popup on
    // pointerdown, i.e. before this click — without the guard the same click
    // would immediately reopen it.
    if (this._menuClosedFor === cardId && performance.now() - this._menuClosedAt < MENU_REOPEN_GUARD_MS) {
      this._menuClosedFor = null;
      return;
    }
    if (this._menu && this._menu.cardId === cardId) { this._closeMenu(); return; }
    this._closeMenu();

    const card = this._byId.get(cardId);
    if (!card || !Array.isArray(card.menu) || !card.menu.length) return;

    const menu = document.createElement('tf-menu');
    menu.setAttribute('placement', 'bottom-end');
    for (const item of card.menu) {
      if (!item || item.id === undefined) continue;
      const mi = document.createElement('tf-menu-item');
      mi.setAttribute('action', String(item.id));
      mi.setAttribute('label', String(item.label ?? item.id));
      if (item.icon && ICON_RE.test(String(item.icon))) mi.setAttribute('icon', String(item.icon));
      if (item.danger) mi.setAttribute('danger', '');
      if (item.disabled) mi.setAttribute('disabled', '');
      mi.textContent = String(item.label ?? item.id);
      menu.appendChild(mi);
    }

    const r = btn.getBoundingClientRect();
    // The popup flows from the layer origin, so the layer is the anchor point.
    this._menuLayer.style.left = `${Math.max(8, Math.min(r.right - 190, window.innerWidth - 200))}px`;
    this._menuLayer.style.top = `${r.bottom + 4}px`;
    this._menuLayer.appendChild(menu);

    menu.addEventListener('action', (ev) => {
      ev.stopPropagation();
      // tf-menu closes itself before dispatching "action", so the guard armed by
      // that close has to be dropped — a selection is not an outside click.
      this._menuClosedFor = null;
      this._emit('card-menu', { cardId, actionId: ev.detail?.action });
      this._closeMenu();
      btn.focus();
    });
    this._menu = { cardId, el: menu };
    menu.open();
    // Attached after open() so the initial "closed" state of tf-menu is ignored.
    // This path means tf-menu closed itself (outside pointerdown or Escape),
    // which is exactly when the reopen guard has to be armed.
    menu.addEventListener('close', () => {
      if (this._menu && this._menu.el === menu) this._closeMenu(true);
    });
  }

  _closeMenu(armGuard = false) {
    if (!this._menu) return;
    this._menuClosedFor = armGuard ? this._menu.cardId : null;
    this._menuClosedAt = performance.now();
    this._menu.el.remove();
    this._menu = null;
  }

  // --------------------------------------------------------------- keyboard

  _onKeyDown(e) {
    const path = e.composedPath();
    // The kebab button keeps its own keyboard semantics (Enter/Space = open menu).
    if (path.some((n) => n instanceof HTMLElement && n.classList?.contains('card-menu-btn'))) return;
    const el = path.find((n) => n instanceof HTMLElement && n.classList?.contains('card'));
    if (!el || !el.dataset.id) return;
    const cardId = el.dataset.id;

    if (e.key === 'Enter') {
      e.preventDefault();
      this._emit('card-open', { cardId });
      return;
    }
    if (e.key === ' ' || e.key === 'Spacebar') {
      e.preventDefault();
      if (this._grab) this._dropGrab();
      else this._startGrab(cardId, el);
      return;
    }
    if (e.key === 'Escape' && this._grab) {
      e.preventDefault();
      this._cancelGrab(true);
      return;
    }
    const dir = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down' }[e.key];
    if (!dir) return;
    e.preventDefault();
    if (this._grab) this._moveGrab(dir);
    else this._moveFocus(el, dir);
  }

  _startGrab(cardId, el) {
    const card = this._byId.get(cardId);
    if (!card || card.disabled || this.readOnly) return;
    const pos = this._cardPosition(cardId);
    if (!pos) return;
    this._grab = { cardId, el, from: pos.columnId, fromIndex: pos.index };
    el.classList.add('is-grabbed');
    el.setAttribute('aria-grabbed', 'true');
    this._announceGrabState(this._labels.grabbed, card, pos);
  }

  _moveGrab(dir) {
    const g = this._grab;
    const pos = this._cardPosition(g.cardId);
    if (!pos) return;
    let { columnId, index } = pos;
    if (dir === 'up' || dir === 'down') {
      index = Math.max(0, index + (dir === 'up' ? -1 : 1));
    } else {
      const order = this._columns.map((c) => c.id);
      const at = order.indexOf(columnId);
      const next = at + (dir === 'left' ? -1 : 1);
      if (next < 0 || next >= order.length) return;
      columnId = order[next];
      const col = this._colEls.get(columnId);
      index = Math.min(index, this._visibleCards(col.body).length);
    }
    if (!this._applyMove(g.cardId, columnId, index)) return;
    g.el.focus();
    g.el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    const after = this._cardPosition(g.cardId);
    this._announceGrabState(this._labels.moved, this._byId.get(g.cardId), after);
  }

  _dropGrab() {
    const g = this._grab;
    const pos = this._cardPosition(g.cardId);
    this._clearGrabVisuals();
    this._grab = null;
    if (!pos) return;
    if (pos.columnId === g.from && pos.index === g.fromIndex) return;
    this._lastMove = { cardId: g.cardId, from: g.from, fromIndex: g.fromIndex, to: pos.columnId, toIndex: pos.index };
    this._emit('card-move', { cardId: g.cardId, from: g.from, to: pos.columnId, index: pos.index });
    const col = this._colEls.get(pos.columnId);
    this._announce(fmt(this._labels.dropped, {
      title: this._byId.get(g.cardId)?.title ?? g.cardId,
      column: col?.label ?? pos.columnId,
      index: pos.index + 1,
    }));
  }

  _cancelGrab(announce) {
    const g = this._grab;
    if (!g) return;
    this._clearGrabVisuals();
    this._grab = null;
    this._applyMove(g.cardId, g.from, g.fromIndex);
    g.el.focus();
    if (announce) this._announce(this._labels.cancelled);
  }

  _clearGrabVisuals() {
    if (!this._grab) return;
    this._grab.el.classList.remove('is-grabbed');
    this._grab.el.removeAttribute('aria-grabbed');
  }

  _announceGrabState(template, card, pos) {
    if (!pos) return;
    const col = this._colEls.get(pos.columnId);
    const count = col ? this._visibleCards(col.body).length : 0;
    this._announce(fmt(template, {
      title: card?.title ?? '',
      column: col?.label ?? pos.columnId,
      index: pos.index + 1,
      count,
    }));
  }

  // Plain focus movement (no card is grabbed): up/down inside a column,
  // left/right to the nearest card of the neighbouring column.
  _moveFocus(el, dir) {
    const body = el.parentElement;
    const cards = this._visibleCards(body);
    const index = cards.indexOf(el);
    if (dir === 'up' || dir === 'down') {
      const next = cards[index + (dir === 'up' ? -1 : 1)];
      if (next) next.focus();
      return;
    }
    const order = this._columns.map((c) => c.id);
    const at = order.indexOf(body.dataset.col);
    const step = dir === 'left' ? -1 : 1;
    for (let i = at + step; i >= 0 && i < order.length; i += step) {
      const col = this._colEls.get(order[i]);
      const list = col ? this._visibleCards(col.body) : [];
      if (list.length) {
        (list[Math.min(index, list.length - 1)] || list[0]).focus();
        return;
      }
    }
  }
}

customElements.define('tf-kanban', TfKanban);
