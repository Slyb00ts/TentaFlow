// =============================================================================
// Plik: components/tf-relation-graph.js
// Opis: Graf powiązań (Domain::RelationGraph 0x0703). Canvas 2D + własny
//       force-directed layout (O(n²) z chłodzeniem alpha i zamrażaniem po
//       ustabilizowaniu). Zoom (wheel/pinch), pan (drag tła), drag nodów,
//       klik noda/krawędzi (CustomEvent node_click / edge_click), wybór z
//       podświetleniem sąsiedztwa i pulsującymi ringami, etykiety
//       collision-aware, kolory z tokenów --tf-*.
// Właściwości: nodes, edges, layout, interactive, maxNodes, selectedNodeId,
//              reducedMotion.
// =============================================================================

import { cssToken } from './shared-styles.js';

// Tone → design-token CSS var (kolor obrysu noda / krawędzi).
const TONE_VAR = {
  neutral: '--tf-text-3',
  primary: '--tf-accent-1',
  success: '--tf-success',
  warning: '--tf-warning',
  critical: '--tf-danger',
  info: '--tf-info',
  muted: '--tf-text-3',
};

// Physics constants — tuned for <2000 nodes at 60fps with plain O(n²)
// repulsion. Alpha cools each tick; below ALPHA_MIN the layout freezes.
const REPULSION = 5200;
const SPRING_LENGTH = 130;
const SPRING_K = 0.035;
const CENTER_GRAVITY = 0.015;
const DAMPING = 0.82;
const ALPHA_START = 1;
const ALPHA_DECAY = 0.985;
const ALPHA_MIN = 0.02;
const MAX_TICKS = 900;
// Hard node cap when the consumer does not set one (maxNodes<=0): the force
// simulation is O(n²) per tick and freezes the tab on thousands of nodes.
const DEFAULT_MAX_NODES = 500;

const ENTRY_DURATION_MS = 420;
const ENTRY_STAGGER_MS = 14;
const PULSE_PERIOD_MS = 2600;
const CLICK_SLOP_PX = 4;
const EDGE_HIT_PX = 6;
const LABEL_MIN_ZOOM = 0.55;

// Deterministic per-id pseudo-random in [0,1) — stable initial positions so
// the same graph always settles into a similar shape.
function idNoise(id, salt) {
  let h = 0x811c9dc5 ^ salt;
  for (let i = 0; i < id.length; i += 1) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return ((h >>> 0) % 100000) / 100000;
}

// Spring easing for the entry animation (overshoot like --tf-spring-snappy).
function springEase(t) {
  if (t >= 1) return 1;
  return 1 - Math.pow(1 - t, 3) * Math.cos(t * Math.PI * 1.2);
}

// BFS depth map from a start node over an undirected adjacency list.
function bfsDepths(startId, adjacency) {
  const depth = new Map([[startId, 0]]);
  const queue = [startId];
  while (queue.length) {
    const cur = queue.shift();
    for (const next of adjacency.get(cur) || []) {
      if (!depth.has(next)) {
        depth.set(next, depth.get(cur) + 1);
        queue.push(next);
      }
    }
  }
  return depth;
}

export class TfRelationGraph extends HTMLElement {
  constructor() {
    super();
    this._nodes = [];
    this._edges = [];
    this._layout = 'force_directed';
    this._interactive = true;
    this._maxNodes = DEFAULT_MAX_NODES;
    this._selectedId = null;
    this._reducedMotion =
      typeof window !== 'undefined' && typeof window.matchMedia === 'function'
        ? window.matchMedia('(prefers-reduced-motion: reduce)').matches
        : false;

    // Simulation state: id → {x,y,vx,vy,r,degree,node}. Kept across data
    // updates so incremental changes do not scramble the whole layout.
    this._sim = new Map();
    this._edgeList = [];
    this._neighbors = new Map();
    this._alpha = 0;
    this._ticks = 0;

    this._canvas = null;
    this._ro = null;
    this._raf = 0;
    this._entryStartTs = 0;
    this._colorCache = null;

    // View transform (world → screen): screen = world * k + [x, y].
    this._view = { x: 0, y: 0, k: 1 };
    this._viewFitted = false;

    // Pointer interaction state.
    this._pointers = new Map();
    this._drag = null;
    this._pinch = null;
    this._userMovedView = false;
  }

