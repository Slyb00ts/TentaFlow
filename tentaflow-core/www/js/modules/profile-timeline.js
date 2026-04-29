// =============================================================================
// File: profile-timeline.js
// Purpose: UnifiedTimeline — Canvas-based multi-source profiling timeline that
//          renders auto-discovered lanes (CPU per-core heatmap, CPU power, RAM,
//          disk, GPU per-device SM/Mem/Power/Kernels, NVTX ranges, network) on
//          a single Canvas2D with SVG/HTML overlays for crosshair, range
//          selection and tooltips. Supports scroll-zoom, drag-pan, shift+drag
//          range selection, hover hit-testing via offscreen color-keyed canvas.
// =============================================================================

import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-searchbox.js';

// ----- helpers ---------------------------------------------------------------

const NS_PER_S = 1_000_000_000;
const NS_PER_MS = 1_000_000;

const VENDOR_PALETTE = {
  nvidia: { stroke: '#76b900', fill: 'rgba(118,185,0,0.08)', dim: '#b9d300' },
  amd:    { stroke: '#ed1c24', fill: 'rgba(237,28,36,0.08)', dim: '#ff6b6b' },
  intel:  { stroke: '#4ba3e0', fill: 'rgba(0,113,197,0.08)', dim: '#4ba3e0' },
  apple:  { stroke: '#d4d4d8', fill: 'rgba(212,212,216,0.08)', dim: '#a1a1aa' },
  generic:{ stroke: '#a78bfa', fill: 'rgba(167,139,250,0.08)', dim: '#c4b5fd' },
};

const KERNEL_COLORS = [
  '#76b900', '#a78bfa', '#60a5fa', '#22c55e', '#f59e0b', '#ef4444',
  '#ed1c24', '#4ba3e0', '#c4b5fd', '#4ade80', '#b9d300', '#ff6b6b',
];

function hashString(s) {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  return Math.abs(h);
}

function colorForName(nameId, names) {
  const s = (names && names[nameId]) || String(nameId);
  return KERNEL_COLORS[hashString(s) % KERNEL_COLORS.length];
}

function fmtTime(ns) {
  const totalSec = ns / NS_PER_S;
  const m = Math.floor(totalSec / 60);
  const rem = totalSec - m * 60;
  if (m > 0) {
    return `${m}:${rem.toFixed(2).padStart(5, '0')}`;
  }
  if (rem >= 1) return `${rem.toFixed(2)}s`;
  if (rem >= 0.001) return `${(rem * 1000).toFixed(2)}ms`;
  return `${(rem * 1_000_000).toFixed(0)}us`;
}

function fmtBytes(b) {
  if (b >= 1e9) return `${(b / 1e9).toFixed(2)} GB`;
  if (b >= 1e6) return `${(b / 1e6).toFixed(1)} MB`;
  if (b >= 1e3) return `${(b / 1e3).toFixed(1)} KB`;
  return `${b} B`;
}

function clamp(v, lo, hi) { return v < lo ? lo : v > hi ? hi : v; }

// Detect vendor from collector / lane-hint metadata.
function vendorOf(devLabel) {
  const s = (devLabel || '').toLowerCase();
  if (s.includes('nvidia') || s.includes('cuda')) return 'nvidia';
  if (s.includes('amd') || s.includes('rocm') || s.includes('radeon')) return 'amd';
  if (s.includes('intel') || s.includes('arc')) return 'intel';
  if (s.includes('apple') || s.includes('metal')) return 'apple';
  return 'generic';
}

// ----- lane construction -----------------------------------------------------

const LANE_HEIGHT = 22;
const LANE_HEAT_HEIGHT = 30;

/**
 * Build lane descriptors from raw events. Each lane records its kind, label,
 * height, color, the indices into events[] that belong to it, and a precomputed
 * value range for line-style lanes.
 */
