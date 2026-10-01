// =============================================================================
// File: tf-org-tree.js
// Description: <tf-org-tree> — the organization chart. SVG drawn inside one
//   viewport group, so pan and zoom are a single `transform` write and never
//   touch the markup. Only what is expanded is laid out, and only what is near
//   the viewport is drawn (the slice is redrawn when the view leaves it), which
//   keeps a chart of thousands of people at a few hundred DOM nodes. Zoomed far
//   out the cards collapse to unit tiles.
//   Two views of one model: persons (a card per position, staff to the side of
//   the line, leaf teams stacked) and units (a frame per unit with its head on
//   top and the team in a grid).
//   Data in: `model` from modules/org-structure/tree.js, `labels` (every string
//   and plural the chart shows — the component holds no translations).
//   Events out: `node-select` {id, kind: 'position'|'unit'}, `node-open` (same
//   detail; double click, or Enter on a card with nothing to expand),
//   `present-toggle`, `export-menu` {anchor}.
//   Edit mode (`editing` attribute): every card carries a "⋯" handle that raises
//   `node-menu` {id, kind, rect} (also on right click, the Menu key and Shift+F10),
//   and a card can be dragged onto another one: the host's `dropRule(source, target)`
//   answers {ok, reason?} while the pointer travels (the target is outlined green or
//   red), and the release raises `node-drop` {source, target, verdict}. The chart never
//   changes data itself. `nodeAt(x, y)` and `setDropHover()` let the host drive the same
//   outline for a drag that starts outside the chart (a person from a list).
//   Keyboard: the chart is one ARIA tree with `aria-activedescendant`; arrows
//   walk parent / first child / siblings, Enter expands or collapses, Space
//   selects, + / - / 0 zoom.
// =============================================================================

import { layoutForest, swapSizes, transposeLayout } from '/js/modules/org-structure/layout.js';
import {
  LOD_ZOOM, buildEdges, cardFit, cardSvg, esc, exportStyle, frameSize, frameSvg, functionalEdge, lodSvg,
  metricsFor, morePillSvg, tileWidthFor,
} from '/js/modules/org-structure/render.js';

const DEFAULT_EXPAND_DEPTH = 3;
const MIN_ZOOM = 0.02;
const MAX_ZOOM = 2.5;
const PRESENT_SCALE = 1.3;
const DRAG_SLOP = 4;
// The edit handle takes this much of a card's width, so text is wrapped before it.
const EDIT_HANDLE = 24;
const CULL_MARGIN = 0.6;
const MAX_WHEEL_STEP = 1.25;
const PILL_ROOM = 44;
// Teams stack in columns this many rows tall; a shorter column is tried before text gets smaller.
const STACK_ROWS = [8, 6, 4, 3, 2];
// Unit frames: grid columns and the members listed before "+N", from roomy to tight.
const UNIT_DETAIL = [
  { cols: 4, max: 24 }, { cols: 6, max: 24 }, { cols: 3, max: 24 }, { cols: 6, max: 12 },
  { cols: 3, max: 12 }, { cols: 2, max: 8 }, { cols: 2, max: 4 }, { cols: 1, max: 2 },
  // Head and numbers only: the header line already counts the team.
  { cols: 1, max: 0 },
];
const FIT_SIDE = 32;
const FIT_TOP = 64;
// The hint pill sits along the bottom edge, so the fit keeps that band free too.
const FIT_BOTTOM = 56;
// Below these zooms the card text drops under 9 px (name) / 8 px (role): the fit never goes there.
// Presentation cards are 1.3x larger, so the same floor is reached at a lower zoom.
const READABLE_ZOOM = 0.77;
const READABLE_ZOOM_PRESENT = 0.74;
// Unit tiles carry smaller type (11 / 9.5 px at 100%), so their floor is higher.
const READABLE_ZOOM_UNITS = 0.85;
const READABLE_ZOOM_UNITS_PRESENT = 0.7;
let instanceCounter = 0;

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

function intersects(item, r) {
  return item.x < r.x1 && item.x + item.w > r.x0 && item.y < r.y1 && item.y + item.h > r.y0;
}