  connectedCallback() {
    if (!this._canvas) {
      this._canvas = document.createElement('canvas');
      this._canvas.style.display = 'block';
      this._canvas.style.width = '100%';
      this._canvas.style.height = '100%';
      this._canvas.style.touchAction = 'none';
      this.appendChild(this._canvas);
      this._bindPointerEvents();
    }
    if (typeof ResizeObserver !== 'undefined' && !this._ro) {
      this._ro = new ResizeObserver(() => {
        this._resizeCanvas();
        // Container size changed (responsive stacking, panel resize): refit
        // the camera unless the user already took over pan/zoom.
        if (!this._userMovedView) this._fitViewIfNeeded(true);
        this._requestFrame();
      });
      this._ro.observe(this);
    }
    this._resizeCanvas();
    this._rebuild();
  }

  disconnectedCallback() {
    if (this._ro) {
      this._ro.disconnect();
      this._ro = null;
    }
    if (this._raf) {
      cancelAnimationFrame(this._raf);
      this._raf = 0;
    }
  }

  // ---------------------------------------------------------------------------
  // Public properties
  // ---------------------------------------------------------------------------

  set nodes(value) {
    this._nodes = Array.isArray(value) ? value : [];
    if (this.isConnected) this._rebuild();
  }

  get nodes() { return this._nodes; }

  set edges(value) {
    this._edges = Array.isArray(value) ? value : [];
    if (this.isConnected) this._rebuild();
  }

  get edges() { return this._edges; }

  set layout(value) {
    const allowed = ['force_directed', 'hierarchical', 'radial', 'manual'];
    this._layout = allowed.includes(value) ? value : 'force_directed';
    this._viewFitted = false;
    if (this.isConnected) this._rebuild();
  }

  get layout() { return this._layout; }

  set interactive(value) { this._interactive = Boolean(value); }

  get interactive() { return this._interactive; }

  set maxNodes(value) {
    // 0/invalid is NOT "unlimited": the O(n²) force simulation freezes the tab
    // on thousands of nodes, so an absent cap falls back to the hard default.
    const n = Number(value);
    this._maxNodes = Number.isInteger(n) && n > 0 ? n : DEFAULT_MAX_NODES;
    if (this.isConnected) this._rebuild();
  }

  get maxNodes() { return this._maxNodes; }

  set selectedNodeId(value) {
    this._selectedId = typeof value === 'string' && value.length ? value : null;
    this._requestFrame();
  }

  get selectedNodeId() { return this._selectedId; }

  set reducedMotion(value) { this._reducedMotion = Boolean(value); this._requestFrame(); }

  get reducedMotion() { return this._reducedMotion; }

  /// Resets zoom/pan so the whole graph fits the viewport again.
  fitView() {
    this._userMovedView = false;
    this._viewFitted = false;
    this._fitViewIfNeeded(true);
    this._requestFrame();
  }

  // ---------------------------------------------------------------------------
  // Data → simulation state
  // ---------------------------------------------------------------------------

