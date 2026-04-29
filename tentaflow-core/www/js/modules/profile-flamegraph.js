// =============================================================================
// Plik: modules/profile-flamegraph.js
// Opis: Interaktywny CPU flamegraph z drill-down, search highlight, reverse
//       (icicle), differential mode i side panel "Selected frame". Zywi sie
//       eventami CpuSample + side-tablicami frames/stacks/names z ProfileReport.
//       Renderowanie SVG z clip-renderingiem (tylko ramki >= MIN_PX_WIDTH).
//       UI komponenty tf-*: tf-button, tf-toggle, tf-searchbox, tf-chip.
// =============================================================================

import '/js/components/tf-button.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-chip.js';

const ROW_H = 22;
const MIN_PX_WIDTH = 1.0;
const SVG_NS = 'http://www.w3.org/2000/svg';

// =============================================================================
// CpuFlamegraph — public class.
// =============================================================================
export class CpuFlamegraph {
  /**
   * @param {HTMLElement} container
   * @param {{ events: Array, frames: Array, stacks: Array, names: Array, totalDurationNs: number, source?: string, sampleHzApprox?: number }} data
   */
  constructor(container, data) {
    if (!container) throw new Error('CpuFlamegraph: container required');
    this.container = container;
    this.data = {
      events: Array.isArray(data?.events) ? data.events : [],
      frames: Array.isArray(data?.frames) ? data.frames : [],
      stacks: Array.isArray(data?.stacks) ? data.stacks : [],
      names: Array.isArray(data?.names) ? data.names : [],
      totalDurationNs: Number(data?.totalDurationNs) || 0,
      source: data?.source || 'linux.perf.cpu_sampling',
      sampleHzApprox: Number(data?.sampleHzApprox) || 0,
    };

    // Derived state
    this.cpuSamples = this._extractCpuSamples();
    this.totalSamples = this.cpuSamples.length;
    this.uniqueStackCount = this._countUniqueStacks();

    // UI state
    this.searchQuery = '';
    this.reversed = false;
    this.minPercent = 1.0;        // hide frames whose share of root < minPercent
    this.colorByModule = false;
    this.differentialMode = false;
    this.diffRangeA = null;       // { startNs, endNs }
    this.diffRangeB = null;
    this.selectedFrameId = null;  // currently highlighted in side panel
    this.zoomPath = [];           // array of frameIds from root → current zoom

    // Tree caches
    this.tree = null;             // current tree (incl. diff data)
    this.diffTreeA = null;
    this.diffTreeB = null;

    // Listeners
    this._listeners = new Map();

    // DOM refs
    this._refs = {};

    // Tooltip element (lives on body)
    this._tooltipEl = null;

    this._renderShell();
    this._buildBaseTree();
    this._renderAll();
  }

  // ------------------------- Public API ------------------------------------
  setSearchQuery(q) {
    this.searchQuery = String(q || '').trim().toLowerCase();
    this._renderFlamegraph();
  }

  setReversed(reversed) {
    this.reversed = !!reversed;
    this._renderFlamegraph();
  }

  setMinPercent(min) {
    const v = Number(min);
    if (!Number.isFinite(v)) return;
    this.minPercent = Math.max(0, Math.min(50, v));
    if (this._refs.minPctVal) this._refs.minPctVal.textContent = `${this.minPercent.toFixed(1)}%`;
    this._renderFlamegraph();
  }

  setColorByModule(on) {
    this.colorByModule = !!on;
    this._renderFlamegraph();
  }

  setDifferentialMode(rangeA, rangeB) {
    if (!rangeA || !rangeB) {
      this.differentialMode = false;
      this.diffRangeA = null;
      this.diffRangeB = null;
      this.diffTreeA = null;
      this.diffTreeB = null;
      this._buildBaseTree();
    } else {
      this.differentialMode = true;
      this.diffRangeA = rangeA;
      this.diffRangeB = rangeB;
      this._buildDifferentialTree();
    }
    this.zoomPath = [];
    this.selectedFrameId = null;
    this._renderAll();
  }

  drillDown(frameId) {
    const node = this._findNodeInCurrentZoomByFrameId(frameId);
    if (!node) return;
    // DFS from current root to node, building the path from frameIds. Using a
    // stack with pop() avoids the O(n) shift() the previous BFS performed and
    // also avoids spreading `path` into a fresh array per visited node.
    const root = this._currentZoomRoot();
    if (root === node) return;
    const found = [];
    const visit = (n) => {
      if (n === node) return true;
      for (const child of n.children.values()) {
        found.push(child.frameId);
        if (visit(child)) return true;
        found.pop();
      }
      return false;
    };
    if (visit(root)) {
      this.zoomPath = [...this.zoomPath, ...found];
      this.selectedFrameId = null;
      this._renderAll();
    }
  }

  reset() {
    this.zoomPath = [];
    this.selectedFrameId = null;
    this.searchQuery = '';
    if (this._refs.searchbox) this._refs.searchbox.value = '';
    this._renderAll();
  }