function buildLanes(events, names) {
  const cpuCores = new Map();        // core -> events[]
  const cpuPower = [];                // PowerSample CpuPkg
  const cpuSamples = [];
  const ramUsed = [];
  const ramBw = [];
  const diskByDev = new Map();        // device -> events[]
  const gpuByDev = new Map();         // device_id -> { util:[], power:[], kernels:[] }
  const nvtxByDev = new Map();
  const networkByIface = new Map();

  events.forEach((ev, idx) => {
    const cat = ev.category;
    const p = ev.payload || {};
    if (cat === 'CpuUtil') {
      const core = (p.core !== undefined) ? p.core : ev.lane_hint;
      if (!cpuCores.has(core)) cpuCores.set(core, []);
      cpuCores.get(core).push(idx);
    } else if (cat === 'CpuSample') {
      cpuSamples.push(idx);
    } else if (cat === 'PowerSample') {
      const dom = p.domain;
      if (dom === 'CpuPkg' || (dom && dom.CpuPkg !== undefined)) {
        cpuPower.push(idx);
      } else if (dom && typeof dom === 'object' && dom.Gpu !== undefined) {
        const dev = dom.Gpu;
        if (!gpuByDev.has(dev)) gpuByDev.set(dev, { util: [], power: [], kernels: [], mem: [] });
        gpuByDev.get(dev).power.push(idx);
      }
    } else if (cat === 'RamSample') {
      ramUsed.push(idx);
    } else if (cat === 'RamBandwidth') {
      ramBw.push(idx);
    } else if (cat === 'DiskIoBurst') {
      const dev = (p.deviceNameId !== undefined ? names[p.deviceNameId] : null) ?? `disk${ev.lane_hint}`;
      if (!diskByDev.has(dev)) diskByDev.set(dev, []);
      diskByDev.get(dev).push(idx);
    } else if (cat === 'GpuUtilSample') {
      const dev = p.device_id !== undefined ? p.device_id : ev.lane_hint;
      if (!gpuByDev.has(dev)) gpuByDev.set(dev, { util: [], power: [], kernels: [], mem: [] });
      gpuByDev.get(dev).util.push(idx);
    } else if (cat === 'GpuKernel') {
      const dev = p.device_id !== undefined ? p.device_id : ev.lane_hint;
      if (!gpuByDev.has(dev)) gpuByDev.set(dev, { util: [], power: [], kernels: [], mem: [] });
      gpuByDev.get(dev).kernels.push(idx);
    } else if (cat === 'GpuMemSample') {
      const dev = p.device_id !== undefined ? p.device_id : ev.lane_hint;
      if (!gpuByDev.has(dev)) gpuByDev.set(dev, { util: [], power: [], kernels: [], mem: [] });
      gpuByDev.get(dev).mem.push(idx);
    } else if (cat === 'NvtxRange') {
      const dev = p.device_id !== undefined ? p.device_id : ev.lane_hint;
      if (!nvtxByDev.has(dev)) nvtxByDev.set(dev, []);
      nvtxByDev.get(dev).push(idx);
    } else if (cat === 'NetworkSample') {
      const iface = (p.ifaceNameId !== undefined ? names[p.ifaceNameId] : null) ?? `net${ev.lane_hint}`;
      if (!networkByIface.has(iface)) networkByIface.set(iface, []);
      networkByIface.get(iface).push(idx);
    }
  });

  const lanes = [];

  // CPU per-core heatmaps (max 16 visible)
  const cores = Array.from(cpuCores.keys()).sort((a, b) => a - b);
  cores.slice(0, 16).forEach((core) => {
    lanes.push({
      id: `cpu-core-${core}`,
      kind: 'heatmap',
      label: `CPU ${core}`,
      color: '#6366f1',
      height: LANE_HEAT_HEIGHT,
      events: cpuCores.get(core),
      visible: true,
      vMin: 0, vMax: 100,
      valueOf: (ev) => ev.payload.util_pct || 0,
    });
  });

  if (cpuSamples.length) {
    lanes.push({
      id: 'cpu-samples',
      kind: 'flamestrip',
      label: 'CPU samples',
      color: '#a78bfa',
      height: LANE_HEIGHT,
      events: cpuSamples,
      visible: true,
    });
  }

  if (cpuPower.length) {
    lanes.push({
      id: 'cpu-power',
      kind: 'line',
      label: 'CPU power W',
      color: '#f59e0b',
      height: LANE_HEIGHT,
      events: cpuPower,
      visible: true,
      valueOf: (ev) => ev.payload.watts || 0,
    });
  }

  if (ramUsed.length) {
    lanes.push({
      id: 'ram-used',
      kind: 'area',
      label: 'RAM used',
      color: '#a78bfa',
      height: LANE_HEIGHT,
      events: ramUsed,
      visible: true,
      valueOf: (ev) => (ev.payload.used_bytes || 0) / 1e9,
      unit: 'GB',
    });
  }

  if (ramBw.length) {
    lanes.push({
      id: 'ram-bw',
      kind: 'line',
      label: 'RAM BW GB/s',
      color: '#60a5fa',
      height: LANE_HEIGHT,
      events: ramBw,
      visible: true,
      valueOf: (ev) => ((ev.payload.read_bps || 0) + (ev.payload.write_bps || 0)) / 1e9,
    });
  }

  for (const [dev, idxs] of diskByDev) {
    lanes.push({
      id: `disk-${dev}-r`,
      kind: 'sparkline',
      label: `Disk ${dev} R`,
      color: '#22c55e',
      height: LANE_HEIGHT,
      events: idxs,
      visible: true,
      valueOf: (ev) => (ev.payload.read_bps || 0) / 1e6,
    });
    lanes.push({
      id: `disk-${dev}-w`,
      kind: 'sparkline',
      label: `Disk ${dev} W`,
      color: '#ef4444',
      height: LANE_HEIGHT,
      events: idxs,
      visible: true,
      valueOf: (ev) => (ev.payload.write_bps || 0) / 1e6,
    });
  }

  // GPU per device — multiple sub-lanes per device.
  const devKeys = Array.from(gpuByDev.keys()).sort((a, b) => a - b);
  devKeys.forEach((dev) => {
    const grp = gpuByDev.get(dev);
    const label = (names && names[`gpu_${dev}`]) || `GPU ${dev}`;
    const vendor = vendorOf(label);
    const pal = VENDOR_PALETTE[vendor];
    if (grp.util.length) {
      lanes.push({
        id: `gpu-${dev}-sm`,
        kind: 'line',
        label: `${label} SM%`,
        color: pal.stroke,
        height: LANE_HEIGHT,
        events: grp.util,
        visible: true,
        vMin: 0, vMax: 100,
        valueOf: (ev) => ev.payload.compute_pct || 0,
      });
      lanes.push({
        id: `gpu-${dev}-mem`,
        kind: 'line',
        label: `${label} Mem%`,
        color: pal.dim,
        height: LANE_HEIGHT,
        events: grp.util,
        visible: true,
        vMin: 0, vMax: 100,
        valueOf: (ev) => ev.payload.mem_pct || 0,
      });
    }
    if (grp.power.length) {
      lanes.push({
        id: `gpu-${dev}-power`,
        kind: 'line',
        label: `${label} Power W`,
        color: '#f59e0b',
        height: LANE_HEIGHT,
        events: grp.power,
        visible: true,
        valueOf: (ev) => ev.payload.watts || 0,
      });
    }
    if (grp.kernels.length) {
      lanes.push({
        id: `gpu-${dev}-kernels`,
        kind: 'bars',
        label: `${label} Kernels`,
        color: pal.stroke,
        height: LANE_HEIGHT,
        events: grp.kernels,
        visible: true,
        vendor,
      });
    }
  });

  for (const [dev, idxs] of nvtxByDev) {
    lanes.push({
      id: `nvtx-${dev}`,
      kind: 'ranges',
      label: `NVTX dev${dev}`,
      color: '#a78bfa',
      height: LANE_HEIGHT,
      events: idxs,
      visible: true,
    });
  }

  for (const [iface, idxs] of networkByIface) {
    lanes.push({
      id: `net-${iface}-rx`,
      kind: 'sparkline',
      label: `Net ${iface} RX`,
      color: '#60a5fa',
      height: LANE_HEIGHT,
      events: idxs,
      visible: true,
      valueOf: (ev) => (ev.payload.rx_bps || 0) / 1e6,
    });
    lanes.push({
      id: `net-${iface}-tx`,
      kind: 'sparkline',
      label: `Net ${iface} TX`,
      color: '#22c55e',
      height: LANE_HEIGHT,
      events: idxs,
      visible: true,
      valueOf: (ev) => (ev.payload.tx_bps || 0) / 1e6,
    });
  }

  // Pre-compute vMin/vMax per lane for line/area/sparkline.
  for (const l of lanes) {
    if (!l.valueOf) continue;
    if (l.vMin !== undefined && l.vMax !== undefined) continue;
    let mn = Infinity, mx = -Infinity;
    for (const i of l.events) {
      const v = l.valueOf(events[i]);
      if (v < mn) mn = v;
      if (v > mx) mx = v;
    }
    if (!isFinite(mn) || !isFinite(mx) || mx === mn) {
      mn = 0; mx = mx > 0 ? mx : 1;
    }
    l.vMin = mn;
    l.vMax = mx;
  }

  return lanes;
}

// ----- main class ------------------------------------------------------------