  _rebuild() {
    let nodes = this._nodes;
    const cap = this._maxNodes > 0 ? this._maxNodes : DEFAULT_MAX_NODES;
    if (nodes.length > cap) {
      nodes = nodes.slice(0, cap);
    }
    const ids = new Set(nodes.map((n) => n.id));
    const edges = this._edges.filter(
      (e) => ids.has(e.sourceId) && ids.has(e.targetId) && e.sourceId !== e.targetId
    );

    const degree = new Map();
    const adjacency = new Map();
    for (const n of nodes) {
      degree.set(n.id, 0);
      adjacency.set(n.id, []);
    }
    for (const e of edges) {
      degree.set(e.sourceId, degree.get(e.sourceId) + 1);
      degree.set(e.targetId, degree.get(e.targetId) + 1);
      adjacency.get(e.sourceId).push(e.targetId);
      adjacency.get(e.targetId).push(e.sourceId);
    }

    const prev = this._sim;
    const sim = new Map();
    const spreadR = 60 + 26 * Math.sqrt(nodes.length);
    let order = 0;
    for (const n of nodes) {
      const old = prev.get(n.id);
      const angle = idNoise(n.id, 7) * Math.PI * 2;
      const dist = 40 + idNoise(n.id, 13) * spreadR;
      const deg = degree.get(n.id);
      sim.set(n.id, {
        node: n,
        x: old ? old.x : Math.cos(angle) * dist,
        y: old ? old.y : Math.sin(angle) * dist,
        vx: 0,
        vy: 0,
        degree: deg,
        r: Math.max(9, Math.min(24, 9 + 2.4 * Math.sqrt(deg))),
        entryOrder: old ? -1 : order,
      });
      order += 1;
    }
    this._sim = sim;
    this._edgeList = edges;
    this._neighbors = adjacency;
    if (this._selectedId && !sim.has(this._selectedId)) this._selectedId = null;

    this._applyStaticLayout(adjacency, degree);

    if (this._layout === 'force_directed') {
      this._alpha = ALPHA_START;
      this._ticks = 0;
      if (this._reducedMotion) {
        // prefers-reduced-motion: settle the layout synchronously and render
        // one static frame instead of animating the simulation. The tick
        // budget keeps the O(n²) settle under ~10⁸ pair ops for big graphs.
        const budget = Math.max(60, Math.min(
          MAX_TICKS,
          Math.ceil(1e8 / Math.max(1, this._sim.size * this._sim.size))
        ));
        let spent = 0;
        while (this._alpha > 0 && spent < budget) {
          this._simTick();
          spent += 1;
        }
        this._alpha = 0;
      }
    } else {
      this._alpha = 0;
    }
    this._entryStartTs = this._now();
    // Theme tokens may have changed between panel opens; re-read lazily.
    this._colorCache = null;
    this._fitViewIfNeeded(false);
    this._requestFrame();
  }

  // Static seeds for the non-force layouts. `radial` = BFS rings around the
  // highest-degree node, `hierarchical` = BFS layers top-down, `manual` =
  // x/y taken verbatim from node data.
  _applyStaticLayout(adjacency, degree) {
    if (this._layout === 'manual') {
      for (const s of this._sim.values()) {
        if (typeof s.node.x === 'number' && Number.isFinite(s.node.x)) s.x = s.node.x;
        if (typeof s.node.y === 'number' && Number.isFinite(s.node.y)) s.y = s.node.y;
      }
      return;
    }
    if (this._layout !== 'radial' && this._layout !== 'hierarchical') return;
    if (this._sim.size === 0) return;

    let rootId = null;
    let best = -1;
    for (const [id, d] of degree) {
      if (d > best) { best = d; rootId = id; }
    }
    const depths = bfsDepths(rootId, adjacency);
    // Disconnected nodes go to the outermost ring/layer.
    let maxDepth = 0;
    for (const d of depths.values()) maxDepth = Math.max(maxDepth, d);
    const outer = maxDepth + 1;
    const byDepth = new Map();
    for (const [id, s] of this._sim) {
      const d = depths.has(id) ? depths.get(id) : outer;
      if (!byDepth.has(d)) byDepth.set(d, []);
      byDepth.get(d).push({ id, s });
    }
    for (const [d, members] of byDepth) {
      members.forEach(({ id, s }, i) => {
        if (this._layout === 'radial') {
          const radius = d * 120;
          const angle = (i / members.length) * Math.PI * 2 + idNoise(id, 3) * 0.35;
          s.x = Math.cos(angle) * radius;
          s.y = Math.sin(angle) * radius;
        } else {
          const width = (members.length - 1) * 140;
          s.x = i * 140 - width / 2;
          s.y = d * 130;
        }
      });
    }
  }

  // ---------------------------------------------------------------------------
  // Force simulation
  // ---------------------------------------------------------------------------