  on(eventName, handler) {
    if (typeof handler !== 'function') return;
    if (!this._listeners.has(eventName)) this._listeners.set(eventName, new Set());
    this._listeners.get(eventName).add(handler);
  }

  destroy() {
    this._listeners.clear();
    if (this._tooltipEl && this._tooltipEl.parentNode) {
      this._tooltipEl.parentNode.removeChild(this._tooltipEl);
    }
    this._tooltipEl = null;
    this.container.innerHTML = '';
    this._refs = {};
  }

  // ------------------------- Internal: data --------------------------------

  _extractCpuSamples() {
    const out = [];
    for (const ev of this.data.events) {
      if (!ev) continue;
      const cat = ev.category;
      const payload = ev.payload || {};
      // Accept both rkyv-shape ({CpuSample: {...}}) and flat shape used by fixtures.
      const cpuPayload = (cat === 'CpuSample' || cat === 0)
        ? (payload.CpuSample || payload)
        : (payload.CpuSample || null);
      if (!cpuPayload) continue;
      const stackId = Number(cpuPayload.stack_id);
      if (!Number.isFinite(stackId)) continue;
      out.push({
        tid: Number(cpuPayload.tid) || 0,
        cpu: Number(cpuPayload.cpu) || 0,
        stackId,
        startNs: Number(ev.start_ns) || 0,
      });
    }
    return out;
  }

  _countUniqueStacks() {
    const set = new Set();
    for (const s of this.cpuSamples) set.add(s.stackId);
    return set.size;
  }

  // Build aggregate count per stack id, optionally restricted to a time range.
  // Fast path for the common (no-range) case caches the result so a search
  // input change or minPercent slider tweak doesn't re-walk all samples; the
  // cache is invalidated when differential mode toggles ranges via
  // setDifferentialMode → _buildDifferentialTree which always passes a range.
  _aggregateStacks(rangeNs) {
    if (!rangeNs && this._fullStacksCache) return this._fullStacksCache;
    const counts = new Map();
    if (!rangeNs) {
      for (let i = 0; i < this.cpuSamples.length; i++) {
        const id = this.cpuSamples[i].stackId;
        counts.set(id, (counts.get(id) || 0) + 1);
      }
      this._fullStacksCache = counts;
    } else {
      const { startNs, endNs } = rangeNs;
      for (let i = 0; i < this.cpuSamples.length; i++) {
        const s = this.cpuSamples[i];
        if (s.startNs >= startNs && s.startNs < endNs) {
          counts.set(s.stackId, (counts.get(s.stackId) || 0) + 1);
        }
      }
    }
    return counts;
  }

  // Build a flame tree from per-stack counts.
  // Each node: { frameId, name, module, file, line, totalCount, selfCount, children: Map<frameId, Node> }
  _buildTree(counts) {
    const root = this._makeNode(-1, '[all]', '', null, null);
    let total = 0;
    for (const [stackId, count] of counts.entries()) {
      const stack = this.data.stacks[stackId];
      if (!stack || stack.length === 0) {
        root.selfCount += count;
        total += count;
        continue;
      }
      // stacks are leaf-first → traverse from end (root frame) toward start (leaf)
      let cursor = root;
      for (let i = stack.length - 1; i >= 0; i--) {
        const frameId = stack[i];
        let child = cursor.children.get(frameId);
        if (!child) {
          const fr = this.data.frames[frameId] || {};
          child = this._makeNode(
            frameId,
            fr.symbol || `<frame ${frameId}>`,
            fr.module || '',
            fr.file || null,
            fr.line || null,
          );
          cursor.children.set(frameId, child);
        }
        child.totalCount += count;
        if (i === 0) child.selfCount += count;
        cursor = child;
      }
      total += count;
    }
    root.totalCount = total;
    return root;
  }

  _makeNode(frameId, name, module, file, line) {
    return {
      frameId,
      name,
      module,
      file,
      line,
      totalCount: 0,
      selfCount: 0,
      countA: 0,
      countB: 0,
      children: new Map(),
    };
  }

  _buildBaseTree() {
    const counts = this._aggregateStacks(null);
    this.tree = this._buildTree(counts);
  }

  _buildDifferentialTree() {
    const countsA = this._aggregateStacks(this.diffRangeA);
    const countsB = this._aggregateStacks(this.diffRangeB);
    this.diffTreeA = this._buildTree(countsA);
    this.diffTreeB = this._buildTree(countsB);
    // The display tree is union: walk both trees in parallel, build a node when
    // it appears in either, store countA/countB.
    const root = this._makeNode(-1, '[all]', '', null, null);
    const totalA = this.diffTreeA.totalCount;
    const totalB = this.diffTreeB.totalCount;
    root.countA = totalA;
    root.countB = totalB;
    root.totalCount = Math.max(totalA, totalB);
    this._mergeDiff(root, this.diffTreeA, this.diffTreeB);
    this.tree = root;
  }