export class UnifiedTimeline {
  constructor(container, data = {}) {
    if (!container) throw new Error('UnifiedTimeline: container required');
    this.container = container;
    this.events = Array.isArray(data.events) ? data.events : [];
    this.names = data.names || {};
    this.frames = data.frames || [];
    this.stacks = data.stacks || [];
    this.collectors = data.collectors || [];
    this.duration_ns = Number(data.duration_ns) || this._inferDuration();

    this.viewportStartNs = 0;
    this.viewportEndNs = this.duration_ns;
    this.lanes = buildLanes(this.events, this.names);

    this._listeners = new Map();
    this._dpr = window.devicePixelRatio || 1;
    this._renderQueued = false;
    this._needsHitTest = true;
    this._hitMap = null;       // Int32Array view of event idx (+1) per pixel column×lane row
    this._hitBuffer = null;    // backing buffer reused across rebuilds to avoid 2MB+ alloc per frame
    this._hitBufferLen = 0;
    this._hitW = 0;
    this._hitH = 0;
    this._rangeStartNs = null;
    this._rangeEndNs = null;
    this._rangeMode = false;
    this._panState = null;
    this._dragSel = null;
    this._searchQuery = '';
    this._matchedEventIds = null;

    this._build();
    this._attachInteractions();
    this._scheduleRender();
  }

  // ---- public API ----
  on(event, handler) {
    if (!this._listeners.has(event)) this._listeners.set(event, new Set());
    this._listeners.get(event).add(handler);
    return () => this._listeners.get(event).delete(handler);
  }
  _emit(event, detail) {
    const set = this._listeners.get(event);
    if (!set) return;
    for (const h of set) {
      try { h(detail); } catch (err) { console.error(`UnifiedTimeline handler error (${event})`, err); }
    }
  }

  setVisibleLanes(laneIds) {
    const wanted = new Set(laneIds);
    for (const l of this.lanes) l.visible = wanted.has(l.id);
    this._renderSidebar();
    this._needsHitTest = true;
    this._scheduleRender();
  }

  setTimeRange(startNs, endNs) {
    if (endNs <= startNs) return;
    this.viewportStartNs = clamp(startNs, 0, this.duration_ns);
    this.viewportEndNs = clamp(endNs, this.viewportStartNs + 1, this.duration_ns);
    this._renderAxis();
    this._needsHitTest = true;
    this._scheduleRender();
  }

  fitTime() { this.setTimeRange(0, this.duration_ns); }

  selectRange(startNs, endNs) {
    this._rangeStartNs = Math.min(startNs, endNs);
    this._rangeEndNs = Math.max(startNs, endNs);
    this._renderRangeOverlay();
    this._renderSelectionPanel();
    this._emit('rangeSelected', { startNs: this._rangeStartNs, endNs: this._rangeEndNs });
  }

  destroy() {
    if (this._ro) this._ro.disconnect();
    this.container.innerHTML = '';
    this._listeners.clear();
  }

  // ---- DOM ----
  _build() {
    this.container.innerHTML = '';
    this.container.classList.add('tf-timeline');

    const toolbar = document.createElement('div');
    toolbar.className = 'tf-timeline-toolbar';
    toolbar.innerHTML = `
      <tf-button variant="ghost" size="sm" data-act="zoom-in">Zoom in</tf-button>
      <tf-button variant="ghost" size="sm" data-act="zoom-out">Zoom out</tf-button>
      <tf-button variant="ghost" size="sm" data-act="fit">Fit</tf-button>
      <div class="tl-pps" role="group" aria-label="time scale">
        <button data-pps="1000">1 s/px</button>
        <button data-pps="100" class="active">100 ms/px</button>
        <button data-pps="10">10 ms/px</button>
      </div>
      <tf-button variant="ghost" size="sm" data-act="lanes">Lanes ▾</tf-button>
      <tf-searchbox placeholder="Search symbol or kernel…" debounce="200"></tf-searchbox>
      <span class="tl-spacer"></span>
      <tf-chip clickable status="info" data-act="range-mode">Range select</tf-chip>
    `;
    this.container.appendChild(toolbar);
    this.toolbar = toolbar;

    this.lanesPopover = document.createElement('div');
    this.lanesPopover.className = 'tf-timeline-lanes-popover';
    this.container.appendChild(this.lanesPopover);

    const layout = document.createElement('div');
    layout.className = 'tf-timeline-layout';
    this.container.appendChild(layout);

    const left = document.createElement('div');
    layout.appendChild(left);

    const stage = document.createElement('div');
    stage.className = 'tf-timeline-stage';
    stage.innerHTML = `
      <div class="tf-timeline-axis"></div>
      <div class="tf-timeline-side"></div>
      <div class="tf-timeline-canvas-wrap">
        <canvas class="tl-main"></canvas>
        <canvas class="tl-hit"></canvas>
        <div class="tf-timeline-overlay">
          <div class="crosshair"></div>
          <div class="range-rect"></div>
          <div class="tooltip"></div>
        </div>
      </div>
    `;
    left.appendChild(stage);

    const hint = document.createElement('div');
    hint.className = 'tf-timeline-hint';
    hint.innerHTML = `
      <span><span class="kbd">Drag</span> pan</span>
      <span><span class="kbd">Scroll</span> zoom</span>
      <span><span class="kbd">Shift</span>+drag select range</span>
      <span><span class="kbd">F</span> fit · <span class="kbd">←/→</span> pan · <span class="kbd">+/-</span> zoom · <span class="kbd">Esc</span> clear</span>
    `;
    left.appendChild(hint);

    this.panel = document.createElement('aside');
    this.panel.className = 'tf-timeline-panel';
    layout.appendChild(this.panel);

    // refs
    this.axisEl = stage.querySelector('.tf-timeline-axis');
    this.sideEl = stage.querySelector('.tf-timeline-side');
    this.canvasWrap = stage.querySelector('.tf-timeline-canvas-wrap');
    this.canvas = stage.querySelector('canvas.tl-main');
    this.hitCanvas = stage.querySelector('canvas.tl-hit');
    this.overlay = stage.querySelector('.tf-timeline-overlay');
    this.crosshair = this.overlay.querySelector('.crosshair');
    this.rangeRectEl = this.overlay.querySelector('.range-rect');
    this.tooltip = this.overlay.querySelector('.tooltip');

    this.ctx = this.canvas.getContext('2d');
    this.hitCtx = this.hitCanvas.getContext('2d', { willReadFrequently: true });

    this._renderSidebar();
    this._renderSelectionPanel();

    // Resize observer.
    this._ro = new ResizeObserver(() => {
      this._needsHitTest = true;
      this._scheduleRender();
    });
    this._ro.observe(this.canvasWrap);
  }

  _renderSidebar() {
    const visibleLanes = this.lanes.filter((l) => l.visible);
    let totalH = 0;
    for (const l of visibleLanes) totalH += l.height;
    this.sideEl.style.height = `${totalH}px`;

    this.sideEl.innerHTML = '';
    for (const l of this.lanes) {
      const row = document.createElement('div');
      row.className = 'lane-row' + (l.visible ? '' : ' hidden');
      row.style.height = `${l.height}px`;
      row.dataset.laneId = l.id;
      row.title = l.label;
      row.innerHTML = `<span class="swatch" style="background:${l.color};"></span>${l.label}`;
      row.addEventListener('click', () => {
        l.visible = !l.visible;
        this._renderSidebar();
        this._needsHitTest = true;
        this._scheduleRender();
        this._emit('laneToggled', { laneId: l.id, visible: l.visible });
      });
      this.sideEl.appendChild(row);
    }
  }