  _simTick() {
    const bodies = [...this._sim.values()];
    const n = bodies.length;
    const alpha = this._alpha;
    for (let i = 0; i < n; i += 1) {
      const a = bodies[i];
      for (let j = i + 1; j < n; j += 1) {
        const b = bodies[j];
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let d2 = dx * dx + dy * dy;
        if (d2 < 1) {
          // Coincident points: nudge apart deterministically.
          dx = (idNoise(a.node.id, 31) - 0.5) * 2;
          dy = (idNoise(b.node.id, 37) - 0.5) * 2;
          d2 = dx * dx + dy * dy + 0.01;
        }
        const f = (REPULSION * alpha) / d2;
        const d = Math.sqrt(d2);
        const fx = (dx / d) * f;
        const fy = (dy / d) * f;
        a.vx += fx; a.vy += fy;
        b.vx -= fx; b.vy -= fy;
      }
    }
    for (const e of this._edgeList) {
      const a = this._sim.get(e.sourceId);
      const b = this._sim.get(e.targetId);
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const d = Math.sqrt(dx * dx + dy * dy) || 1;
      const rest = SPRING_LENGTH + a.r + b.r;
      const f = SPRING_K * (d - rest) * alpha;
      const fx = (dx / d) * f;
      const fy = (dy / d) * f;
      a.vx += fx; a.vy += fy;
      b.vx -= fx; b.vy -= fy;
    }
    for (const s of this._sim.values()) {
      s.vx -= s.x * CENTER_GRAVITY * alpha;
      s.vy -= s.y * CENTER_GRAVITY * alpha;
      if (this._drag && this._drag.kind === 'node' && this._drag.id === s.node.id) {
        s.vx = 0; s.vy = 0;
        continue;
      }
      s.vx *= DAMPING;
      s.vy *= DAMPING;
      s.x += s.vx;
      s.y += s.vy;
    }
    this._ticks += 1;
    this._alpha *= ALPHA_DECAY;
    if (this._alpha < ALPHA_MIN || this._ticks > MAX_TICKS) this._alpha = 0;
  }

  /// Re-heats the simulation (drag / data change) without restarting layout.
  _reheat(alpha) {
    if (this._layout !== 'force_directed') return;
    this._alpha = Math.max(this._alpha, alpha);
    this._ticks = 0;
    this._requestFrame();
  }

  // ---------------------------------------------------------------------------
  // View transform
  // ---------------------------------------------------------------------------

  _fitViewIfNeeded(force) {
    if ((this._viewFitted && !force) || this._sim.size === 0) return;
    const w = this.clientWidth || 800;
    const h = this.clientHeight || 500;
    let minX = Infinity; let minY = Infinity; let maxX = -Infinity; let maxY = -Infinity;
    for (const s of this._sim.values()) {
      minX = Math.min(minX, s.x - s.r);
      minY = Math.min(minY, s.y - s.r);
      maxX = Math.max(maxX, s.x + s.r);
      maxY = Math.max(maxY, s.y + s.r);
    }
    const spanX = Math.max(maxX - minX, 100);
    const spanY = Math.max(maxY - minY, 100);
    const k = Math.min(2, Math.min((w * 0.82) / spanX, (h * 0.82) / spanY));
    this._view = {
      k,
      x: w / 2 - ((minX + maxX) / 2) * k,
      y: h / 2 - ((minY + maxY) / 2) * k,
    };
    this._viewFitted = true;
  }

  _toWorld(px, py) {
    return {
      x: (px - this._view.x) / this._view.k,
      y: (py - this._view.y) / this._view.k,
    };
  }

  _zoomAt(px, py, factor) {
    this._userMovedView = true;
    const k = Math.max(0.15, Math.min(6, this._view.k * factor));
    const scale = k / this._view.k;
    this._view = {
      k,
      x: px - (px - this._view.x) * scale,
      y: py - (py - this._view.y) * scale,
    };
    this._requestFrame();
  }

  // ---------------------------------------------------------------------------
  // Pointer interaction
  // ---------------------------------------------------------------------------

  _bindPointerEvents() {
    const cv = this._canvas;
    cv.addEventListener('wheel', (e) => {
      if (!this._interactive) return;
      e.preventDefault();
      const rect = cv.getBoundingClientRect();
      this._zoomAt(e.clientX - rect.left, e.clientY - rect.top, Math.exp(-e.deltaY * 0.0016));
    }, { passive: false });

    cv.addEventListener('pointerdown', (e) => {
      if (!this._interactive) return;
      const rect = cv.getBoundingClientRect();
      const px = e.clientX - rect.left;
      const py = e.clientY - rect.top;
      this._pointers.set(e.pointerId, { x: px, y: py });
      if (typeof cv.setPointerCapture === 'function' && e.pointerId != null) {
        try { cv.setPointerCapture(e.pointerId); } catch {}
      }
      if (this._pointers.size === 2) {
        // Second finger → switch to pinch, cancel any drag.
        const pts = [...this._pointers.values()];
        this._pinch = { dist: Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y) };
        this._drag = null;
        return;
      }
      const world = this._toWorld(px, py);
      const hitNode = this._hitTestNode(world);
      this._drag = hitNode
        ? { kind: 'node', id: hitNode.node.id, startPx: px, startPy: py, moved: false }
        : { kind: 'pan', startPx: px, startPy: py, viewX: this._view.x, viewY: this._view.y, moved: false };
    });