  _mergeDiff(out, nodeA, nodeB) {
    const ids = new Set();
    if (nodeA) for (const k of nodeA.children.keys()) ids.add(k);
    if (nodeB) for (const k of nodeB.children.keys()) ids.add(k);
    for (const frameId of ids) {
      const cA = nodeA ? nodeA.children.get(frameId) : null;
      const cB = nodeB ? nodeB.children.get(frameId) : null;
      const ref = cA || cB;
      const child = this._makeNode(frameId, ref.name, ref.module, ref.file, ref.line);
      child.countA = cA ? cA.totalCount : 0;
      child.countB = cB ? cB.totalCount : 0;
      child.totalCount = Math.max(child.countA, child.countB);
      child.selfCount = (cA ? cA.selfCount : 0) + (cB ? cB.selfCount : 0);
      out.children.set(frameId, child);
      this._mergeDiff(child, cA, cB);
    }
  }

  // ------------------------- Rendering: shell ------------------------------

  _renderShell() {
    this.container.innerHTML = '';
    const root = document.createElement('div');
    root.className = 'flamegraph-root';
    root.innerHTML = `
      <div class="flamegraph-toolbar">
        <tf-searchbox placeholder="Search frame…" debounce="120" data-ref="searchbox"></tf-searchbox>
        <span class="ftb-group">
          <tf-toggle data-ref="reverseToggle" aria-label="Reverse (icicle)"></tf-toggle>
          Reverse (icicle)
        </span>
        <span class="ftb-group">
          <tf-toggle data-ref="diffToggle" aria-label="Differential mode"></tf-toggle>
          Differential
        </span>
        <span class="ftb-group">
          <tf-toggle data-ref="moduleColorToggle" aria-label="Color by module"></tf-toggle>
          Color by module
        </span>
        <span class="ftb-group">
          Min %
          <input type="range" class="ftb-slider" min="0" max="5" step="0.1" value="1.0" data-ref="minPctSlider" aria-label="Minimum frame percent" />
          <span class="ftb-slider-val" data-ref="minPctVal">1.0%</span>
        </span>
        <tf-button variant="ghost" size="sm" data-ref="resetBtn">Reset</tf-button>
      </div>

      <div class="flamegraph-diff-bar" data-ref="diffBar" hidden>
        <div class="flamegraph-diff-range">
          <div class="fdr-label">Range A (ms)</div>
          <div class="fdr-inputs">
            <input type="number" min="0" step="10" data-ref="diffAStart" placeholder="start" />
            <span class="fdr-unit">→</span>
            <input type="number" min="0" step="10" data-ref="diffAEnd" placeholder="end" />
          </div>
        </div>
        <div class="flamegraph-diff-range">
          <div class="fdr-label">Range B (ms)</div>
          <div class="fdr-inputs">
            <input type="number" min="0" step="10" data-ref="diffBStart" placeholder="start" />
            <span class="fdr-unit">→</span>
            <input type="number" min="0" step="10" data-ref="diffBEnd" placeholder="end" />
          </div>
        </div>
      </div>

      <div class="flamegraph-diff-legend" data-ref="diffLegend" hidden>
        <span class="fdl-end">B faster</span>
        <span class="fdl-bar" aria-hidden="true"></span>
        <span class="fdl-end">B slower</span>
        <span style="margin-left:auto;color:var(--fg-text-3);">color = (B − A) / max</span>
      </div>

      <div class="flamegraph-breadcrumb" data-ref="breadcrumb"></div>

      <div class="flamegraph-layout">
        <div>
          <div class="flamegraph-wrap" data-ref="wrap" tabindex="0" role="application" aria-label="CPU flamegraph">
            <svg class="flamegraph-svg" data-ref="svg" preserveAspectRatio="none" xmlns="${SVG_NS}"></svg>
          </div>
          <div class="flamegraph-foot" data-ref="foot"></div>
        </div>
        <aside class="flamegraph-panel" data-ref="panel"></aside>
      </div>
    `;
    this.container.appendChild(root);

    // Collect refs
    for (const el of root.querySelectorAll('[data-ref]')) {
      this._refs[el.dataset.ref] = el;
    }

    // Tooltip on body (singleton)
    this._tooltipEl = document.createElement('div');
    this._tooltipEl.className = 'flamegraph-tooltip';
    document.body.appendChild(this._tooltipEl);

    this._wireEvents();
  }