function prefersReducedMotion() {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

const TOOLS = [
  { id: 'zoom-out', icon: 'zoom-out', label: 'zoomOut' },
  { id: 'zoom-reset', icon: null, label: 'zoomReset' },
  { id: 'zoom-in', icon: 'zoom-in', label: 'zoomIn' },
  { id: 'fit', icon: 'fit', label: 'fit' },
  { id: 'present', icon: 'present', label: 'present' },
  { id: 'export', icon: 'download', label: 'export' },
];

class TfOrgTree extends HTMLElement {
  static get observedAttributes() {
    return ['mode', 'functional', 'presentation', 'orientation', 'editing'];
  }

  constructor() {
    super();
    this._uid = `ot${(instanceCounter += 1)}`;
    this._model = { nodes: [], units: [], functional: [], meIds: [] };
    this._nodeById = new Map();
    this._unitById = new Map();
    this._labels = null;
    this._expanded = new Map();
    // Ancestors opened to show the current path or focus; they close again when the path is cleared.
    this._pathOpen = new Set();
    this._expandDepth = DEFAULT_EXPAND_DEPTH;
    this._stackRows = STACK_ROWS[0];
    this._unitDetail = 0;
    this._trim = false;
    this._selectedId = null;
    this._activeId = null;
    this._pathSet = new Set();
    this._matchSet = new Set();
    this._layout = null;
    this._edges = [];
    this._roots = [];
    this._view = { x: 0, y: 0, k: 1 };
    this._size = { w: 800, h: 600 };
    this._fitted = false;
    this._pristine = true;
    this._fallback = false;
    this._focusId = null;
    this._rendered = null;
    this._renderedLod = false;
    this._raf = 0;
    this._anim = 0;
    this._pointers = new Map();
    this._drag = null;
    this._pinch = null;
    this._built = false;
    this._resize = null;
    this._refitOnResize = false;
    this._cardDrag = null;
    this._hover = null;
    this.dropRule = null;
  }

  // -------------------------------------------------------------------------
  // Lifecycle and properties
  // -------------------------------------------------------------------------

  connectedCallback() {
    if (!this._built) this._build();
    if (typeof ResizeObserver !== 'undefined' && !this._resize) {
      this._resize = new ResizeObserver(() => this._onResize());
      this._resize.observe(this._box);
    }
    this._onResize();
  }

  disconnectedCallback() {
    this._resize?.disconnect();
    this._resize = null;
    cancelAnimationFrame(this._raf);
    cancelAnimationFrame(this._anim);
    this._raf = 0;
    this._anim = 0;
    this._cancelCardDrag();
  }

  attributeChangedCallback(name, oldValue, newValue) {
    if (oldValue === newValue || !this._built) return;
    if (name === 'presentation') {
      // Going fullscreen resizes the chart a moment later; the fit has to use the new size.
      this._refitOnResize = true;
      this._relayout({ refit: true });
    } else if (name === 'mode' || name === 'orientation') this._relayout({ refit: true });
    else if (name === 'editing') {
      // The handle changes card widths, so the layout is redone; the view stays where the user left it.
      this._cancelCardDrag();
      this._relayout({ keepView: true });
    } else this._scheduleRender();
  }

  get mode() { return this.getAttribute('mode') === 'units' ? 'units' : 'persons'; }
  set mode(v) { this.setAttribute('mode', v === 'units' ? 'units' : 'persons'); }

  get horizontal() { return this.getAttribute('orientation') === 'horizontal'; }
  set horizontal(v) { this.setAttribute('orientation', v ? 'horizontal' : 'vertical'); }

  get functional() { return this.hasAttribute('functional'); }
  set functional(v) { this.toggleAttribute('functional', Boolean(v)); }

  get presentation() { return this.hasAttribute('presentation'); }
  set presentation(v) { this.toggleAttribute('presentation', Boolean(v)); }

  get editing() { return this.hasAttribute('editing'); }
  set editing(v) { this.toggleAttribute('editing', Boolean(v)); }

  get model() { return this._model; }
  set model(model) {
    this._adopt(model);
    this._expanded.clear();
    this._pathOpen.clear();
    this._expandDepth = DEFAULT_EXPAND_DEPTH;
    this._selectedId = null;
    this._activeId = null;
    this._pathSet = new Set();
    this._matchSet = new Set();
    this._fitted = false;
    if (this._built) this._relayout({ refit: true });
  }

  _adopt(model) {
    this._model = model ?? { nodes: [], units: [], functional: [], meIds: [] };
    this._nodeById = new Map(this._model.nodes.map((n) => [n.id, n]));
    this._unitById = new Map(this._model.units.map((u) => [u.id, u]));
  }

  /**
   * Swaps in fresh data and keeps what the user built up: expansion, selection, and the view
   * (unless the chart is still on its opening fit, which is then redone for the new size of the tree).
   */
  updateModel(model) {
    this._adopt(model);
    const alive = (id) => this._nodeById.has(id) || (String(id).startsWith('unit:') && this._unitById.has(String(id).slice(5)));
    for (const id of [...this._expanded.keys()]) if (!alive(id)) this._expanded.delete(id);
    if (this._selectedId && !alive(this._selectedId)) this._selectedId = null;
    if (this._activeId && !alive(this._activeId)) this._activeId = null;
    this._pathSet = new Set([...this._pathSet].filter(alive));
    this._matchSet = new Set([...this._matchSet].filter(alive));
    if (!this._built) return;
    this._relayout({ keepView: true });
    if (this._pristine) this._initialView();
  }

  get labels() { return this._labels; }
  set labels(labels) {
    this._labels = labels;
    if (!this._built) return;
    this._applyLabels();
    // Card sizes come from the text, so new labels (another language) mean a new layout.
    if (this._layout) this._relayout({ keepView: true });
  }

  get selectedId() { return this._selectedId; }
  set selectedId(id) {
    this._selectedId = id || null;
    this._scheduleRender();
  }

  /** Ids highlighted as the path from the root; their ancestors are expanded so the whole path shows. */
  set pathIds(ids) {
    const list = Array.from(ids ?? []);
    this._pathSet = new Set(list);
    this._pathOpen = new Set();
    this._trim = false;
    if (this.mode === 'units') {
      for (const id of list) {
        for (let u = this._unitById.get(this._nodeById.get(id)?.unitId); u; u = u.parentId ? this._unitById.get(u.parentId) : null) {
          this._pathSet.add(`unit:${u.id}`);
          if (u.parentId) this._pathOpen.add(`unit:${u.parentId}`);
        }
      }
    } else {
      for (const id of list.slice(0, -1)) this._pathOpen.add(id);
    }
    if (this._built && this._layout) {
      this._relayout({ keepView: true });
      if (this._pristine) this._initialView();
    }
  }

  set matchIds(ids) {
    this._matchSet = new Set(ids ?? []);
    this._scheduleRender();
  }

  // -------------------------------------------------------------------------
  // DOM
  // -------------------------------------------------------------------------

  _build() {
    this.innerHTML = `
      <div class="tf-orgtree">
        <svg class="tf-orgtree__svg" role="tree" tabindex="0" xmlns="http://www.w3.org/2000/svg">
          <g class="tf-orgtree__viewport">
            <g class="ot-lines"></g>
            <g class="ot-functional"></g>
            <g class="ot-cards"></g>
          </g>
        </svg>
        <div class="tf-orgtree__tools" role="toolbar"></div>
        <div class="tf-orgtree__minimap" aria-hidden="true">
          <svg preserveAspectRatio="xMidYMid meet"><g class="ot-mini-items"></g><rect class="ot-mini-view"/></svg>
        </div>
        <div class="tf-orgtree__hint"></div>
      </div>`;
    this._box = this.querySelector('.tf-orgtree');
    this._svg = this.querySelector('.tf-orgtree__svg');
    this._viewport = this.querySelector('.tf-orgtree__viewport');
    this._linesLayer = this.querySelector('.ot-lines');
    this._functionalLayer = this.querySelector('.ot-functional');
    this._cardsLayer = this.querySelector('.ot-cards');
    this._tools = this.querySelector('.tf-orgtree__tools');
    this._minimap = this.querySelector('.tf-orgtree__minimap');
    this._miniItems = this.querySelector('.ot-mini-items');
    this._miniView = this.querySelector('.ot-mini-view');
    this._hint = this.querySelector('.tf-orgtree__hint');

    this._svg.addEventListener('pointerdown', (e) => this._onPointerDown(e));
    this._svg.addEventListener('pointermove', (e) => this._onPointerMove(e));
    this._svg.addEventListener('pointerup', (e) => this._onPointerUp(e));
    this._svg.addEventListener('pointercancel', (e) => this._onPointerUp(e, true));
    this._svg.addEventListener('wheel', (e) => this._onWheel(e), { passive: false });
    this._svg.addEventListener('dblclick', (e) => this._onDoubleClick(e));
    this._svg.addEventListener('contextmenu', (e) => this._onContextMenu(e));
    this._svg.addEventListener('keydown', (e) => this._onKey(e));
    this._tools.addEventListener('click', (e) => this._onTool(e));
    this._minimap.addEventListener('pointerdown', (e) => this._onMinimap(e));
    this._minimap.addEventListener('pointermove', (e) => { if (e.buttons) this._onMinimap(e); });
    this._built = true;
    this._applyLabels();
    if (this._model.nodes.length) this._relayout({ refit: true });
  }

  // tf-button copies `aria-label` to its inner button once, at build, so the tools are
  // written after the labels are known rather than patched afterwards.
  _applyLabels() {
    const tools = this._labels?.tools ?? {};
    this._tools.innerHTML = TOOLS.map((tool) => {
      const label = esc(tools[tool.label] ?? '');
      const attrs = `variant="ghost" size="sm" data-tool="${tool.id}" aria-label="${label}" title="${label}"`;
      return tool.icon
        ? `<tf-button ${attrs} icon="${tool.icon}"></tf-button>`
        : `<tf-button ${attrs}>${Math.round(this._view.k * 100)}%</tf-button>`;
    }).join('');
    this._zoomLabel = this._tools.querySelector('[data-tool="zoom-reset"]');
    this._svg.setAttribute('aria-label', this._labels?.tree ?? '');
    this._hint.textContent = this._labels?.hint ?? '';
  }

  _onResize() {
    // Layout size, not the on-screen rect: a page-enter animation scales the screen, and the chart
    // must not be fitted to a transient 25 px box.
    const width = this._box?.clientWidth ?? 0;
    const height = this._box?.clientHeight ?? 0;
    const resized = width > 0 && height > 0 && (Math.abs(width - this._size.w) > 1 || Math.abs(height - this._size.h) > 1);
    if (resized) this._size = { w: width, h: height };
    if (!this._layout) return;
    // Until the user moves the chart, the opening view follows the real size: the first layout
    // may run before the element has been laid out at all, and a panel opening beside the chart
    // narrows it.
    if (!this._fitted || (resized && (this._refitOnResize || this._pristine))) {
      this._refitOnResize = false;
      this._initialView();
    } else {
      if (resized) this._keepFocusVisible();
      this._applyView(true);
    }
  }

  _keepFocusVisible() {
    const id = this._selectedId ?? this._activeId;
    const item = id ? this._layout.byId.get(this.mode === 'units' ? this._unitLayoutIdOf(id) : id) : null;
    if (!item) return;
    const v = this._viewportWorld();
    const inside = item.x >= v.x0 && item.y >= v.y0 && item.x + item.w <= v.x1 && item.y + item.h <= v.y1;
    if (!inside) this._centerOn(item, this._view.k);
  }

  // -------------------------------------------------------------------------
  // Forest construction (what is visible, in layout terms)
  // -------------------------------------------------------------------------

  get _scale() { return this.presentation ? PRESENT_SCALE : 1; }

  _isExpanded(id, depth, expandAll) {
    return expandAll || (this._expanded.get(id) ?? (this._pathOpen.has(id) || depth < this._expandDepth));
  }

  /**
   * Builds layout nodes for the persons view. `include` restricts the chart to a subset
   * (export of one unit); everything cut away is counted into the "+N" of its manager.
   */
  _personForest(m, { include = null, expandAll = false, editing = this.editing } = {}) {
    const byId = this._nodeById;
    const visibleKids = (n) => n.childIds.map((id) => byId.get(id)).filter((c) => !include || include(c));
    const isSide = (c) => c.staff && c.childIds.length === 0;
    const allReports = (n) => visibleKids(n).filter((c) => !isSide(c));
    // Trimmed: an ancestor of the shown path lists only its child on the path; the rest is a "+N".
    const trimmed = (n) => this._trim && this._pathOpen.has(n.id);
    const reportsOf = (n) => (trimmed(n) ? allReports(n).filter((c) => this._pathSet.has(c.id) || this._pathOpen.has(c.id) || c.id === this._focusId) : allReports(n));
    const staffOf = (n) => visibleKids(n).filter(isSide);
    // A report joins a stacked column when nothing is laid out under it; a folded one still does
    // (its "+N" pill sits beside the card, in the room `padRight` keeps free).
    const stackCell = (c, depth) => staffOf(c).length === 0 && !(reportsOf(c).length > 0 && this._isExpanded(c.id, depth, expandAll));
    const makeNode = (n, depth, parent, stacked) => {
      const padRight = stacked && reportsOf(n).length > 0 ? PILL_ROOM * m.s : 0;
      const fit = cardFit(n, m, stacked, this._labels ?? { vacancy: '' }, editing ? EDIT_HANDLE * m.s : 0);
      return {
        id: n.id, ref: n, depth, parent, fit, w: fit.w + padRight, padRight, h: fit.h,
        children: [], staff: [], more: 0, expandable: false, expanded: false,
      };
    };
    const roots = this._model.nodes
      .filter((n) => (!include || include(n)) && (!n.parentId || !byId.has(n.parentId) || (include && !include(byId.get(n.parentId)))))
      .map((n) => makeNode(n, 0, null, false));
    const pending = [...roots];
    while (pending.length) {
      const ln = pending.pop();
      const n = ln.ref;
      const normal = reportsOf(n);
      const cut = n.childIds.length - visibleKids(n).length;
      ln.staff = staffOf(n).map((c) => makeNode(c, ln.depth + 1, ln, false));
      ln.expandable = normal.length > 0;
      ln.expanded = ln.expandable && this._isExpanded(n.id, ln.depth, expandAll);
      if (ln.expanded) {
        const stacked = normal.length >= 2 && normal.every((c) => stackCell(c, ln.depth + 1));
        ln.children = normal.map((c) => makeNode(c, ln.depth + 1, ln, stacked));
        for (const child of ln.children) pending.push(child);
      } else {
        ln.more = normal.length;
      }
      ln.more += cut > 0 && include ? cut : 0;
      if (trimmed(n)) ln.more += allReports(n).length - normal.length;
    }
    return roots;
  }

  _unitForest(m, { expandAll = false, editing = this.editing } = {}) {
    const units = this._model.units;
    m.editRoom = editing ? EDIT_HANDLE * m.s : 0;
    m.tileW = tileWidthFor(this._model.nodes, m, this._labels);
    const members = new Map();
    for (const n of this._model.nodes) {
      if (!members.has(n.unitId)) members.set(n.unitId, []);
      members.get(n.unitId).push(n);
    }
    const makeNode = (u, depth, parent) => {
      const list = (members.get(u.id) ?? []).filter((n) => n.id !== u.headId)
        .sort((a, b) => Number(b.childIds.length > 0) - Number(a.childIds.length > 0) || a.name.localeCompare(b.name));
      const unit = { ...u, head: u.headId ? this._nodeById.get(u.headId) : null, members: list };
      const size = frameSize(unit, list.length, m, this._labels);
      return {
        id: `unit:${u.id}`, unit, size, depth, parent, w: size.w, h: size.h,
        children: [], staff: [], more: 0, expandable: false, expanded: false,
      };
    };
    const roots = units.filter((u) => !u.parentId).map((u) => makeNode(u, 0, null));
    const pending = [...roots];
    while (pending.length) {
      const ln = pending.pop();
      const kids = ln.unit.childIds.map((id) => this._unitById.get(id));
      ln.expandable = kids.length > 0;
      ln.expanded = ln.expandable && this._isExpanded(ln.id, ln.depth, expandAll);
      if (ln.expanded) {
        ln.children = kids.map((u) => makeNode(u, ln.depth + 1, ln));
        for (const child of ln.children) pending.push(child);
      } else {
        ln.more = kids.length;
      }
    }
    return roots;
  }

  _computeLayout(options = {}) {
    const m = metricsFor(options.scale ?? this._scale);
    m.gridCols = UNIT_DETAIL[this._unitDetail].cols;
    m.gridMax = UNIT_DETAIL[this._unitDetail].max;
    const persons = (options.mode ?? this.mode) === 'persons';
    const horizontal = this.horizontal;
    const roots = persons ? this._personForest(m, options) : this._unitForest(m, options);
    // Horizontal is the same tidy tree laid out on transposed boxes, then turned back: depth runs
    // along x, and the wider gap between levels leaves room for the "+N" pill beside a card.
    if (horizontal) swapSizes(roots);
    const layout = layoutForest(roots, {
      ...m.layout,
      stackLeaves: persons && !horizontal,
      stackRowsMax: this._stackRows,
      stackColumnsMax: 12,
      vgap: m.layout.vgap * (horizontal ? 1.7 : 1),
    });
    if (horizontal) {
      swapSizes(roots);
      transposeLayout(layout);
    }
    return { m, roots, layout, edges: buildEdges(layout, m.s, horizontal) };
  }

  _layoutNow() {
    const { m, roots, layout, edges } = this._computeLayout();
    this._m = m;
    this._roots = roots;
    this._layout = layout;
    this._edges = edges;
  }

  _relayout({ refit = false, keepView = false } = {}) {
    const anchorId = keepView ? this._anchorId : null;
    const before = anchorId ? this._layout?.byId.get(anchorId) : null;
    this._layoutNow();
    this._buildMinimap();
    if (before) {
      const after = this._layout.byId.get(anchorId);
      if (after) {
        this._view.x += (before.x - after.x) * this._view.k;
        this._view.y += (before.y - after.y) * this._view.k;
      }
    }
    this._anchorId = null;
    if (refit || !this._fitted) this._initialView();
    else this._applyView(true);
  }

  // -------------------------------------------------------------------------
  // View: zoom, pan, fit
  // -------------------------------------------------------------------------

  // Deeper is preferred over wrapped: levels first, and within a level the tallest columns that still fit
  // (for units: the roomiest frame grid). Failing that, a path is shown alone, its siblings behind "+N".
  _chooseShape() {
    const search = () => {
      const variants = this.mode === 'units' ? UNIT_DETAIL.map((_, i) => i) : STACK_ROWS;
      for (let depth = DEFAULT_EXPAND_DEPTH; depth >= 1; depth -= 1) {
        this._expandDepth = depth;
        for (const variant of variants) {
          if (this.mode === 'units') this._unitDetail = variant;
          else this._stackRows = variant;
          this._layoutNow();
          if (this._fitZoom() >= this._floorZoom) return true;
          if (this.horizontal && this.mode !== 'units') break;
        }
      }
      return false;
    };
    this._trim = false;
    if (search()) return;
    if (this._pathOpen.size > 0) {
      this._trim = true;
      if (search()) return;
      this._trim = false;
    }
    // Nothing fits at the floor: the shallowest, most compact shape is the one that stays.
    this._expandDepth = 1;
    this._stackRows = STACK_ROWS[0];
    this._unitDetail = UNIT_DETAIL.length - 1;
    this._layoutNow();
  }

  get _floorZoom() {
    if (this.mode === 'units') return this.presentation ? READABLE_ZOOM_UNITS_PRESENT : READABLE_ZOOM_UNITS;
    return this.presentation ? READABLE_ZOOM_PRESENT : READABLE_ZOOM;
  }

  // The automatic view: the WHOLE laid-out tree inside the chart, text at or above the readable
  // floor. When the tree would have to shrink below the floor, the default expansion gives way
  // level by level (the rest stays behind "+N"; what the user or a search path expanded stays).
  // Should even one level be too much, the chart keeps the floor and centres on the card that
  // matters (selection, search hit); only then is anything off screen, and the minimap appears.
  _initialView() {
    if (!this._layout) return;
    this._chooseShape();
    this._buildMinimap();
    if (!this._layout.items.length) return;
    this._fallback = false;
    const focus = this._fitZoom() < this._floorZoom ? this._layout.byId.get(this._focusLayoutId()) : null;
    if (focus) {
      const k = this._floorZoom;
      this._view = { k, x: this._size.w / 2 - (focus.x + focus.w / 2) * k, y: this._size.h / 2 - (focus.y + focus.h / 2) * k };
      this._fallback = true;
    } else {
      this._view = this._fitView();
    }
    this._fitted = true;
    this._pristine = true;
    this._applyView(true);
  }

  _focusLayoutId() {
    const id = this._selectedId ?? this._focusId ?? this._activeId;
    return id && this.mode === 'units' ? this._unitLayoutIdOf(id) : id;
  }

  _fitZoom() {
    const { width, height } = this._layout;
    return clamp(
      Math.min((this._size.w - FIT_SIDE) / width, (this._size.h - FIT_TOP - FIT_BOTTOM) / height),
      MIN_ZOOM,
      this.presentation ? 1.5 : 1,
    );
  }

  // The top band is kept free for the tools (and, in presentation, the "as of" chip).
  _fitView() {
    const k = this._fitZoom();
    return { k, x: (this._size.w - this._layout.width * k) / 2, y: FIT_TOP };
  }

  fit() {
    if (!this._layout) return;
    cancelAnimationFrame(this._anim);
    this._initialView();
  }

  zoomBy(factor, cx = this._size.w / 2, cy = this._size.h / 2) {
    const k = clamp(this._view.k * factor, MIN_ZOOM, MAX_ZOOM);
    this._goTo({
      k,
      x: cx - (cx - this._view.x) * (k / this._view.k),
      y: cy - (cy - this._view.y) * (k / this._view.k),
    });
  }

  resetZoom() {
    this.zoomBy(1 / this._view.k);
  }

  _goTo(target) {
    this._pristine = false;
    cancelAnimationFrame(this._anim);
    if (prefersReducedMotion() || typeof requestAnimationFrame !== 'function') {
      this._view = target;
      this._applyView();
      return;
    }
    const from = { ...this._view };
    const started = performance.now();
    const step = (now) => {
      const t = clamp((now - started) / 260, 0, 1);
      const e = 1 - (1 - t) ** 3;
      this._view = {
        k: from.k + (target.k - from.k) * e,
        x: from.x + (target.x - from.x) * e,
        y: from.y + (target.y - from.y) * e,
      };
      this._applyView();
      if (t < 1) this._anim = requestAnimationFrame(step);
    };
    this._anim = requestAnimationFrame(step);
  }

  _applyView(forceRender = false) {
    const { x, y, k } = this._view;
    this._viewport.setAttribute('transform', `translate(${x} ${y}) scale(${k})`);
    this._zoomLabel?.setAttribute('label', `${Math.round(k * 100)}%`);
    // The minimap only earns its place once part of the chart is out of sight.
    this._minimap.hidden = this._pristine && !this._fallback;
    this._updateMiniView();
    const lod = k < LOD_ZOOM;
    if (forceRender || lod !== this._renderedLod || !this._rendered || !this._coversViewport()) {
      this._scheduleRender();
    }
  }

  _viewportWorld() {
    const { x, y, k } = this._view;
    return { x0: -x / k, y0: -y / k, x1: (this._size.w - x) / k, y1: (this._size.h - y) / k };
  }

  _coversViewport() {
    const v = this._viewportWorld();
    const r = this._rendered;
    return v.x0 >= r.x0 && v.y0 >= r.y0 && v.x1 <= r.x1 && v.y1 <= r.y1;
  }

  /** World point at the centre of the view, animated; `zoom` raises the zoom to at least that. */
  _centerOn(item, zoom = 0.9) {
    const k = clamp(Math.max(this._view.k, zoom), MIN_ZOOM, MAX_ZOOM);
    this._goTo({
      k,
      x: this._size.w / 2 - (item.x + item.w / 2) * k,
      y: this._size.h / 2 - (item.y + item.h / 2) * k,
    });
  }

  // -------------------------------------------------------------------------
  // Rendering
  // -------------------------------------------------------------------------

  _scheduleRender() {
    if (this._raf || !this._layout) return;
    if (typeof requestAnimationFrame !== 'function') {
      this._render();
      return;
    }
    this._raf = requestAnimationFrame(() => {
      this._raf = 0;
      this._render();
    });
  }

  _renderContext(lod) {
    const m = this._m;
    return {
      s: m.s, zoom: this._view.k, lod, labels: this._labels, uid: this._uid, tileW: m.tileW,
      selectedId: this._selectedId, activeId: this._activeId, pathSet: this._pathSet, matchSet: this._matchSet,
      level: 1, expandable: false, expanded: false, unitSelected: false, horizontal: this.horizontal,
      editing: this.editing,
    };
  }

  _render() {
    if (!this._layout || !this._labels) return;
    const v = this._viewportWorld();
    const mx = (v.x1 - v.x0) * CULL_MARGIN;
    const my = (v.y1 - v.y0) * CULL_MARGIN;
    const rect = { x0: v.x0 - mx, y0: v.y0 - my, x1: v.x1 + mx, y1: v.y1 + my };
    this._rendered = rect;
    const lod = this._view.k < LOD_ZOOM;
    this._renderedLod = lod;
    const ctx = this._renderContext(lod);
    const persons = this.mode === 'persons';

    let cards = '';
    for (const item of this._layout.items) {
      if (!intersects(item, rect)) continue;
      const ln = item.node;
      ctx.level = ln.depth + 1;
      ctx.expandable = ln.expandable;
      ctx.expanded = ln.expanded;
      if (persons) {
        cards += lod ? lodSvg(ln.ref, item, ctx) : cardSvg(ln.ref, item, ctx);
        if (!lod && ln.more > 0) cards += morePillSvg(ln.ref, item, ln.more, ctx);
      } else {
        ctx.unitSelected = this._selectedId === ln.id;
        cards += frameSvg(ln.unit, item, ctx, ln.size);
        if (!lod && ln.more > 0) cards += morePillSvg({ id: ln.id }, item, ln.more, ctx);
      }
    }
    this._cardsLayer.innerHTML = cards;

    let plain = '';
    let highlighted = '';
    let staff = '';
    for (const edge of this._edges) {
      if (edge.x0 > rect.x1 || edge.x1 < rect.x0 || edge.y0 > rect.y1 || edge.y1 < rect.y0) continue;
      if (edge.kind === 'staff') staff += edge.d;
      else if (this._pathSet.has(edge.childId) && this._pathSet.has(edge.parentId)) highlighted += edge.d;
      else plain += edge.d;
    }
    this._linesLayer.innerHTML = `<path d="${plain}"/><path class="staff" d="${staff}"/><path class="hl" d="${highlighted}"/>`;

    this._functionalLayer.innerHTML = persons && this.functional && !lod ? this._functionalMarkup(rect) : '';
    this._syncActive();
  }

  _functionalMarkup(rect) {
    const byId = this._layout.byId;
    let d = '';
    let i = 0;
    for (const link of this._model.functional) {
      const child = byId.get(link.from);
      const parent = byId.get(link.to);
      if (!child || !parent) continue;
      if (Math.max(child.x + child.w, parent.x + parent.w) < rect.x0 || Math.min(child.x, parent.x) > rect.x1
        || Math.max(child.y + child.h, parent.y + parent.h) < rect.y0 || Math.min(child.y, parent.y) > rect.y1) continue;
      d += functionalEdge(child, parent, this._m.s, i, this.horizontal);
      i += 1;
    }
    return d ? `<path d="${d}"/>` : '';
  }

  _syncActive() {
    const el = this._activeId ? this._cardsLayer.querySelector(`[data-node="${this._attr(this._activeId)}"], [data-unit="${this._attr(String(this._activeId).replace(/^unit:/, ''))}"]`) : null;
    if (el?.id) this._svg.setAttribute('aria-activedescendant', el.id);
    else this._svg.removeAttribute('aria-activedescendant');
  }

  _attr(value) {
    return String(value).replace(/["\\]/g, '');
  }

  _buildMinimap() {
    const { items, width, height } = this._layout;
    const svg = this._miniItems.ownerSVGElement;
    svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
    const persons = this.mode === 'persons';
    let rects = '';
    for (const item of items) {
      const color = persons ? item.node.ref.color : item.node.unit.color;
      rects += `<rect x="${Math.round(item.x)}" y="${Math.round(item.y)}" width="${Math.round(item.w)}" height="${Math.round(item.h)}" rx="10" fill="${esc(color)}" fill-opacity="0.55"/>`;
    }
    this._miniItems.innerHTML = rects;
    this._miniStroke = Math.max(width, height) / 90;
    this._miniView.setAttribute('stroke-width', String(this._miniStroke));
  }

  _updateMiniView() {
    if (!this._layout) return;
    const v = this._viewportWorld();
    this._miniView.setAttribute('x', String(v.x0));
    this._miniView.setAttribute('y', String(v.y0));
    this._miniView.setAttribute('width', String(Math.max(1, v.x1 - v.x0)));
    this._miniView.setAttribute('height', String(Math.max(1, v.y1 - v.y0)));
  }

  _onMinimap(e) {
    if (!this._layout) return;
    const box = this._minimap.getBoundingClientRect();
    const { width, height } = this._layout;
    // The minimap letterboxes the chart (xMidYMid meet): convert through the fitted scale.
    const scale = Math.min(box.width / width, box.height / height);
    if (!(scale > 0)) return;
    const ox = (box.width - width * scale) / 2;
    const oy = (box.height - height * scale) / 2;
    const wx = (e.clientX - box.left - ox) / scale;
    const wy = (e.clientY - box.top - oy) / scale;
    cancelAnimationFrame(this._anim);
    this._pristine = false;
    this._view = { ...this._view, x: this._size.w / 2 - wx * this._view.k, y: this._size.h / 2 - wy * this._view.k };
    this._applyView();
    e.preventDefault();
  }

  // -------------------------------------------------------------------------
  // Pointer, wheel, tools
  // -------------------------------------------------------------------------

  _onPointerDown(e) {
    if (e.pointerType === 'mouse' && e.button !== 0) return;
    this._svg.focus({ preventScroll: true });
    this._hint.hidden = true;
    this._pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (this._pointers.size === 1) {
      this._drag = {
        sx: e.clientX, sy: e.clientY, vx: this._view.x, vy: this._view.y, moved: false, target: e.target, card: this._dragCandidate(e.target),
      };
    } else if (this._pointers.size === 2) {
      const [a, b] = [...this._pointers.values()];
      this._pinch = { dist: Math.hypot(a.x - b.x, a.y - b.y), k: this._view.k };
      if (this._drag) this._drag.moved = true;
    }
  }

  _onPointerMove(e) {
    if (!this._pointers.has(e.pointerId)) return;
    this._pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (this._pinch && this._pointers.size >= 2) {
      const [a, b] = [...this._pointers.values()];
      const box = this._svg.getBoundingClientRect();
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      const k = clamp(this._pinch.k * (dist / this._pinch.dist), MIN_ZOOM, MAX_ZOOM);
      const cx = (a.x + b.x) / 2 - box.left;
      const cy = (a.y + b.y) / 2 - box.top;
      this._pristine = false;
      this._view = {
        k,
        x: cx - (cx - this._view.x) * (k / this._view.k),
        y: cy - (cy - this._view.y) * (k / this._view.k),
      };
      this._applyView();
      return;
    }
    const d = this._drag;
    if (!d) return;
    const dx = e.clientX - d.sx;
    const dy = e.clientY - d.sy;
    if (!d.moved && Math.hypot(dx, dy) > DRAG_SLOP) {
      d.moved = true;
      cancelAnimationFrame(this._anim);
      this._svg.setPointerCapture?.(e.pointerId);
      if (d.card) this._beginCardDrag(d.card);
      else this.classList.add('tf-orgtree--dragging');
    }
    if (d.moved && d.card) {
      this._moveCardDrag(e);
    } else if (d.moved) {
      this._pristine = false;
      this._view = { ...this._view, x: d.vx + dx, y: d.vy + dy };
      this._applyView();
    }
  }

  _onPointerUp(e, cancelled = false) {
    this._pointers.delete(e.pointerId);
    if (this._pointers.size < 2) this._pinch = null;
    const d = this._drag;
    if (this._pointers.size === 0) {
      this._drag = null;
      this.classList.remove('tf-orgtree--dragging');
      if (d?.card && d.moved) this._endCardDrag(e, cancelled);
      else if (d && !d.moved && !cancelled) this._click(d.target);
    }
  }

  _onWheel(e) {
    e.preventDefault();
    this._pristine = false;
    const box = this._svg.getBoundingClientRect();
    const unit = e.deltaMode === 1 ? 16 : 1;
    if (e.ctrlKey || e.metaKey) {
      // One notch must not jump the zoom: the step is clamped per event.
      const step = clamp(Math.exp(-e.deltaY * unit * 0.002), 1 / MAX_WHEEL_STEP, MAX_WHEEL_STEP);
      const k = clamp(this._view.k * step, MIN_ZOOM, MAX_ZOOM);
      const cx = e.clientX - box.left;
      const cy = e.clientY - box.top;
      cancelAnimationFrame(this._anim);
      this._view = {
        k,
        x: cx - (cx - this._view.x) * (k / this._view.k),
        y: cy - (cy - this._view.y) * (k / this._view.k),
      };
    } else {
      cancelAnimationFrame(this._anim);
      this._view = { ...this._view, x: this._view.x - e.deltaX * unit, y: this._view.y - e.deltaY * unit };
    }
    this._hint.hidden = true;
    this._applyView();
  }

  _onTool(e) {
    const button = e.target.closest('[data-tool]');
    if (!button) return;
    switch (button.dataset.tool) {
      case 'zoom-in': this.zoomBy(1.25); break;
      case 'zoom-out': this.zoomBy(1 / 1.25); break;
      case 'zoom-reset': this.resetZoom(); break;
      case 'fit': this.fit(); break;
      case 'present': this.dispatchEvent(new CustomEvent('present-toggle', { bubbles: true })); break;
      case 'export': this.dispatchEvent(new CustomEvent('export-menu', { bubbles: true, detail: { anchor: button } })); break;
      default: break;
    }
  }

  // -------------------------------------------------------------------------
  // Edit mode: card menu and drag onto another card
  // -------------------------------------------------------------------------

  // In persons mode any card is a drag source; in units mode only a unit frame (its tiles are
  // positions, which a unit drag would misrepresent).
  _dragCandidate(element) {
    if (!this.editing || element?.closest?.('[data-toggle], [data-more], [data-menu]')) return null;
    if (this.mode === 'units') {
      if (element?.closest?.('[data-node]')) return null;
      const unit = element?.closest?.('[data-unit]');
      return unit ? { id: unit.dataset.unit, kind: 'unit' } : null;
    }
    const node = element?.closest?.('[data-node]');
    return node ? { id: node.dataset.node, kind: 'position' } : null;
  }

  _labelOf(source) {
    if (source.kind === 'unit') return this._unitById.get(source.id)?.name ?? '';
    const node = this._nodeById.get(source.id);
    return node ? `${node.name} · ${node.role}` : '';
  }

  _elementOf(ref) {
    const attr = ref.kind === 'unit' ? 'data-unit' : 'data-node';
    return this._cardsLayer.querySelector(`[${attr}="${this._attr(ref.id)}"]`);
  }

  _beginCardDrag(source) {
    this._cardDrag = { ...source, ghost: document.createElement('div') };
    const { ghost } = this._cardDrag;
    ghost.className = 'tf-orgtree__ghost';
    ghost.textContent = this._labelOf(source);
    this._box.appendChild(ghost);
    this._elementOf(source)?.classList.add('ot-dragging');
    this.classList.add('tf-orgtree--card-drag');
  }

  _verdict(source, target) {
    return this.dropRule ? this.dropRule(source, target) : { ok: true };
  }

  _moveCardDrag(e) {
    const drag = this._cardDrag;
    if (!drag) return;
    const box = this._box.getBoundingClientRect();
    drag.ghost.style.transform = `translate(${Math.round(e.clientX - box.left + 14)}px, ${Math.round(e.clientY - box.top + 14)}px)`;
    const hit = this.nodeAt(e.clientX, e.clientY, drag.kind);
    const target = hit && !(hit.id === drag.id && hit.kind === drag.kind) ? hit : null;
    const verdict = target ? this._verdict(drag, target) : null;
    drag.ghost.dataset.state = verdict ? (verdict.ok ? 'ok' : 'bad') : 'none';
    this.setDropHover(target, verdict ? verdict.ok : null);
  }

  _endCardDrag(e, cancelled) {
    const drag = this._cardDrag;
    if (!drag) return;
    const hit = cancelled ? null : this.nodeAt(e.clientX, e.clientY, drag.kind);
    const target = hit && !(hit.id === drag.id && hit.kind === drag.kind) ? hit : null;
    const verdict = target ? this._verdict(drag, target) : null;
    const source = { id: drag.id, kind: drag.kind };
    this._cancelCardDrag();
    if (target) this.dispatchEvent(new CustomEvent('node-drop', { bubbles: true, detail: { source, target, verdict } }));
  }

  _cancelCardDrag() {
    const drag = this._cardDrag;
    if (!drag) return;
    this._cardDrag = null;
    drag.ghost.remove();
    this._elementOf(drag)?.classList.remove('ot-dragging');
    this.classList.remove('tf-orgtree--card-drag');
    this.setDropHover(null);
  }

  /** Outlines `target` ({id, kind}) green (`ok`) or red; null clears. Used by the host for drags that start outside the chart. */
  setDropHover(target, ok = true) {
    if (this._hover) {
      this._hover.classList.remove('ot-drop-ok', 'ot-drop-bad');
      this._hover = null;
    }
    if (!target || ok === null) return;
    const el = this._elementOf(target);
    if (!el) return;
    el.classList.add(ok ? 'ot-drop-ok' : 'ot-drop-bad');
    this._hover = el;
  }

  /**
   * The card under a screen point: `{id, kind}` or null. `want` limits the answer to
   * 'position' or 'unit' (a person dragged from a list can only land on a position).
   */
  nodeAt(clientX, clientY, want = null) {
    const el = typeof document.elementFromPoint === 'function' ? document.elementFromPoint(clientX, clientY) : null;
    if (el && this._svg.contains(el)) {
      const node = want !== 'unit' ? el.closest('[data-node]') : null;
      if (node) return { id: node.dataset.node, kind: 'position' };
      const unit = want !== 'position' ? el.closest('[data-unit]') : null;
      if (unit) return { id: unit.dataset.unit, kind: 'unit' };
    }
    if (!this._layout) return null;
    const box = this._svg.getBoundingClientRect();
    const x = (clientX - box.left - this._view.x) / this._view.k;
    const y = (clientY - box.top - this._view.y) / this._view.k;
    let hit = null;
    for (const item of this._layout.items) {
      if (x >= item.x && x <= item.x + item.w && y >= item.y && y <= item.y + item.h) hit = item;
    }
    if (!hit) return null;
    const ref = hit.node.ref ? { id: hit.id, kind: 'position' } : { id: hit.node.unit.id, kind: 'unit' };
    return want && want !== ref.kind ? null : ref;
  }

  _onContextMenu(e) {
    if (!this.editing) return;
    const target = this._target(e.target);
    if (target?.type !== 'select') return;
    e.preventDefault();
    this.dispatchEvent(new CustomEvent('node-menu', {
      bubbles: true,
      detail: { id: target.id, kind: target.kind, rect: { left: e.clientX, top: e.clientY, width: 1, height: 1 } },
    }));
  }

  _emitMenuForActive() {
    const active = this._layout.byId.get(this._activeId);
    if (!active) return;
    const ref = active.node.ref ? { id: active.id, kind: 'position' } : { id: active.node.unit.id, kind: 'unit' };
    const handle = this._elementOf(ref)?.querySelector('[data-menu]');
    const box = handle ? handle.getBoundingClientRect() : this._svg.getBoundingClientRect();
    this.dispatchEvent(new CustomEvent('node-menu', {
      bubbles: true,
      detail: { ...ref, rect: { left: box.left, top: box.top, width: box.width, height: box.height } },
    }));
  }

  // -------------------------------------------------------------------------
  // Selection and expansion
  // -------------------------------------------------------------------------

  _target(element) {
    const menu = element?.closest?.('[data-menu]');
    if (menu) return { type: 'menu', id: menu.dataset.menu, kind: menu.dataset.kind, element: menu };
    const toggle = element?.closest?.('[data-toggle]');
    if (toggle) return { type: 'toggle', id: toggle.dataset.toggle };
    const more = element?.closest?.('[data-more]');
    if (more) return { type: 'toggle', id: more.dataset.more };
    const node = element?.closest?.('[data-node]');
    if (node) return { type: 'select', id: node.dataset.node, kind: 'position' };
    const unit = element?.closest?.('[data-unit]');
    if (unit) return { type: 'select', id: unit.dataset.unit, kind: 'unit' };
    return null;
  }

  _click(element) {
    const target = this._target(element);
    if (!target) return;
    if (target.type === 'toggle') this.toggle(target.id);
    else if (target.type === 'menu') this._emitMenu(target.id, target.kind, target.element);
    else this._select(target.id, target.kind);
  }

  _emitMenu(id, kind, element) {
    const rect = element.getBoundingClientRect();
    this.dispatchEvent(new CustomEvent('node-menu', {
      bubbles: true,
      detail: { id, kind, rect: { left: rect.left, top: rect.top, width: rect.width, height: rect.height } },
    }));
  }

  _onDoubleClick(e) {
    const target = this._target(e.target);
    if (target?.type === 'select') {
      this.dispatchEvent(new CustomEvent('node-open', { bubbles: true, detail: { id: target.id, kind: target.kind } }));
    }
  }

  _select(id, kind) {
    this._selectedId = kind === 'unit' ? `unit:${id}` : id;
    this._activeId = this._selectedId;
    this._scheduleRender();
    this.dispatchEvent(new CustomEvent('node-select', { bubbles: true, detail: { id, kind } }));
  }

  toggle(layoutId) {
    const ln = this._layout?.byId.get(layoutId)?.node;
    if (!ln?.expandable) return;
    if (this._trim && ln.expanded && ln.more > 0) {
      // The "+N" of a trimmed ancestor: show its other reports (and everything else the trim hid).
      this._trim = false;
      this._pristine = false;
      this._anchorId = layoutId;
      this._relayout({ keepView: true });
      return;
    }
    this._expanded.set(layoutId, !ln.expanded);
    this._pristine = false;
    this._anchorId = layoutId;
    this._relayout({ keepView: true });
    this.classList.remove('tf-orgtree--settle');
    // Restarting the class restarts the fade; reading layout between the two flushes the removal.
    void this.offsetWidth;
    this.classList.add('tf-orgtree--settle');
  }

  /** Centres a node (expanding what hides it), optionally selecting it, and pulses it. */
  focusNode(id, { select = false, zoom = 0.9 } = {}) {
    if (!this._layout) return;
    const layoutId = this.mode === 'units' ? this._unitLayoutIdOf(id) : id;
    if (!layoutId) return;
    this._expandAncestors(layoutId);
    this._activeId = id;
    this._focusId = id;
    if (select) this._selectedId = id;
    this._relayout({ keepView: true });
    if (this._pristine) {
      this._initialView();
    } else {
      const item = this._layout.byId.get(layoutId);
      if (item) this._centerOn(item, zoom);
    }
    this._scheduleRender();
    setTimeout(() => this._pulse(id), 320);
  }

  _unitLayoutIdOf(id) {
    if (String(id).startsWith('unit:')) return id;
    const node = this._nodeById.get(id);
    return node ? `unit:${node.unitId}` : null;
  }

  _expandAncestors(layoutId) {
    if (this.mode === 'units') {
      for (let u = this._unitById.get(String(layoutId).replace(/^unit:/, '')); u?.parentId; u = this._unitById.get(u.parentId)) {
        this._pathOpen.add(`unit:${u.parentId}`);
      }
      return;
    }
    for (let n = this._nodeById.get(layoutId); n?.parentId; n = this._nodeById.get(n.parentId)) {
      this._pathOpen.add(n.parentId);
    }
  }

  _pulse(id) {
    const el = this._cardsLayer.querySelector(`[data-node="${this._attr(id)}"]`);
    if (!el) return;
    el.classList.remove('ot-pulse');
    el.classList.add('ot-pulse');
  }

  // -------------------------------------------------------------------------
  // Keyboard (ARIA tree)
  // -------------------------------------------------------------------------

  // Staff follow the reports in the walk, so every card is reachable by the arrows.
  _siblingsOf(ln) {
    return ln.parent ? [...ln.parent.children, ...ln.parent.staff] : this._roots;
  }

  _neighbour(ln, key) {
    if (key === 'ArrowUp') return ln.parent ?? null;
    if (key === 'ArrowDown') return ln.children[0] ?? ln.staff[0] ?? null;
    const siblings = this._siblingsOf(ln);
    const i = siblings.indexOf(ln);
    return siblings[i + (key === 'ArrowRight' ? 1 : -1)] ?? null;
  }

  _onKey(e) {
    if (!this._layout || e.altKey || e.ctrlKey || e.metaKey) return;
    const active = this._layout.byId.get(this._activeId) ?? this._layout.items[0];
    if (this._cardDrag && e.key === 'Escape') {
      e.preventDefault();
      this._cancelCardDrag();
      return;
    }
    if (this.editing && (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey))) {
      e.preventDefault();
      this._emitMenuForActive();
      return;
    }
    switch (e.key) {
      case 'ArrowUp': case 'ArrowDown': case 'ArrowLeft': case 'ArrowRight': {
        e.preventDefault();
        if (e.shiftKey) {
          this._view = {
            ...this._view,
            x: this._view.x + (e.key === 'ArrowLeft' ? 80 : e.key === 'ArrowRight' ? -80 : 0),
            y: this._view.y + (e.key === 'ArrowUp' ? 80 : e.key === 'ArrowDown' ? -80 : 0),
          };
          this._applyView();
          return;
        }
        if (!active) return;
        if (!this._activeId) { this._moveActive(active.node); return; }
        let ln = active.node;
        if (e.key === 'ArrowDown' && ln.expandable && !ln.expanded) {
          this.toggle(ln.id);
          ln = this._layout.byId.get(ln.id).node;
        }
        const next = this._neighbour(ln, e.key);
        if (next) this._moveActive(next);
        return;
      }
      case 'Enter': {
        e.preventDefault();
        if (!active) return;
        const ln = active.node;
        if (ln.expandable) this.toggle(ln.id);
        else this._emitOpen(ln);
        return;
      }
      case ' ': {
        e.preventDefault();
        if (active) this._selectLayoutNode(active.node);
        return;
      }
      case '+': case '=': e.preventDefault(); this.zoomBy(1.25); return;
      case '-': case '_': e.preventDefault(); this.zoomBy(1 / 1.25); return;
      case '0': e.preventDefault(); this.resetZoom(); return;
      default:
    }
  }

  _moveActive(ln) {
    this._activeId = ln.id;
    const item = this._layout.byId.get(ln.id);
    const v = this._viewportWorld();
    const inside = item.x >= v.x0 && item.y >= v.y0 && item.x + item.w <= v.x1 && item.y + item.h <= v.y1;
    if (!inside) this._centerOn(item, this._view.k);
    this._scheduleRender();
  }

  _selectLayoutNode(ln) {
    if (this.mode === 'units') this._select(ln.unit.id, 'unit');
    else this._select(ln.id, 'position');
  }

  _emitOpen(ln) {
    const detail = this.mode === 'units' ? { id: ln.unit.id, kind: 'unit' } : { id: ln.id, kind: 'position' };
    this.dispatchEvent(new CustomEvent('node-open', { bubbles: true, detail }));
  }

  // -------------------------------------------------------------------------
  // Export
  // -------------------------------------------------------------------------

  /**
   * A standalone SVG of the whole chart (or of one unit's positions, always as persons), fully
   * expanded, at normal size, with its own stylesheet. `theme` is 'light' or 'dark'.
   */
  toSvg({ theme = 'light', unitId = null, subtreeOf = null, mode = this.mode } = {}) {
    let include = unitId ? (n) => n.unitId === unitId : null;
    if (subtreeOf) {
      const branch = new Set();
      const pending = [subtreeOf];
      while (pending.length) {
        const id = pending.pop();
        if (branch.has(id) || !this._nodeById.has(id)) continue;
        branch.add(id);
        pending.push(...this._nodeById.get(id).childIds);
      }
      include = (n) => branch.has(n.id);
    }
    const { m, layout, edges } = this._computeLayout({ scale: 1, expandAll: true, include, editing: false, mode: unitId || subtreeOf ? 'persons' : mode });
    if (!layout.items.length) return null;
    const ctx = { ...this._renderContext(false), s: m.s, selectedId: null, activeId: null, pathSet: new Set(), matchSet: new Set(), editing: false };
    let cards = '';
    for (const item of layout.items) {
      const ln = item.node;
      ctx.level = ln.depth + 1;
      ctx.expandable = ln.expandable;
      ctx.expanded = ln.expanded;
      if (ln.ref) {
        cards += cardSvg(ln.ref, item, ctx);
        if (ln.more > 0) cards += morePillSvg(ln.ref, item, ln.more, ctx);
      } else {
        cards += frameSvg(ln.unit, item, ctx, ln.size);
      }
    }
    let lines = '';
    let staff = '';
    for (const edge of edges) {
      if (edge.kind === 'staff') staff += edge.d;
      else lines += edge.d;
    }
    const { width, height } = layout;
    const palette = theme === 'dark' ? '#0a0d24' : '#ffffff';
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${Math.round(width)}" height="${Math.round(height)}" viewBox="0 0 ${Math.round(width)} ${Math.round(height)}">`
      + `<style>${exportStyle(theme)}</style><rect width="100%" height="100%" fill="${palette}"/>`
      + `<g class="ot-lines"><path d="${lines}"/><path class="staff" d="${staff}"/></g>${cards}</svg>`;
    return { svg, width: Math.round(width), height: Math.round(height) };
  }
}

customElements.define('tf-org-tree', TfOrgTree);
export { TfOrgTree };