    cv.addEventListener('pointermove', (e) => {
      if (!this._interactive) return;
      const rect = cv.getBoundingClientRect();
      const px = e.clientX - rect.left;
      const py = e.clientY - rect.top;
      if (this._pointers.has(e.pointerId)) this._pointers.set(e.pointerId, { x: px, y: py });

      if (this._pinch && this._pointers.size >= 2) {
        const pts = [...this._pointers.values()];
        const dist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y) || 1;
        const cx = (pts[0].x + pts[1].x) / 2;
        const cy = (pts[0].y + pts[1].y) / 2;
        this._zoomAt(cx, cy, dist / this._pinch.dist);
        this._pinch.dist = dist;
        return;
      }
      if (!this._drag) return;
      const dx = px - this._drag.startPx;
      const dy = py - this._drag.startPy;
      if (Math.hypot(dx, dy) > CLICK_SLOP_PX) this._drag.moved = true;
      if (this._drag.kind === 'pan') {
        if (this._drag.moved) this._userMovedView = true;
        this._view = { k: this._view.k, x: this._drag.viewX + dx, y: this._drag.viewY + dy };
        this._requestFrame();
      } else if (this._drag.moved) {
        const world = this._toWorld(px, py);
        const s = this._sim.get(this._drag.id);
        if (s) {
          s.x = world.x;
          s.y = world.y;
          s.vx = 0;
          s.vy = 0;
          this._reheat(0.25);
          this._requestFrame();
        }
      }
    });

    const endPointer = (e) => {
      this._pointers.delete(e.pointerId);
      if (this._pointers.size < 2) this._pinch = null;
      if (!this._drag) return;
      const drag = this._drag;
      this._drag = null;
      if (drag.moved) return;
      // Click (no movement): node → select + node_click; edge → edge_click;
      // background → deselect.
      const rect = this._canvas.getBoundingClientRect();
      const world = this._toWorld(e.clientX - rect.left, e.clientY - rect.top);
      const hitNode = this._hitTestNode(world);
      if (hitNode) {
        this._selectedId = hitNode.node.id;
        this._requestFrame();
        this.dispatchEvent(new CustomEvent('node_click', {
          bubbles: false,
          detail: { node_id: hitNode.node.id },
        }));
        return;
      }
      const hitEdge = this._hitTestEdge(world);
      if (hitEdge) {
        this.dispatchEvent(new CustomEvent('edge_click', {
          bubbles: false,
          detail: { edge_id: hitEdge.id },
        }));
        return;
      }
      if (this._selectedId !== null) {
        this._selectedId = null;
        this._requestFrame();
        // Consumers tracking the selection (BFS depth filters, detail panels)
        // need to know the background click cleared it.
        this.dispatchEvent(new CustomEvent('deselect', { bubbles: false, detail: {} }));
      }
    };
    cv.addEventListener('pointerup', endPointer);
    cv.addEventListener('pointercancel', endPointer);
  }

  _hitTestNode(world) {
    let best = null;
    let bestD = Infinity;
    for (const s of this._sim.values()) {
      const d = Math.hypot(world.x - s.x, world.y - s.y);
      if (d <= s.r + 4 / this._view.k && d < bestD) {
        best = s;
        bestD = d;
      }
    }
    return best;
  }

  _hitTestEdge(world) {
    const threshold = EDGE_HIT_PX / this._view.k;
    let best = null;
    let bestD = Infinity;
    for (const e of this._edgeList) {
      const a = this._sim.get(e.sourceId);
      const b = this._sim.get(e.targetId);
      const d = distToSegment(world, a, b);
      if (d <= threshold && d < bestD) {
        best = e;
        bestD = d;
      }
    }
    return best;
  }

  // ---------------------------------------------------------------------------
  // Rendering
  // ---------------------------------------------------------------------------

  _now() {
    return typeof performance !== 'undefined' ? performance.now() : Date.now();
  }

  _resizeCanvas() {
    const cv = this._canvas;
    if (!cv) return;
    const dpr = (typeof window !== 'undefined' && window.devicePixelRatio) || 1;
    const w = Math.max(1, this.clientWidth || 800);
    const h = Math.max(1, this.clientHeight || 500);
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
  }

  _requestFrame() {
    if (this._raf || !this.isConnected) return;
    this._raf = requestAnimationFrame(() => {
      this._raf = 0;
      this._frame();
    });
  }

  _frame() {
    if (this._alpha > 0) {
      this._simTick();
      // Keep the settling graph in view until the user takes over the camera.
      if (!this._userMovedView) this._fitViewIfNeeded(true);
    }
    this._draw();
    const entryActive = !this._reducedMotion
      && this._now() - this._entryStartTs < ENTRY_DURATION_MS + this._sim.size * ENTRY_STAGGER_MS;
    const pulseActive = !this._reducedMotion && this._selectedId !== null;
    if (this._alpha > 0 || entryActive || pulseActive) this._requestFrame();
  }

  _colors() {
    if (this._colorCache) return this._colorCache;
    const tone = {};
    for (const [t, cssVar] of Object.entries(TONE_VAR)) tone[t] = cssToken(cssVar, '#6366f1');
    this._colorCache = {
      tone,
      accent1: cssToken('--tf-accent-1', '#6366f1'),
      accent2: cssToken('--tf-accent-2', '#818cf8'),
      nodeFill: cssToken('--tf-bg-card', '#141836'),
      edgeDim: cssToken('--tf-border', '#1f2548'),
      text: cssToken('--tf-text', '#f5f6ff'),
      text2: cssToken('--tf-text-2', '#c1c5e0'),
      text3: cssToken('--tf-text-3', '#6a7196'),
    };
    return this._colorCache;
  }

  _entryProgress(s) {
    if (this._reducedMotion || s.entryOrder < 0) return 1;
    const t = (this._now() - this._entryStartTs - s.entryOrder * ENTRY_STAGGER_MS) / ENTRY_DURATION_MS;
    if (t <= 0) return 0;
    return Math.min(1, springEase(t));
  }

  _draw() {
    const cv = this._canvas;
    const g = cv && cv.getContext ? cv.getContext('2d') : null;
    if (!g) return;
    const dpr = (typeof window !== 'undefined' && window.devicePixelRatio) || 1;
    const w = cv.width / dpr;
    const h = cv.height / dpr;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);

    const colors = this._colors();
    const view = this._view;
    g.translate(view.x, view.y);
    g.scale(view.k, view.k);

    const selected = this._selectedId;
    const neighborSet = new Set();
    if (selected) {
      neighborSet.add(selected);
      for (const nb of this._neighbors.get(selected) || []) neighborSet.add(nb);
    }
    const nowMs = this._now();

    // --- edges ---
    for (const e of this._edgeList) {
      const a = this._sim.get(e.sourceId);
      const b = this._sim.get(e.targetId);
      const entry = Math.min(this._entryProgress(a), this._entryProgress(b));
      if (entry <= 0) continue;
      const touchesSelection = selected
        && (e.sourceId === selected || e.targetId === selected);
      const dimmed = selected && !touchesSelection;
      const weight = typeof e.weight === 'number' && Number.isFinite(e.weight) ? e.weight : 1;
      const width = Math.max(0.8, Math.min(5, 0.9 + weight * 1.6));
      const toneColor = e.tone ? colors.tone[e.tone] : null;

      g.save();
      g.globalAlpha = (dimmed ? 0.16 : touchesSelection ? 0.9 : 0.5) * entry;
      g.lineWidth = touchesSelection ? width + 0.8 : width;
      g.strokeStyle = dimmed
        ? colors.edgeDim
        : (toneColor || (touchesSelection ? colors.accent2 : colors.edgeDim));
      if (e.dashed) {
        g.setLineDash([6, 6]);
        if (touchesSelection && !this._reducedMotion) {
          g.lineDashOffset = -(nowMs / 90) % 12;
        }
      }
      g.beginPath();
      g.moveTo(a.x, a.y);
      g.lineTo(b.x, b.y);
      g.stroke();

      // Flowing dash overlay on highlighted solid edges (mockup edge-hl-flow).
      if (touchesSelection && !e.dashed && !this._reducedMotion) {
        g.globalAlpha = 0.85 * entry;
        g.strokeStyle = colors.accent2;
        g.lineWidth = Math.max(1.4, width * 0.8);
        g.setLineDash([3, 15]);
        g.lineDashOffset = -(nowMs / 70) % 18;
        g.beginPath();
        g.moveTo(a.x, a.y);
        g.lineTo(b.x, b.y);
        g.stroke();
      }
      g.restore();
    }

    // --- nodes ---
    for (const s of this._sim.values()) {
      const entry = this._entryProgress(s);
      if (entry <= 0) continue;
      const isSelected = s.node.id === selected;
      const isNeighbor = !isSelected && neighborSet.has(s.node.id);
      const dimmed = selected && !isSelected && !isNeighbor;
      const toneColor = s.node.tone ? colors.tone[s.node.tone] : colors.accent1;
      const r = s.r * (isSelected ? 1.35 : 1) * entry;

      g.save();
      g.globalAlpha = (dimmed ? 0.3 : 1) * entry;

      if (isSelected && !this._reducedMotion) {
        // Two staggered pulse rings expanding out of the selected node.
        for (const phase of [0, 0.5]) {
          const t = ((nowMs / PULSE_PERIOD_MS) + phase) % 1;
          g.beginPath();
          g.arc(s.x, s.y, r * (1 + 1.15 * t), 0, Math.PI * 2);
          g.strokeStyle = colors.accent2;
          g.globalAlpha = (1 - t) * 0.75 * entry;
          g.lineWidth = 1.5;
          g.stroke();
        }
        g.globalAlpha = entry;
      }

      if (isSelected || isNeighbor) {
        g.shadowColor = toneColor;
        g.shadowBlur = isSelected ? 18 : 9;
      }
      g.beginPath();
      g.arc(s.x, s.y, r, 0, Math.PI * 2);
      g.fillStyle = isSelected ? toneColor : colors.nodeFill;
      g.fill();
      g.shadowBlur = 0;
      g.lineWidth = isSelected ? 1.5 : 2;
      g.strokeStyle = isSelected ? colors.accent2 : toneColor;
      g.stroke();
      g.restore();
    }

    // --- labels (collision-aware, hidden at low zoom) ---
    if (view.k >= LABEL_MIN_ZOOM) {
      const placed = [];
      const bodies = [...this._sim.values()].sort((a, b) => {
        const pa = a.node.id === selected ? 2 : neighborSet.has(a.node.id) ? 1 : 0;
        const pb = b.node.id === selected ? 2 : neighborSet.has(b.node.id) ? 1 : 0;
        return pb - pa || b.degree - a.degree;
      });
      const fontPx = 12 / view.k;
      for (const s of bodies) {
        const entry = this._entryProgress(s);
        if (entry <= 0.3) continue;
        const isSelected = s.node.id === selected;
        const dimmed = selected && !isSelected && !neighborSet.has(s.node.id);
        const label = s.node.label;
        if (!label) continue;
        const size = isSelected ? fontPx * 1.25 : fontPx;
        // The selected node is drawn 1.35x larger — offset its label from the
        // drawn radius, not the base one, so the text clears the circle.
        const drawnR = s.r * (isSelected ? 1.35 : 1);
        g.save();
        g.font = `${isSelected ? 800 : 600} ${size}px system-ui, sans-serif`;
        const metrics = g.measureText(label);
        const lw = metrics.width;
        const lx = s.x - lw / 2;
        const ly = s.y + drawnR + size * 1.5;
        const box = { x: lx - 2, y: ly - size, w: lw + 4, h: size * 1.4 };
        const collides = placed.some((p) =>
          box.x < p.x + p.w && box.x + box.w > p.x && box.y < p.y + p.h && box.y + box.h > p.y
        );
        if (collides && !isSelected) {
          g.restore();
          continue;
        }
        placed.push(box);
        g.globalAlpha = (dimmed ? 0.35 : 1) * entry;
        g.fillStyle = isSelected ? colors.text : dimmed ? colors.text3 : colors.text2;
        g.textAlign = 'center';
        g.fillText(label, s.x, ly);
        g.restore();
      }
    }

    g.setTransform(1, 0, 0, 1, 0, 0);
  }
}

function distToSegment(p, a, b) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return Math.hypot(p.x - a.x, p.y - a.y);
  let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2;
  t = Math.max(0, Math.min(1, t));
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

if (!customElements.get('tf-relation-graph')) {
  customElements.define('tf-relation-graph', TfRelationGraph);
}