  _wireEvents() {
    const r = this._refs;

    r.searchbox.addEventListener('search', (e) => {
      this.setSearchQuery(e.detail?.value ?? '');
    });

    r.reverseToggle.addEventListener('change', (e) => {
      this.setReversed(!!e.detail?.checked);
    });

    r.moduleColorToggle.addEventListener('change', (e) => {
      this.setColorByModule(!!e.detail?.checked);
    });

    r.diffToggle.addEventListener('change', (e) => {
      const on = !!e.detail?.checked;
      r.diffBar.hidden = !on;
      r.diffLegend.hidden = !on;
      if (!on) {
        this.setDifferentialMode(null, null);
      } else {
        // Pre-fill ranges with first half / second half if empty.
        const totalMs = (this.data.totalDurationNs || 0) / 1e6;
        if (totalMs > 0) {
          if (!r.diffAStart.value) r.diffAStart.value = '0';
          if (!r.diffAEnd.value) r.diffAEnd.value = String(Math.round(totalMs / 2));
          if (!r.diffBStart.value) r.diffBStart.value = String(Math.round(totalMs / 2));
          if (!r.diffBEnd.value) r.diffBEnd.value = String(Math.round(totalMs));
        }
        this._applyDiffFromInputs();
      }
    });

    for (const ref of ['diffAStart', 'diffAEnd', 'diffBStart', 'diffBEnd']) {
      r[ref].addEventListener('change', () => {
        if (this.differentialMode) this._applyDiffFromInputs();
      });
    }

    r.minPctSlider.addEventListener('input', (e) => {
      this.setMinPercent(parseFloat(e.target.value));
    });

    r.resetBtn.addEventListener('click', () => this.reset());

    // Keyboard nav inside the chart area
    r.wrap.addEventListener('keydown', (e) => this._onKey(e));
  }

  _applyDiffFromInputs() {
    const r = this._refs;
    const aStart = parseFloat(r.diffAStart.value) || 0;
    const aEnd = parseFloat(r.diffAEnd.value) || 0;
    const bStart = parseFloat(r.diffBStart.value) || 0;
    const bEnd = parseFloat(r.diffBEnd.value) || 0;
    if (aEnd <= aStart || bEnd <= bStart) return;
    this.setDifferentialMode(
      { startNs: aStart * 1e6, endNs: aEnd * 1e6 },
      { startNs: bStart * 1e6, endNs: bEnd * 1e6 },
    );
  }

  // ------------------------- Rendering: full pass --------------------------

  _renderAll() {
    this._renderBreadcrumb();
    this._renderFlamegraph();
    this._renderPanel();
    this._renderFoot();
  }

  _renderBreadcrumb() {
    const bc = this._refs.breadcrumb;
    bc.innerHTML = '';
    const allItem = document.createElement('span');
    allItem.className = 'fbc-item' + (this.zoomPath.length === 0 ? ' current' : '');
    allItem.textContent = '[all]';
    if (this.zoomPath.length > 0) {
      allItem.addEventListener('click', () => {
        this.zoomPath = [];
        this._renderAll();
      });
    }
    bc.appendChild(allItem);

    let cursor = this.tree;
    for (let i = 0; i < this.zoomPath.length; i++) {
      const sep = document.createElement('span');
      sep.className = 'fbc-sep';
      sep.textContent = '›';
      bc.appendChild(sep);

      const fid = this.zoomPath[i];
      const next = cursor.children.get(fid);
      if (!next) break;
      cursor = next;
      const isLast = i === this.zoomPath.length - 1;
      const item = document.createElement('span');
      item.className = 'fbc-item' + (isLast ? ' current' : '');
      item.textContent = next.name;
      if (!isLast) {
        const upTo = i;
        item.addEventListener('click', () => {
          this.zoomPath = this.zoomPath.slice(0, upTo + 1);
          this._renderAll();
        });
      }
      bc.appendChild(item);
    }
  }

  _renderFoot() {
    const samples = this.totalSamples.toLocaleString('en-US');
    const stacks = this.uniqueStackCount.toLocaleString('en-US');
    const hz = this.data.sampleHzApprox > 0 ? `${this.data.sampleHzApprox} Hz · ` : '';
    this._refs.foot.textContent = `Source: ${this.data.source} · ${hz}${samples} samples · ${stacks} unique stacks`;
  }