  // ---- interactions ----
  _attachInteractions() {
    // Toolbar.
    this.toolbar.addEventListener('click', (e) => {
      const tgt = e.target.closest('[data-act]');
      if (tgt) {
        const act = tgt.getAttribute('data-act');
        if (act === 'zoom-in') this._zoomBy(0.7, 0.5);
        else if (act === 'zoom-out') this._zoomBy(1 / 0.7, 0.5);
        else if (act === 'fit') this.fitTime();
        else if (act === 'lanes') this._toggleLanesPopover();
        else if (act === 'range-mode') this._toggleRangeMode();
        return;
      }
      const pps = e.target.closest('[data-pps]');
      if (pps) {
        this.toolbar.querySelectorAll('[data-pps]').forEach((b) => b.classList.remove('active'));
        pps.classList.add('active');
        const msPerPx = Number(pps.dataset.pps);
        this._applyPxScale(msPerPx);
      }
    });
    this.toolbar.querySelector('tf-searchbox').addEventListener('search', (e) => {
      this._applySearch(e.detail?.value ?? '');
    });

    // Wheel zoom.
    this.canvas.addEventListener('wheel', (e) => {
      e.preventDefault();
      const rect = this.canvas.getBoundingClientRect();
      const mouseX = e.clientX - rect.left;
      const pivot = mouseX / rect.width;
      const factor = e.deltaY > 0 ? 1.2 : 1 / 1.2;
      this._zoomBy(factor, pivot);
    }, { passive: false });

    // Mouse interactions.
    this.canvas.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return;
      this.canvas.setPointerCapture(e.pointerId);
      const rect = this.canvas.getBoundingClientRect();
      const x = e.clientX - rect.left;
      if (e.shiftKey || this._rangeMode) {
        this._dragSel = { startX: x, startNs: this._xToNs(x) };
        this.rangeRectEl.style.display = 'block';
      } else {
        this._panState = { startX: e.clientX, vStart: this.viewportStartNs, vEnd: this.viewportEndNs };
        this.canvas.classList.add('panning');
      }
    });
    this.canvas.addEventListener('pointermove', (e) => {
      const rect = this.canvas.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      if (this._panState) {
        const dx = e.clientX - this._panState.startX;
        const span = this._panState.vEnd - this._panState.vStart;
        const nsPerPx = span / rect.width;
        const delta = -dx * nsPerPx;
        let s = this._panState.vStart + delta;
        let en = this._panState.vEnd + delta;
        if (s < 0) { en -= s; s = 0; }
        if (en > this.duration_ns) { s -= (en - this.duration_ns); en = this.duration_ns; }
        s = clamp(s, 0, this.duration_ns - 1);
        en = clamp(en, s + 1, this.duration_ns);
        this.viewportStartNs = s;
        this.viewportEndNs = en;
        this._renderAxis();
        this._needsHitTest = true;
        this._scheduleRender();
      } else if (this._dragSel) {
        const x0 = Math.min(this._dragSel.startX, x);
        const x1 = Math.max(this._dragSel.startX, x);
        this.rangeRectEl.style.left = `${x0}px`;
        this.rangeRectEl.style.width = `${x1 - x0}px`;
      } else {
        this._showHover(x, y);
      }
    });
    this.canvas.addEventListener('pointerup', (e) => {
      if (this._panState) {
        this._panState = null;
        this.canvas.classList.remove('panning');
      } else if (this._dragSel) {
        const rect = this.canvas.getBoundingClientRect();
        const x = e.clientX - rect.left;
        const endNs = this._xToNs(x);
        const startNs = this._dragSel.startNs;
        if (Math.abs(x - this._dragSel.startX) > 3) {
          this.selectRange(startNs, endNs);
        } else {
          this.rangeRectEl.style.display = 'none';
        }
        this._dragSel = null;
      } else {
        // Click — hit test for event.
        const rect = this.canvas.getBoundingClientRect();
        const evIdx = this._hitTest(e.clientX - rect.left, e.clientY - rect.top);
        if (evIdx !== null) {
          this._emit('eventClicked', { event: this.events[evIdx], index: evIdx });
        }
      }
    });
    this.canvas.addEventListener('pointerleave', () => {
      this.crosshair.style.display = 'none';
      this.tooltip.style.display = 'none';
    });

    // Keyboard.
    this.container.tabIndex = 0;
    this.container.addEventListener('keydown', (e) => {
      if (e.key === 'f' || e.key === 'F') { this.fitTime(); e.preventDefault(); }
      else if (e.key === '+' || e.key === '=') { this._zoomBy(0.7, 0.5); e.preventDefault(); }
      else if (e.key === '-' || e.key === '_') { this._zoomBy(1 / 0.7, 0.5); e.preventDefault(); }
      else if (e.key === 'ArrowLeft') { this._panBy(-0.1); e.preventDefault(); }
      else if (e.key === 'ArrowRight') { this._panBy(0.1); e.preventDefault(); }
      else if (e.key === 'Escape') {
        this._rangeStartNs = null;
        this._rangeEndNs = null;
        this.rangeRectEl.style.display = 'none';
        this._renderSelectionPanel();
        if (this._dragSel) { this._dragSel = null; }
      }
    });
  }

  _toggleLanesPopover() {
    if (this.lanesPopover.classList.contains('open')) {
      this.lanesPopover.classList.remove('open');
      return;
    }
    const btn = this.toolbar.querySelector('[data-act="lanes"]');
    const r = btn.getBoundingClientRect();
    const cr = this.container.getBoundingClientRect();
    this.lanesPopover.style.top = `${r.bottom - cr.top + 4}px`;
    this.lanesPopover.style.left = `${r.left - cr.left}px`;
    this.lanesPopover.innerHTML = this.lanes.map((l) => `
      <label><input type="checkbox" data-lane="${l.id}" ${l.visible ? 'checked' : ''}/>
      <span style="display:inline-block;width:8px;height:8px;border-radius:2px;background:${l.color};"></span>
      ${l.label}</label>`).join('');
    this.lanesPopover.querySelectorAll('input[type="checkbox"]').forEach((cb) => {
      cb.addEventListener('change', () => {
        const id = cb.dataset.lane;
        const l = this.lanes.find((x) => x.id === id);
        if (!l) return;
        l.visible = cb.checked;
        this._renderSidebar();
        this._needsHitTest = true;
        this._scheduleRender();
      });
    });
    this.lanesPopover.classList.add('open');
    const offClick = (e) => {
      if (this.lanesPopover.contains(e.target) || e.target.closest('[data-act="lanes"]')) return;
      this.lanesPopover.classList.remove('open');
      document.removeEventListener('pointerdown', offClick, true);
    };
    setTimeout(() => document.addEventListener('pointerdown', offClick, true), 0);
  }

  _toggleRangeMode() {
    this._rangeMode = !this._rangeMode;
    const chip = this.toolbar.querySelector('[data-act="range-mode"]');
    if (this._rangeMode) chip.setAttribute('active', '');
    else chip.removeAttribute('active');
    this.canvas.classList.toggle('range-mode', this._rangeMode);
  }

  _applyPxScale(msPerPx) {
    const w = this.canvasWrap.clientWidth || 1;
    const span = msPerPx * NS_PER_MS * w;
    const center = (this.viewportStartNs + this.viewportEndNs) / 2;
    let s = center - span / 2;
    let en = center + span / 2;
    if (s < 0) { en -= s; s = 0; }
    if (en > this.duration_ns) { s -= (en - this.duration_ns); en = this.duration_ns; }
    s = clamp(s, 0, this.duration_ns - 1);
    en = clamp(en, s + 1, this.duration_ns);
    this.setTimeRange(s, en);
  }

  _applySearch(query) {
    this._searchQuery = (query || '').trim().toLowerCase();
    if (!this._searchQuery) {
      this._matchedEventIds = null;
    } else {
      const set = new Set();
      this.events.forEach((ev, i) => {
        const p = ev.payload || {};
        const nameId = p.name_id;
        const nm = (nameId !== undefined && this.names[nameId]) || '';
        if (nm.toLowerCase().includes(this._searchQuery)) set.add(i);
      });
      this._matchedEventIds = set;
    }
    this._scheduleRender();
  }

  _zoomBy(factor, pivot) {
    const span = this.viewportEndNs - this.viewportStartNs;
    const newSpan = clamp(span * factor, 1000, this.duration_ns);
    const pivotNs = this.viewportStartNs + span * pivot;
    let s = pivotNs - newSpan * pivot;
    let en = s + newSpan;
    if (s < 0) { en -= s; s = 0; }
    if (en > this.duration_ns) { s -= (en - this.duration_ns); en = this.duration_ns; }
    s = clamp(s, 0, this.duration_ns - 1);
    en = clamp(en, s + 1, this.duration_ns);
    this.setTimeRange(s, en);
  }

  _panBy(fraction) {
    const span = this.viewportEndNs - this.viewportStartNs;
    const delta = span * fraction;
    let s = this.viewportStartNs + delta;
    let en = this.viewportEndNs + delta;
    if (s < 0) { en -= s; s = 0; }
    if (en > this.duration_ns) { s -= (en - this.duration_ns); en = this.duration_ns; }
    this.setTimeRange(s, en);
  }

  // ---- coordinate helpers ----
  _xToNs(x) {
    const w = this.canvas.clientWidth || 1;
    const span = this.viewportEndNs - this.viewportStartNs;
    return this.viewportStartNs + (x / w) * span;
  }
  _nsToX(ns) {
    const w = this.canvas.clientWidth || 1;
    const span = this.viewportEndNs - this.viewportStartNs;
    return ((ns - this.viewportStartNs) / span) * w;
  }

  // ---- rendering ----
  _scheduleRender() {
    if (this._renderQueued) return;
    this._renderQueued = true;
    requestAnimationFrame(() => {
      this._renderQueued = false;
      this._renderAxis();
      this._renderCanvas();
      this._renderRangeOverlay();
      if (this._needsHitTest) {
        this._buildHitMap();
        this._needsHitTest = false;
      }
    });
  }

  _renderAxis() {
    if (!this.axisEl) return;
    const w = this.canvasWrap.clientWidth || 1;
    const span = this.viewportEndNs - this.viewportStartNs;
    // pick tick spacing.
    const targetTicks = Math.max(4, Math.floor(w / 110));
    const rawStep = span / targetTicks;
    const niceSteps = [
      1e6, 5e6, 1e7, 5e7, 1e8, 5e8, 1e9, 5e9, 1e10, 3e10, 6e10,
    ];
    let step = niceSteps[niceSteps.length - 1];
    for (const s of niceSteps) { if (s >= rawStep) { step = s; break; } }
    let html = '';
    const first = Math.ceil(this.viewportStartNs / step) * step;
    for (let t = first; t <= this.viewportEndNs; t += step) {
      const px = ((t - this.viewportStartNs) / span) * w;
      html += `<div class="tick" style="left:${px}px">${fmtTime(t)}</div>`;
    }
    this.axisEl.innerHTML = html;
  }

  _renderCanvas() {
    const wrap = this.canvasWrap;
    const w = wrap.clientWidth;
    const visible = this.lanes.filter((l) => l.visible);
    let totalH = 0;
    for (const l of visible) totalH += l.height;
    if (w <= 0 || totalH <= 0) return;

    const dpr = this._dpr;
    this.canvas.width = Math.floor(w * dpr);
    this.canvas.height = Math.floor(totalH * dpr);
    this.canvas.style.width = `${w}px`;
    this.canvas.style.height = `${totalH}px`;
    this.hitCanvas.width = this.canvas.width;
    this.hitCanvas.height = this.canvas.height;

    const ctx = this.ctx;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = '#0b0e22';
    ctx.fillRect(0, 0, w, totalH);

    let y = 0;
    for (const lane of visible) {
      this._drawLane(ctx, lane, y, w, lane.height);
      // separator
      ctx.strokeStyle = 'rgba(31,37,72,0.5)';
      ctx.beginPath();
      ctx.moveTo(0, y + lane.height - 0.5);
      ctx.lineTo(w, y + lane.height - 0.5);
      ctx.stroke();
      y += lane.height;
    }
  }

  _drawLane(ctx, lane, y, w, h) {
    const pad = 2;
    const innerY = y + pad;
    const innerH = h - pad * 2;
    const vs = this.viewportStartNs;
    const ve = this.viewportEndNs;
    const span = ve - vs;
    const nsPerPx = span / w;

    if (lane.kind === 'heatmap') {
      // Bin events into pixel columns; max util_pct color per bin.
      const bins = new Float32Array(w);
      const counts = new Uint32Array(w);
      for (const i of lane.events) {
        const ev = this.events[i];
        const t = ev.t_start_ns;
        if (t < vs || t > ve) continue;
        const px = Math.floor(((t - vs) / span) * w);
        if (px < 0 || px >= w) continue;
        const v = lane.valueOf(ev);
        if (v > bins[px]) bins[px] = v;
        counts[px]++;
      }
      for (let x = 0; x < w; x++) {
        if (counts[x] === 0) continue;
        const t = bins[x] / 100;
        ctx.fillStyle = this._heatColor(t);
        ctx.fillRect(x, innerY, 1, innerH);
      }
    } else if (lane.kind === 'line') {
      this._drawLine(ctx, lane, innerY, w, innerH, vs, ve, span, false);
    } else if (lane.kind === 'area') {
      this._drawLine(ctx, lane, innerY, w, innerH, vs, ve, span, true);
    } else if (lane.kind === 'sparkline') {
      this._drawLine(ctx, lane, innerY, w, innerH, vs, ve, span, false);
    } else if (lane.kind === 'bars') {
      ctx.save();
      for (const i of lane.events) {
        const ev = this.events[i];
        if (ev.t_end_ns < vs || ev.t_start_ns > ve) continue;
        const x0 = Math.max(0, ((ev.t_start_ns - vs) / span) * w);
        const x1 = Math.min(w, ((ev.t_end_ns - vs) / span) * w);
        const ww = Math.max(1, x1 - x0);
        const nameId = ev.payload.name_id;
        const c = colorForName(nameId, this.names);
        if (this._matchedEventIds && !this._matchedEventIds.has(i)) {
          ctx.globalAlpha = 0.18;
        } else {
          ctx.globalAlpha = 0.85;
        }
        ctx.fillStyle = c;
        ctx.fillRect(x0, innerY + 2, ww, innerH - 4);
      }
      ctx.restore();
    } else if (lane.kind === 'ranges') {
      ctx.save();
      for (const i of lane.events) {
        const ev = this.events[i];
        if (ev.t_end_ns < vs || ev.t_start_ns > ve) continue;
        const x0 = Math.max(0, ((ev.t_start_ns - vs) / span) * w);
        const x1 = Math.min(w, ((ev.t_end_ns - vs) / span) * w);
        const ww = Math.max(2, x1 - x0);
        const nameId = ev.payload.name_id;
        const c = colorForName(nameId, this.names);
        ctx.fillStyle = c + '55';
        ctx.strokeStyle = c;
        ctx.lineWidth = 1;
        ctx.fillRect(x0, innerY + 1, ww, innerH - 2);
        ctx.strokeRect(x0 + 0.5, innerY + 1.5, ww - 1, innerH - 3);
        if (ww > 50) {
          ctx.fillStyle = '#e8ebf5';
          ctx.font = '10px JetBrains Mono, monospace';
          const nm = this.names[nameId] || `nvtx_${nameId}`;
          ctx.fillText(nm.substring(0, Math.floor(ww / 7)), x0 + 4, innerY + innerH - 4);
        }
      }
      ctx.restore();
    } else if (lane.kind === 'flamestrip') {
      // Vertical mini-bars colored by stack_id top byte.
      ctx.save();
      const cols = new Uint8Array(w);
      const stackByCol = new Uint32Array(w);
      for (const i of lane.events) {
        const ev = this.events[i];
        const t = ev.t_start_ns;
        if (t < vs || t > ve) continue;
        const px = Math.floor(((t - vs) / span) * w);
        if (px < 0 || px >= w) continue;
        cols[px] = 1;
        stackByCol[px] = ev.payload.stack_id || 0;
      }
      for (let x = 0; x < w; x++) {
        if (!cols[x]) continue;
        const c = KERNEL_COLORS[stackByCol[x] % KERNEL_COLORS.length];
        ctx.fillStyle = c;
        ctx.fillRect(x, innerY + 2, 1, innerH - 4);
      }
      ctx.restore();
    }

    // Lane label (right-edge muted) for clarity at narrow widths.
    ctx.fillStyle = 'rgba(106,113,150,0.0)';
  }

  _drawLine(ctx, lane, y, w, h, vs, ve, span, area) {
    // Downsample: per pixel column compute min/max and last value.
    const minA = new Float32Array(w);
    const maxA = new Float32Array(w);
    const seen = new Uint8Array(w);
    for (let x = 0; x < w; x++) { minA[x] = Infinity; maxA[x] = -Infinity; }
    let lastV = null;
    for (const i of lane.events) {
      const ev = this.events[i];
      const t = ev.t_start_ns;
      if (t < vs || t > ve) continue;
      const px = Math.floor(((t - vs) / span) * w);
      if (px < 0 || px >= w) continue;
      const v = lane.valueOf(ev);
      if (v < minA[px]) minA[px] = v;
      if (v > maxA[px]) maxA[px] = v;
      seen[px] = 1;
      lastV = v;
    }
    const vMin = lane.vMin;
    const vMax = lane.vMax;
    const range = (vMax - vMin) || 1;
    const yOf = (v) => y + h - ((v - vMin) / range) * h;

    ctx.save();
    ctx.lineWidth = 1.4;
    ctx.strokeStyle = lane.color;

    if (area) {
      ctx.fillStyle = lane.color + '33';
      ctx.beginPath();
      let started = false;
      let lv = vMin;
      for (let x = 0; x < w; x++) {
        const v = seen[x] ? maxA[x] : lv;
        lv = v;
        const yy = yOf(v);
        if (!started) { ctx.moveTo(x, yy); started = true; }
        else ctx.lineTo(x, yy);
      }
      ctx.lineTo(w, y + h);
      ctx.lineTo(0, y + h);
      ctx.closePath();
      ctx.fill();
    }

    // Stroke: draw min/max band as a single path.
    ctx.beginPath();
    let started = false;
    let lv = vMin;
    for (let x = 0; x < w; x++) {
      const v = seen[x] ? maxA[x] : lv;
      lv = v;
      const yy = yOf(v);
      if (!started) { ctx.moveTo(x, yy); started = true; }
      else ctx.lineTo(x, yy);
    }
    ctx.stroke();

    // For columns where min<max, draw a faint vertical extent.
    ctx.strokeStyle = lane.color + '55';
    ctx.beginPath();
    for (let x = 0; x < w; x++) {
      if (!seen[x]) continue;
      if (maxA[x] - minA[x] <= 0) continue;
      ctx.moveTo(x + 0.5, yOf(maxA[x]));
      ctx.lineTo(x + 0.5, yOf(minA[x]));
    }
    ctx.stroke();
    ctx.restore();
  }

  _heatColor(t) {
    // Indigo -> violet -> light gradient.
    t = clamp(t, 0, 1);
    const stops = [
      [0, [31, 37, 72]],
      [0.25, [49, 46, 129]],
      [0.5, [99, 102, 241]],
      [0.75, [167, 139, 250]],
      [1, [196, 181, 253]],
    ];
    for (let i = 0; i < stops.length - 1; i++) {
      const [t0, c0] = stops[i];
      const [t1, c1] = stops[i + 1];
      if (t >= t0 && t <= t1) {
        const f = (t - t0) / (t1 - t0);
        const r = Math.round(c0[0] + (c1[0] - c0[0]) * f);
        const g = Math.round(c0[1] + (c1[1] - c0[1]) * f);
        const b = Math.round(c0[2] + (c1[2] - c0[2]) * f);
        return `rgb(${r},${g},${b})`;
      }
    }
    return '#6366f1';
  }

  _renderRangeOverlay() {
    if (this._rangeStartNs === null) {
      this.rangeRectEl.style.display = 'none';
      return;
    }
    const x0 = clamp(this._nsToX(this._rangeStartNs), 0, this.canvas.clientWidth);
    const x1 = clamp(this._nsToX(this._rangeEndNs), 0, this.canvas.clientWidth);
    if (x1 <= x0) {
      this.rangeRectEl.style.display = 'none';
      return;
    }
    this.rangeRectEl.style.display = 'block';
    this.rangeRectEl.style.left = `${x0}px`;
    this.rangeRectEl.style.width = `${x1 - x0}px`;
  }

  // ---- hit testing (off-screen color-keyed canvas) ----
  _buildHitMap() {
    const w = this.canvas.clientWidth;
    const visible = this.lanes.filter((l) => l.visible);
    let totalH = 0;
    for (const l of visible) totalH += l.height;
    if (w <= 0 || totalH <= 0) { this._hitMap = null; return; }
    const need = w * totalH;
    // Reuse backing buffer to avoid reallocating ~2MB on every viewport change.
    // Grow with 1.5x slack so a few resize ticks don't re-grow.
    if (!this._hitBuffer || this._hitBufferLen < need) {
      const cap = Math.ceil(need * 1.5);
      this._hitBuffer = new Int32Array(cap);
      this._hitBufferLen = cap;
    }
    const map = this._hitBuffer.subarray(0, need);
    map.fill(0);
    const vs = this.viewportStartNs;
    const ve = this.viewportEndNs;
    const span = ve - vs;
    let y = 0;
    for (const lane of visible) {
      // For wide bar/range lanes write event idx directly.
      if (lane.kind === 'bars' || lane.kind === 'ranges') {
        for (const i of lane.events) {
          const ev = this.events[i];
          if (ev.t_end_ns < vs || ev.t_start_ns > ve) continue;
          const x0 = Math.max(0, Math.floor(((ev.t_start_ns - vs) / span) * w));
          const x1 = Math.min(w, Math.ceil(((ev.t_end_ns - vs) / span) * w));
          for (let xx = x0; xx < x1; xx++) {
            for (let yy = y; yy < y + lane.height; yy++) {
              map[yy * w + xx] = i + 1;
            }
          }
        }
      } else {
        // Point lanes: write the closest event idx into the column.
        for (const i of lane.events) {
          const ev = this.events[i];
          const t = ev.t_start_ns;
          if (t < vs || t > ve) continue;
          const px = Math.floor(((t - vs) / span) * w);
          if (px < 0 || px >= w) continue;
          for (let yy = y; yy < y + lane.height; yy++) {
            map[yy * w + px] = i + 1;
          }
        }
      }
      y += lane.height;
    }
    this._hitMap = map;
    this._hitW = w;
    this._hitH = totalH;
  }

  _hitTest(x, y) {
    if (!this._hitMap) return null;
    const px = Math.floor(x);
    const py = Math.floor(y);
    if (px < 0 || px >= this._hitW || py < 0 || py >= this._hitH) return null;
    const v = this._hitMap[py * this._hitW + px];
    if (v === 0) {
      // Try +/- 2px column for thin point lanes.
      for (let d = 1; d <= 3; d++) {
        const a = this._hitMap[py * this._hitW + Math.min(this._hitW - 1, px + d)];
        if (a) return a - 1;
        const b = this._hitMap[py * this._hitW + Math.max(0, px - d)];
        if (b) return b - 1;
      }
      return null;
    }
    return v - 1;
  }

  _showHover(x, y) {
    this.crosshair.style.display = 'block';
    this.crosshair.style.left = `${x}px`;
    const idx = this._hitTest(x, y);
    if (idx === null) {
      this.tooltip.style.display = 'none';
      return;
    }
    const ev = this.events[idx];
    const dur = ev.t_end_ns - ev.t_start_ns;
    const p = ev.payload || {};
    const nm = (p.name_id !== undefined && this.names[p.name_id]) || ev.category;
    const rows = [];
    rows.push(`<div class="t-title">${nm}</div>`);
    rows.push(`<div class="t-row"><b>cat</b> ${ev.category}</div>`);
    rows.push(`<div class="t-row"><b>t</b> ${fmtTime(ev.t_start_ns)} → ${fmtTime(ev.t_end_ns)}</div>`);
    rows.push(`<div class="t-row"><b>dur</b> ${dur > 0 ? fmtTime(dur) : 'point'}</div>`);
    if (p.device_id !== undefined) rows.push(`<div class="t-row"><b>dev</b> ${p.device_id}</div>`);
    if (p.bytes !== undefined) rows.push(`<div class="t-row"><b>bytes</b> ${fmtBytes(p.bytes)}</div>`);
    if (p.watts !== undefined) rows.push(`<div class="t-row"><b>W</b> ${p.watts.toFixed(1)}</div>`);
    if (p.compute_pct !== undefined) rows.push(`<div class="t-row"><b>SM%</b> ${p.compute_pct.toFixed(1)}</div>`);
    if (p.util_pct !== undefined) rows.push(`<div class="t-row"><b>util%</b> ${p.util_pct.toFixed(1)} core ${p.core ?? ev.lane_hint}</div>`);
    if (p.ifaceNameId !== undefined) {
      const ifaceLabel = this.names[p.ifaceNameId] || `iface_${p.ifaceNameId}`;
      rows.push(`<div class="t-row"><b>${ifaceLabel}</b> rx ${fmtBytes(p.rx_bps || 0)}/s · tx ${fmtBytes(p.tx_bps || 0)}/s</div>`);
    }
    if (p.deviceNameId !== undefined && p.read_bps !== undefined) {
      const devLabel = this.names[p.deviceNameId] || `disk_${p.deviceNameId}`;
      rows.push(`<div class="t-row"><b>${devLabel}</b> r ${fmtBytes(p.read_bps)}/s · w ${fmtBytes(p.write_bps)}/s</div>`);
    }

    this.tooltip.innerHTML = rows.join('');
    this.tooltip.style.display = 'block';
    const tx = clamp(x + 14, 0, this.canvas.clientWidth - 240);
    const ty = clamp(y + 14, 0, (this.canvas.clientHeight || 200) - 100);
    this.tooltip.style.left = `${tx}px`;
    this.tooltip.style.top = `${ty}px`;
  }

  // ---- selection panel (stats from current range) ----
  _renderSelectionPanel() {
    if (this._rangeStartNs === null || this._rangeEndNs === null) {
      this.panel.innerHTML = `<div class="sp-empty">Drag with <b>Shift</b> to select a range — cross-source stats will appear here.</div>`;
      return;
    }
    const s = this._rangeStartNs;
    const en = this._rangeEndNs;
    const dur = en - s;

    // Aggregate.
    const cpuSamplesByStack = new Map();   // stack_id top -> count
    const gpuKernelsByDev = new Map();      // dev -> Map<name_id,duration>
    let powerSum = 0, powerN = 0;
    let diskRead = 0, diskWrite = 0;

    for (const ev of this.events) {
      if (ev.t_end_ns < s || ev.t_start_ns > en) continue;
      const overlap = Math.max(0, Math.min(ev.t_end_ns, en) - Math.max(ev.t_start_ns, s));
      const p = ev.payload || {};
      if (ev.category === 'CpuSample') {
        const key = p.stack_id || 0;
        cpuSamplesByStack.set(key, (cpuSamplesByStack.get(key) || 0) + 1);
      } else if (ev.category === 'GpuKernel') {
        const dev = p.device_id || 0;
        if (!gpuKernelsByDev.has(dev)) gpuKernelsByDev.set(dev, new Map());
        const inner = gpuKernelsByDev.get(dev);
        const nm = (this.names[p.name_id] || `kernel_${p.name_id}`);
        inner.set(nm, (inner.get(nm) || 0) + (overlap || 1));
      } else if (ev.category === 'PowerSample') {
        powerSum += p.watts || 0;
        powerN++;
      } else if (ev.category === 'DiskIoBurst') {
        // Approximate: rate × overlap seconds.
        const sec = overlap / NS_PER_S;
        diskRead += (p.read_bps || 0) * sec;
        diskWrite += (p.write_bps || 0) * sec;
      }
    }

    // CPU top stacks → resolve top frame.
    const totalCpu = Array.from(cpuSamplesByStack.values()).reduce((a, b) => a + b, 0) || 1;
    const cpuRows = Array.from(cpuSamplesByStack.entries())
      .sort((a, b) => b[1] - a[1])
      .slice(0, 5)
      .map(([stackId, cnt]) => {
        const stack = this.stacks[stackId] || [];
        const top = stack.length ? this.frames[stack[0]] : null;
        const sym = (top && top.name) || `stack_${stackId}`;
        return `<div class="sp-item"><span class="sym">${sym}</span><span class="pct">${(cnt / totalCpu * 100).toFixed(1)}%</span></div>`;
      })
      .join('');

    // GPU per-device.
    const vendorBlocks = [];
    for (const [dev, inner] of gpuKernelsByDev) {
      const total = Array.from(inner.values()).reduce((a, b) => a + b, 0) || 1;
      const top = Array.from(inner.entries()).sort((a, b) => b[1] - a[1]).slice(0, 3);
      const label = this.names[`gpu_${dev}`] || `GPU ${dev}`;
      const vendor = vendorOf(label);
      const pal = VENDOR_PALETTE[vendor];
      vendorBlocks.push(`
        <div class="v-col">
          <div class="v-name" style="color:${pal.stroke}">${label}</div>
          ${top.map(([nm, d]) => `<div class="v-row">${nm} ${(d / total * 100).toFixed(0)}%</div>`).join('') || '<div class="v-row" style="color:var(--text-3); font-style:italic">no data</div>'}
        </div>`);
    }

    const avgPower = powerN ? (powerSum / powerN) : 0;
    const totalEnergy = avgPower * (dur / NS_PER_S);

    const hasCpu = cpuRows.length > 0;
    this.panel.innerHTML = `
      <div class="sp-head">
        <span class="range">${fmtTime(s)} → ${fmtTime(en)}</span>
        <span class="duration-pill">${fmtTime(dur)}</span>
      </div>
      ${hasCpu ? `<div class="sp-block">
        <div class="sp-title">Top CPU symbols</div>
        <div class="sp-list">${cpuRows}</div>
      </div>` : ''}
      ${vendorBlocks.length ? `<div class="sp-block">
        <div class="sp-title">Top GPU kernels</div>
        <div class="sp-vendor-grid">${vendorBlocks.join('')}</div>
      </div>` : ''}
      <div class="sp-block">
        <div class="sp-title">Power & IO</div>
        <div class="sp-list">
          <div class="sp-item"><span class="sym">Avg power</span><span class="pct">${avgPower.toFixed(0)} W</span></div>
          <div class="sp-item"><span class="sym">Total energy</span><span class="pct">${(totalEnergy / 1000).toFixed(2)} kJ</span></div>
          <div class="sp-item"><span class="sym">Disk read</span><span class="pct">${fmtBytes(diskRead)}</span></div>
          <div class="sp-item"><span class="sym">Disk write</span><span class="pct">${fmtBytes(diskWrite)}</span></div>
        </div>
      </div>
      ${hasCpu ? `<div class="open-flame">
        <tf-button variant="ghost" size="sm" data-act="open-flame">Open in flamegraph →</tf-button>
      </div>` : ''}
    `;
    const flameBtn = this.panel.querySelector('[data-act="open-flame"]');
    if (flameBtn) {
      flameBtn.addEventListener('click', () => {
        this._emit('openFlamegraph', { startNs: s, endNs: en });
      });
    }
  }

  // ---- misc ----
  _inferDuration() {
    let mx = 0;
    for (const ev of this.events) if (ev.t_end_ns > mx) mx = ev.t_end_ns;
    return mx || NS_PER_S;
  }
}