  _renderPanel() {
    const panel = this._refs.panel;
    panel.innerHTML = '';
    if (this.selectedFrameId === null || this.selectedFrameId === undefined) {
      panel.innerHTML = `
        <div class="fp-head">Selected frame</div>
        <div class="fp-empty">Click a frame to inspect.<br>Enter to drill down · Esc to reset</div>
      `;
      return;
    }
    const node = this._findNodeInCurrentZoomByFrameId(this.selectedFrameId);
    if (!node) {
      panel.innerHTML = `<div class="fp-head">Selected frame</div><div class="fp-empty">Frame not in current zoom.</div>`;
      return;
    }
    const rootTotal = this.tree.totalCount || 1;
    const totalPct = (node.totalCount / rootTotal) * 100;
    const selfPct = (node.selfCount / rootTotal) * 100;

    const file = node.file ? `${escapeText(node.file)}${node.line ? ':' + node.line : ''}` : '—';

    let diffRow = '';
    if (this.differentialMode) {
      const aPct = node.countA / Math.max(1, this.diffTreeA.totalCount) * 100;
      const bPct = node.countB / Math.max(1, this.diffTreeB.totalCount) * 100;
      const delta = bPct - aPct;
      const cls = delta > 0.5 ? 'tt-delta-up' : delta < -0.5 ? 'tt-delta-down' : '';
      const sign = delta > 0 ? '+' : '';
      diffRow = `
        <div class="fp-row"><span class="l">A %</span><span class="r">${aPct.toFixed(2)}%</span></div>
        <div class="fp-row"><span class="l">B %</span><span class="r">${bPct.toFixed(2)}%</span></div>
        <div class="fp-row"><span class="l">Δ (B−A)</span><span class="r ${cls}">${sign}${delta.toFixed(2)}%</span></div>
      `;
    }

    // Top 5 children by totalCount
    const children = [...node.children.values()].sort((a, b) => b.totalCount - a.totalCount).slice(0, 5);
    const childrenHtml = children.length === 0
      ? `<div class="fp-empty" style="padding:8px 0;">No children (leaf)</div>`
      : children.map((c) => {
          const pct = (c.totalCount / rootTotal) * 100;
          return `<div class="fp-child" data-frame="${c.frameId}" tabindex="0" role="button">
            <span class="sym">${escapeText(c.name)}</span>
            <span class="pct">${pct.toFixed(2)}%</span>
          </div>`;
        }).join('');

    // Mockup #06 ma przycisk "Open source (file:line)" — pokazujemy tylko gdy
    // mamy realną ścieżkę pliku, zeby nie zaśmiecać UI dla framów bez debug info.
    const openSrcBtn = node.file
      ? `<tf-button variant="outline" size="sm" data-action="open-source" style="margin-top:6px;"><svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 18l6-6-6-6"/></svg>Open source (${escapeText(node.file)}${node.line ? ':' + node.line : ''})</tf-button>`
      : '';

    panel.innerHTML = `
      <div class="fp-head">Selected frame</div>
      <div class="fp-symbol">${escapeText(node.name)}</div>
      <div class="fp-row"><span class="l">Module</span><span class="r">${escapeText(node.module || '—')}</span></div>
      <div class="fp-row"><span class="l">File</span><span class="r">${file}</span></div>
      <div class="fp-row"><span class="l">Self %</span><span class="r">${selfPct.toFixed(2)}%</span></div>
      <div class="fp-row"><span class="l">Total %</span><span class="r">${totalPct.toFixed(2)}%</span></div>
      <div class="fp-row"><span class="l">Samples</span><span class="r">${node.totalCount.toLocaleString('en-US')}</span></div>
      ${diffRow}
      <div class="fp-section">
        <div class="fp-section-title">Top children</div>
        <div class="fp-children">${childrenHtml}</div>
      </div>
      ${openSrcBtn}
    `;

    panel.querySelectorAll('.fp-child').forEach((el) => {
      const fid = parseInt(el.dataset.frame, 10);
      const fire = () => {
        this.zoomPath = [...this.zoomPath, fid];
        this.selectedFrameId = null;
        this._renderAll();
      };
      el.addEventListener('click', fire);
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); fire(); }
      });
    });

    const openBtn = panel.querySelector('[data-action="open-source"]');
    if (openBtn) {
      openBtn.addEventListener('click', () => {
        this._emit('openSource', { file: node.file, line: node.line, frameId: node.frameId, name: node.name });
      });
    }
  }

  // ------------------------- Rendering: flame SVG --------------------------

  _renderFlamegraph() {
    const svg = this._refs.svg;
    while (svg.firstChild) svg.removeChild(svg.firstChild);

    const zoomRoot = this._currentZoomRoot();
    if (!zoomRoot || zoomRoot.totalCount === 0) {
      // empty state
      svg.setAttribute('viewBox', '0 0 920 60');
      svg.style.height = '60px';
      const t = document.createElementNS(SVG_NS, 'text');
      t.setAttribute('x', '460');
      t.setAttribute('y', '32');
      t.setAttribute('text-anchor', 'middle');
      t.setAttribute('fill', '#6a7196');
      t.setAttribute('font-family', 'JetBrains Mono, monospace');
      t.setAttribute('font-size', '12');
      t.textContent = 'No CPU samples in this range.';
      svg.appendChild(t);
      return;
    }

    // Compute depth so we can size svg
    const maxDepth = this._computeMaxRenderableDepth(zoomRoot);
    const totalRows = maxDepth + 1; // include root row
    const widthPx = 920;
    const heightPx = totalRows * ROW_H + 4;
    svg.setAttribute('viewBox', `0 0 ${widthPx} ${heightPx}`);
    svg.style.height = `${heightPx}px`;

    const zoomTotal = zoomRoot.totalCount;
    const minCount = (this.minPercent / 100) * zoomTotal;

    // Render root row first so it's behind / above visually
    this._renderNode(svg, zoomRoot, 0, widthPx, 0, widthPx, zoomTotal, minCount, totalRows);
  }

  _computeMaxRenderableDepth(root) {
    const minCount = (this.minPercent / 100) * (root.totalCount || 1);
    let maxDepth = 0;
    const walk = (node, depth) => {
      if (depth > maxDepth) maxDepth = depth;
      for (const child of node.children.values()) {
        if (child.totalCount < minCount) continue;
        walk(child, depth + 1);
      }
    };
    walk(root, 0);
    return maxDepth;
  }

  _renderNode(svg, node, x, width, depth, totalWidth, zoomTotal, minCount, totalRows) {
    if (width < MIN_PX_WIDTH) return;

    const reversed = this.reversed;
    // depth 0 = current root (bottom in flame, top in icicle)
    const yFlame = (totalRows - 1 - depth) * ROW_H;       // bottom-up
    const yIcicle = depth * ROW_H;                         // top-down
    const y = reversed ? yIcicle : yFlame;

    const rect = document.createElementNS(SVG_NS, 'rect');
    rect.setAttribute('class', 'flame-rect');
    rect.setAttribute('x', x.toFixed(2));
    rect.setAttribute('y', y.toFixed(2));
    rect.setAttribute('width', width.toFixed(2));
    rect.setAttribute('height', String(ROW_H - 1));
    rect.setAttribute('fill', this._colorForNode(node));
    rect.dataset.frame = String(node.frameId);

    // Search highlight / dim
    if (this.searchQuery && node.frameId !== -1) {
      if (node.name.toLowerCase().includes(this.searchQuery)) {
        rect.classList.add('match');
      } else {
        rect.classList.add('dim');
      }
    }
    if (this.selectedFrameId === node.frameId && node.frameId !== -1) {
      rect.classList.add('selected');
    }

    // Wire interactions
    rect.addEventListener('click', (e) => this._onFrameClick(e, node));
    rect.addEventListener('dblclick', (e) => {
      e.preventDefault();
      this._drillIntoChild(node);
    });
    rect.addEventListener('mouseenter', (e) => this._showTooltip(e, node, zoomTotal));
    rect.addEventListener('mousemove', (e) => this._moveTooltip(e));
    rect.addEventListener('mouseleave', () => this._hideTooltip());

    svg.appendChild(rect);

    // Label only if there's room (>= 28px)
    if (width >= 28) {
      const label = document.createElementNS(SVG_NS, 'text');
      label.setAttribute('class', 'flame-label' + (this._labelLightFor(node) ? ' light' : ''));
      label.setAttribute('x', (x + 5).toFixed(2));
      label.setAttribute('y', (y + ROW_H / 2).toFixed(2));
      const pct = ((node.totalCount / (zoomTotal || 1)) * 100);
      const text = node.frameId === -1
        ? `[all] ${pct.toFixed(0)}% — ${node.totalCount.toLocaleString('en-US')} samples`
        : this._truncateLabel(node.name, width, pct);
      label.textContent = text;
      svg.appendChild(label);
    }

    // Children
    const children = [...node.children.values()].filter((c) => c.totalCount >= minCount);
    children.sort((a, b) => b.totalCount - a.totalCount);
    let cursorX = x;
    const total = node.totalCount || 1;
    for (const child of children) {
      const childW = (child.totalCount / total) * width;
      if (childW >= MIN_PX_WIDTH) {
        this._renderNode(svg, child, cursorX, childW, depth + 1, totalWidth, zoomTotal, minCount, totalRows);
      }
      cursorX += childW;
    }
  }

  _truncateLabel(name, width, pct) {
    // ~6.5px per mono char at 10px font
    const maxChars = Math.max(3, Math.floor((width - 10) / 6.5));
    const pctSuffix = pct >= 1 ? ` ${pct.toFixed(1)}%` : '';
    const budget = maxChars - pctSuffix.length;
    if (budget <= 3) return name.slice(0, Math.max(1, maxChars - 1)) + '…';
    if (name.length <= budget) return name + pctSuffix;
    return name.slice(0, budget - 1) + '…' + pctSuffix;
  }

  _labelLightFor(node) {
    // For diff mode (red/green tones), use light text. For warm palette use dark.
    return this.differentialMode;
  }

  _colorForNode(node) {
    if (this.differentialMode && node.frameId !== -1) {
      return this._diffColor(node);
    }
    if (node.frameId === -1) return '#fbbf24';
    let key;
    if (this.colorByModule) {
      key = node.module || node.name;
    } else {
      key = String(node.frameId) + ':' + node.name;
    }
    const h = hashString(key);
    // Warm palette tuned for dark bg: H ∈ [10..55] (red→yellow), S=70%, L=62%.
    const hue = 10 + (h % 46);
    return `hsl(${hue}, 70%, 62%)`;
  }

  _diffColor(node) {
    const totalA = Math.max(1, this.diffTreeA?.totalCount || 1);
    const totalB = Math.max(1, this.diffTreeB?.totalCount || 1);
    const aPct = node.countA / totalA;
    const bPct = node.countB / totalB;
    const delta = bPct - aPct;
    const absMax = 0.05; // ±5% saturates the gradient
    const norm = Math.max(-1, Math.min(1, delta / absMax));
    if (Math.abs(delta) * 100 < 0.3) return '#4b5563';
    if (norm > 0) {
      // regression: red, intensity = norm
      const l = 55 - norm * 18; // 55 → 37
      return `hsl(0, 72%, ${l.toFixed(0)}%)`;
    }
    const l = 55 + norm * 8; // norm < 0 → l slightly down
    return `hsl(140, 60%, ${Math.max(40, l).toFixed(0)}%)`;
  }

  // ------------------------- Tooltip ---------------------------------------

  _showTooltip(e, node, zoomTotal) {
    if (!this._tooltipEl) return;
    const pct = (node.totalCount / (zoomTotal || 1)) * 100;
    const rootTotal = this.tree.totalCount || 1;
    const globalPct = (node.totalCount / rootTotal) * 100;
    let extra = '';
    if (this.differentialMode && node.frameId !== -1) {
      const aPct = node.countA / Math.max(1, this.diffTreeA.totalCount) * 100;
      const bPct = node.countB / Math.max(1, this.diffTreeB.totalCount) * 100;
      const delta = bPct - aPct;
      const cls = delta > 0.5 ? 'tt-delta-up' : delta < -0.5 ? 'tt-delta-down' : '';
      const sign = delta > 0 ? '+' : '';
      extra = ` <span class="${cls}">Δ ${sign}${delta.toFixed(2)}%</span>`;
    }
    this._tooltipEl.innerHTML = `
      <div><span class="tt-sym">${escapeText(node.name)}</span> ${pct.toFixed(2)}%${extra}</div>
      <div class="tt-meta">${escapeText(node.module || '')} · ${node.totalCount.toLocaleString('en-US')} samples · ${globalPct.toFixed(2)}% of root</div>
    `;
    this._tooltipEl.classList.add('visible');
    this._moveTooltip(e);
  }

  _moveTooltip(e) {
    if (!this._tooltipEl) return;
    const x = e.clientX + 12;
    const y = e.clientY + 14;
    this._tooltipEl.style.left = `${x}px`;
    this._tooltipEl.style.top = `${y}px`;
  }

  _hideTooltip() {
    if (this._tooltipEl) this._tooltipEl.classList.remove('visible');
  }

  // ------------------------- Interactions ----------------------------------

  _onFrameClick(e, node) {
    if (node.frameId === -1) {
      // Click on root in zoomed view = step out one level
      if (this.zoomPath.length > 0) {
        this.zoomPath = this.zoomPath.slice(0, -1);
        this.selectedFrameId = null;
        this._renderAll();
      }
      return;
    }
    if (e.shiftKey || e.altKey) {
      // Modifier = drill down directly
      this.zoomPath = [...this.zoomPath, node.frameId];
      this.selectedFrameId = null;
      this._renderAll();
      return;
    }
    if (this.selectedFrameId === node.frameId) {
      // Second click on same selection → drill down
      this.zoomPath = [...this.zoomPath, node.frameId];
      this.selectedFrameId = null;
      this._renderAll();
      return;
    }
    this.selectedFrameId = node.frameId;
    this._emit('frameSelected', { frameId: node.frameId, node });
    this._renderFlamegraph();
    this._renderPanel();
  }

  _drillIntoChild(node) {
    if (node.frameId === -1) return;
    this.zoomPath = [...this.zoomPath, node.frameId];
    this.selectedFrameId = null;
    this._renderAll();
  }

  _onKey(e) {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (this.zoomPath.length > 0) {
        this.zoomPath = [];
        this.selectedFrameId = null;
        this._renderAll();
      } else {
        this.selectedFrameId = null;
        this._renderPanel();
        this._renderFlamegraph();
      }
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      if (this.selectedFrameId !== null && this.selectedFrameId !== -1) {
        this.zoomPath = [...this.zoomPath, this.selectedFrameId];
        this.selectedFrameId = null;
        this._renderAll();
      }
      return;
    }
    if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
      e.preventDefault();
      this._navigateSibling(e.key === 'ArrowRight' ? 1 : -1);
      return;
    }
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      e.preventDefault();
      const dir = e.key === 'ArrowDown' ? 1 : -1;
      // In flame mode (default), Down = parent, Up = child. In icicle reversed.
      const goChild = (dir === 1) === this.reversed ? false : true;
      this._navigateVertical(goChild);
      return;
    }
  }

  _navigateSibling(delta) {
    const node = this.selectedFrameId !== null
      ? this._findNodeInCurrentZoomByFrameId(this.selectedFrameId)
      : null;
    const parent = node ? this._findParent(this._currentZoomRoot(), node) : this._currentZoomRoot();
    if (!parent) return;
    const minCount = (this.minPercent / 100) * (this._currentZoomRoot().totalCount || 1);
    const siblings = [...parent.children.values()]
      .filter((c) => c.totalCount >= minCount)
      .sort((a, b) => b.totalCount - a.totalCount);
    if (siblings.length === 0) return;
    let idx = node ? siblings.findIndex((c) => c.frameId === node.frameId) : -1;
    idx = (idx + delta + siblings.length) % siblings.length;
    this.selectedFrameId = siblings[idx].frameId;
    this._renderFlamegraph();
    this._renderPanel();
  }

  _navigateVertical(goChild) {
    const root = this._currentZoomRoot();
    const node = this.selectedFrameId !== null
      ? this._findNodeInCurrentZoomByFrameId(this.selectedFrameId)
      : root;
    if (!node) return;
    if (goChild) {
      const minCount = (this.minPercent / 100) * (root.totalCount || 1);
      const children = [...node.children.values()]
        .filter((c) => c.totalCount >= minCount)
        .sort((a, b) => b.totalCount - a.totalCount);
      if (children.length === 0) return;
      this.selectedFrameId = children[0].frameId;
    } else {
      const parent = this._findParent(root, node);
      if (!parent || parent === root) {
        this.selectedFrameId = null;
      } else {
        this.selectedFrameId = parent.frameId;
      }
    }
    this._renderFlamegraph();
    this._renderPanel();
  }

  _findParent(root, target) {
    if (root === target) return null;
    for (const child of root.children.values()) {
      if (child === target) return root;
      const inner = this._findParent(child, target);
      if (inner) return inner;
    }
    return null;
  }

  _findNodeInCurrentZoomByFrameId(frameId) {
    if (frameId === null || frameId === undefined) return null;
    if (frameId === -1) return this._currentZoomRoot();
    const stack = [this._currentZoomRoot()];
    while (stack.length > 0) {
      const n = stack.pop();
      if (n.frameId === frameId) return n;
      for (const c of n.children.values()) stack.push(c);
    }
    return null;
  }

  _currentZoomRoot() {
    let cursor = this.tree;
    for (const fid of this.zoomPath) {
      const next = cursor.children.get(fid);
      if (!next) break;
      cursor = next;
    }
    return cursor;
  }

  _emit(name, detail) {
    const set = this._listeners.get(name);
    if (!set) return;
    for (const fn of set) {
      try { fn(detail); } catch (_) { /* swallow */ }
    }
  }
}

// =============================================================================
// Helpers.
// =============================================================================

function hashString(s) {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = (h + ((h << 1) + (h << 4) + (h << 7) + (h << 8) + (h << 24))) >>> 0;
  }
  return h;
}

function escapeText(s) {
  return String(s ?? '').replace(/[&<>"']/g, (ch) => {
    switch (ch) {
      case '&': return '&amp;';
      case '<': return '&lt;';
      case '>': return '&gt;';
      case '"': return '&quot;';
      default: return '&#39;';
    }
  });
}

// =============================================================================
// FlamegraphView — adapter for profile-report dispatcher.
// Dispatcher (renderLazyTab) wymaga `render(host, ctx)`. Tutaj montujemy
// CpuFlamegraph w hostowym kontenerze i mapujemy ksztalt `ctx.report` (kompat
// z TimelineView) na argumenty konstruktora. names moze byc obiektem (rkyv)
// albo tablica (fixtures) — flatten do tablicy zeby data lookup dzialal.
// =============================================================================

function namesToArray(names) {
  if (Array.isArray(names)) return names;
  if (names && typeof names === 'object') {
    const out = [];
    for (const [k, v] of Object.entries(names)) {
      const idx = Number(k);
      if (Number.isFinite(idx)) out[idx] = v;
    }
    return out;
  }
  return [];
}