// =============================================================================
// TimelineView — adapter wiring UnifiedTimeline into profile-report.
// Profile report dispatcher (renderLazyTab) wymaga `render(host, ctx)`. Tu
// hostujemy class UnifiedTimeline w kontenerze i przepinamy event
// `openFlamegraph` na <tf-tabs> rodzica zeby panel "Open in flamegraph" dzialal.
// =============================================================================

export const TimelineView = {
  render(host, ctx) {
    if (!host) return;
    const report = ctx?.report || {};
    const events = ctx?.events || report.events || [];
    if (!events.length) {
      host.innerHTML = `
        <div class="pr-card">
          <div class="pr-banner-degraded">
            <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
            <div><strong>No timeline data.</strong> This report contains zero events.</div>
          </div>
        </div>`;
      return;
    }

    // Tear down previous instance jesli host byl juz uzywany — chroni przed
    // wyciekiem ResizeObserver gdy uzytkownik przelacza taby tam i z powrotem.
    if (host._timelineInstance && typeof host._timelineInstance.destroy === 'function') {
      try { host._timelineInstance.destroy(); } catch (_) { /* noop */ }
      host._timelineInstance = null;
    }

    host.innerHTML = '';
    const card = document.createElement('div');
    card.className = 'pr-card pr-timeline-card';
    host.appendChild(card);

    const mount = document.createElement('div');
    card.appendChild(mount);

    const tl = new UnifiedTimeline(mount, {
      events,
      names: report.names || {},
      frames: report.frames || [],
      stacks: report.stacks || [],
      collectors: report.collectors || [],
      duration_ns: report.duration_ns || 0,
    });

    // Bridge "Open in flamegraph" → switch to flame tab via <tf-tabs>.
    tl.on('openFlamegraph', () => {
      const root = host.closest('[id]')?.ownerDocument || document;
      const tabs = root.querySelector('#pr-tabs');
      if (tabs && typeof tabs.setAttribute === 'function') {
        tabs.setAttribute('value', 'flame');
        tabs.dispatchEvent(new CustomEvent('change', { detail: { value: 'flame' } }));
      }
    });

    // Cleanup hook for future host re-renders.
    host._timelineInstance = tl;
  },
};

export default UnifiedTimeline;