export const FlamegraphView = {
  render(host, ctx) {
    if (!host) return;
    const report = ctx?.report || {};
    const events = ctx?.events || report.events || [];
    if (!events.length) {
      host.innerHTML = `
        <div class="pr-card">
          <div class="pr-banner-degraded">
            <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
            <div><strong>No CPU samples.</strong> This report contains no events for flamegraph aggregation.</div>
          </div>
        </div>`;
      return;
    }

    // Tear down poprzednia instancja zeby nie wyciekal singleton tooltipa
    // ani listenery na <tf-tabs> przy przelaczaniu zakladek.
    if (host._flamegraphInstance && typeof host._flamegraphInstance.destroy === 'function') {
      try { host._flamegraphInstance.destroy(); } catch (_) { /* noop */ }
      host._flamegraphInstance = null;
    }

    host.innerHTML = '';
    const card = document.createElement('div');
    card.className = 'pr-card';
    host.appendChild(card);

    const mount = document.createElement('div');
    card.appendChild(mount);

    const fg = new CpuFlamegraph(mount, {
      events,
      frames: Array.isArray(report.frames) ? report.frames : [],
      stacks: Array.isArray(report.stacks) ? report.stacks : [],
      names: namesToArray(report.names),
      totalDurationNs: Number(report.duration_ns) || 0,
      source: report.cpu_source || 'linux.perf.cpu_sampling',
      sampleHzApprox: Number(report.cpu_hz) || 0,
    });

    host._flamegraphInstance = fg;
  },
};

export default CpuFlamegraph;
