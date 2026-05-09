// =============================================================================
// Plik: modules/flows-builder/canvas.js
// Opis: Canvas Flow Buildera - renderowanie nodów jako HTML (position absolute),
//       krawędzi jako SVG bezier w WORLD coords, pan/zoom (wheel = zoom),
//       pointer-events z rAF throttle. Historia undo/redo in-memory.
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { getNodeDisplayTitle, isAutoNodeLabel } from '/js/modules/flows-builder/node-i18n.js';

const NODE_WIDTH = 280;
const NODE_H_APPROX = 110;
const GRID = 10;
const MAX_HISTORY = 30;
// Layout portów: header ma ~44px, porty zaczynają się od HEADER_OFFSET i są
// rozłożone co PORT_STEP pikseli.
const PORT_HEADER_OFFSET = 44;
const PORT_STEP = 22;
const DRAG_THRESHOLD = 4;

// Mapa category -> ikona (sprite id) używana dla renderowania nodów na canvas
const TYPE_ICON = {
  trigger: 'bolt',
  start: 'bolt',
  llm: 'chip',
  stt: 'mic',
  tts: 'speaker',
  memory: 'database',
  embeddings: 'sparkle',
  reranker: 'sparkle',
  condition: 'branch',
  switch: 'branch',
  template: 'code',
  transform: 'transform',
  pii_filter: 'shield',
  tts_clean: 'shield',
  router: 'transform',
  output: 'arrow-out',
  end: 'arrow-out',
  conversation_history: 'database',
  session_context: 'database',
  speaker_context: 'database',
  memory_analyzer: 'sparkle',
};

// Mapa node_type -> CSS var dla --node-color
function typeVar(type) {
  const map = {
    trigger: '--node-trigger',
    start: '--node-start',
    llm: '--node-llm',
    stt: '--node-stt',
    tts: '--node-tts',
    memory: '--node-memory',
    embeddings: '--node-embeddings',
    reranker: '--node-reranker',
    condition: '--node-condition',
    switch: '--node-switch',
    template: '--node-template',
    transform: '--node-transform',
    pii_filter: '--node-pii_filter',
    tts_clean: '--node-tts_clean',
    router: '--node-router',
    output: '--node-output',
    end: '--node-end',
    conversation_history: '--node-conversation_history',
    session_context: '--node-session_context',
    speaker_context: '--node-speaker_context',
    memory_analyzer: '--node-memory_analyzer',
  };
  return map[type] || '--node-llm';
}

// Kategoria -> krótka etykieta na nodzie
const TYPE_CATEGORY = {
  trigger: 'trigger', start: 'trigger',
  llm: 'ai', embeddings: 'ai', reranker: 'ai', stt: 'ai', tts: 'ai',
  memory: 'memory', conversation_history: 'memory', session_context: 'memory',
  speaker_context: 'memory', memory_analyzer: 'memory',
  condition: 'logic', switch: 'logic',
  template: 'transform', transform: 'transform', router: 'transform',
  pii_filter: 'filter', tts_clean: 'filter',
  output: 'output', end: 'output',
};

// Zwraca listę portów wejściowych/wyjściowych dla nody. Priorytet: adapter
// metadata z backendu (`template.input_ports`/`template.output_ports` plus
// `input_port_types`/`output_port_types`), potem legacy pola
// `inputs`/`outputs`, a na koncu heurystyki.
// Adapter metadata jest autorytatywnym zrodlem — backend odrzuci krawedz z
// nazwa portu spoza listy, wiec UI musi pokazywac dokladnie te porty.
// Kazdy port ma `type` (string `text`/`audio`/`image`/`video`/`embedding`/
// `other`/`json`/`any`) — uzywany do kolorowania i walidacji polaczenia
// po stronie GUI (lustrzana R8: `any` na ktorejkolwiek stronie = wildcard).
function portsForNode(node, template) {
  const isTrigger = node.type === 'trigger' || node.type === 'start';
  const isOutput = node.type === 'output' || node.type === 'end';

  const readList = (raw, types) => {
    if (!raw) return null;
    if (!Array.isArray(raw)) return null;
    return raw.map((p, i) => {
      const name = typeof p === 'string' ? p : p.name;
      const type = (Array.isArray(types) && typeof types[i] === 'string') ? types[i] : 'any';
      return { name, type };
    });
  };

  let tplIn = null;
  let tplOut = null;
  if (template) {
    if (Array.isArray(template.input_ports) && template.input_ports.length > 0) {
      tplIn = readList(template.input_ports, template.input_port_types);
    }
    if (Array.isArray(template.output_ports) && template.output_ports.length > 0) {
      tplOut = readList(template.output_ports, template.output_port_types);
    }
    if (!tplIn) tplIn = readList(template.inputs, null);
    if (!tplOut) tplOut = readList(template.outputs, null);
    if (!tplIn || !tplOut) {
      try {
        const schema = typeof template.params_schema === 'string'
          ? JSON.parse(template.params_schema)
          : template.params_schema;
        if (schema && schema.ports) {
          if (!tplIn) tplIn = readList(schema.ports.inputs, null);
          if (!tplOut) tplOut = readList(schema.ports.outputs, null);
        }
      } catch (_) {}
    }
  }

  const inputs = tplIn || (isTrigger ? [] : [{ name: 'in', type: 'any' }]);

  let outputs;
  if (tplOut) {
    outputs = tplOut;
  } else if (node.type === 'condition') {
    outputs = [{ name: 'true', type: 'any' }, { name: 'false', type: 'any' }];
  } else if (node.type === 'switch' || node.type === 'router') {
    const cases = Array.isArray(node.config?.cases) ? node.config.cases : [];
    if (cases.length > 0) {
      outputs = cases.map((c, i) => ({
        name: typeof c === 'string' ? c : (c.name || `case_${i + 1}`),
        type: 'any',
      }));
      outputs.push({ name: 'default', type: 'any' });
    } else {
      outputs = [{ name: 'case_1', type: 'any' }, { name: 'case_2', type: 'any' }, { name: 'default', type: 'any' }];
    }
  } else if (isOutput) {
    outputs = [];
  } else {
    outputs = [{ name: 'full', type: 'any' }];
  }

  return { inputs, outputs };
}

// Lustrzana walidacja R8 po stronie GUI: `any` na ktorejkolwiek stronie =
// wildcard, inaczej wymaga dokladnego match'a typow. Uzywane przy probie
// stworzenia krawedzi (drag-drop) — niedopasowane typy odrzucamy zanim
// uzytkownik puscic na portcie input.
function arePortTypesCompatible(fromType, toType) {
  const a = (fromType || 'any').toLowerCase();
  const b = (toType || 'any').toLowerCase();
  return a === 'any' || b === 'any' || a === b;
}

export class FlowCanvas {
  constructor(rootEl, opts = {}) {
    this.root = rootEl;
    this.opts = opts;
    this.nodes = [];
    this.edges = [];
    this.selectedIds = new Set();
    this.selectedEdgeId = null;
    this.view = { x: 0, y: 0, zoom: 1 };
    this.history = [];
    this.historyIndex = -1;
    this.onChange = opts.onChange || (() => {});
    this.onSelect = opts.onSelect || (() => {});
    this.onViewChange = opts.onViewChange || (() => {});
    this.templates = new Map(); // node_type -> template
    this._zTop = 1;

    this._rafPending = false;
    this._draggingNode = null;     // { ids, startClientX, startClientY, origs, moved, pointerId }
    this._connecting = null;        // { fromNode, fromPort, currentX, currentY, pointerId }
    this._panning = null;           // { startClientX, startClientY, origView, pointerId }
    this._pinch = null;             // { startDist, startZoom, cx, cy }
    this._pointerMovePending = null;
    this._suppressNextClick = false;

    this._buildDom();
    this._bindEvents();
  }

  setTemplates(list) {
    this.templates.clear();
    for (const t of list || []) {
      this.templates.set(t.node_type, t);
    }
    this._normalizeNodeLabels();
    this._normalizeEdgePorts();
  }

  _buildDom() {
    this.root.classList.add('fb-canvas');
    this.root.innerHTML = `
      <div class="fb-canvas-world">
        <svg class="fb-edges" xmlns="http://www.w3.org/2000/svg"></svg>
        <div class="fb-nodes-layer"></div>
      </div>
      <div class="fb-canvas-hint" data-role="hint">
        <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M12 5v14M5 12h14"/></svg>
        <div>${escapeHtml(I18n.t('flows_builder.canvas_hint'))}</div>
      </div>
    `;
    this.world = this.root.querySelector('.fb-canvas-world');
    this.svg = this.root.querySelector('.fb-edges');
    this.nodesLayer = this.root.querySelector('.fb-nodes-layer');
    this.hintEl = this.root.querySelector('[data-role="hint"]');
  }

  _bindEvents() {
    this._h = {
      pd: this._onPointerDown.bind(this),
      pm: this._onPointerMove.bind(this),
      pu: this._onPointerUp.bind(this),
      wh: this._onWheel.bind(this),
      ck: this._onClick.bind(this),
    };
    this.root.addEventListener('pointerdown', this._h.pd);
    window.addEventListener('pointermove', this._h.pm);
    window.addEventListener('pointerup', this._h.pu);
    window.addEventListener('pointercancel', this._h.pu);
    this.root.addEventListener('wheel', this._h.wh, { passive: false });
    this.root.addEventListener('click', this._h.ck);
    this._activePointers = new Map();
  }

  destroy() {
    if (this._h) {
      this.root.removeEventListener('pointerdown', this._h.pd);
      window.removeEventListener('pointermove', this._h.pm);
      window.removeEventListener('pointerup', this._h.pu);
      window.removeEventListener('pointercancel', this._h.pu);
      this.root.removeEventListener('wheel', this._h.wh);
      this.root.removeEventListener('click', this._h.ck);
    }
    this.root.innerHTML = '';
  }

  // -------------------------------------------------------------------------
  // Dane
  // -------------------------------------------------------------------------
  setData(nodes, edges, { reset = true } = {}) {
    // Seed/backend pisze pozycje jako `{position: {x, y}}` (zagniezdzone),
    // canvas renderuje przez flat `n.x`/`n.y`. Bez tej normalizacji wszystkie
    // nodes lecialy na (0,0) — w GUI wygladalo to jak pojedynczy node.
    this.nodes = (nodes || []).map((n) => {
      const pos = n.position || {};
      const x = typeof n.x === 'number' ? n.x : (typeof pos.x === 'number' ? pos.x : 0);
      const y = typeof n.y === 'number' ? n.y : (typeof pos.y === 'number' ? pos.y : 0);
      return { ...n, x, y, config: n.config || {} };
    });
    this.edges = (edges || []).map((e) => ({ ...e }));
    this._normalizeNodeLabels();
    this._normalizeEdgePorts();
    this.selectedIds.clear();
    this.selectedEdgeId = null;
    if (reset) {
      this.history = [];
      this.historyIndex = -1;
      this._pushHistory();
    }
    this.render();
  }

  getData() {
    this._normalizeEdgePorts();
    // Przy serializacji pomijamy porty rowne domyslnym ("full"/"in"), zeby
    // nie zasmiecac flow_json pusta metadata — backend rozumie brak pol
    // dzieki serde(default). Dzieki temu edge tak jak przed S4a round-trippuje
    // do bajtowo identycznego JSONu.
    return {
      nodes: this.nodes.map((n) => {
        // Backend FlowNode oczekuje `position: {x, y}` (custom deserializer
        // akceptuje tez tablicowy `[x,y]`, ale nie ma flat `x`/`y`). Bez
        // konwersji round-trip gubi pozycje — node po save'ie wraca bez
        // wspolrzednych i renderuje sie na (0,0).
        const { x, y, ...rest } = n;
        return {
          ...rest,
          position: { x: Number(x) || 0, y: Number(y) || 0 },
          config: { ...n.config },
        };
      }),
      edges: this.edges.map((e) => {
        const out = { ...e };
        if (out.from_port === 'full') delete out.from_port;
        if (out.to_port === 'in') delete out.to_port;
        return out;
      }),
    };
  }

  // Waliduje klient-side przed zapisem: kazdy edge musi wskazywac istniejace
  // node'y i porty obecne w adapter metadata. Zwraca liste bledow jako
  // stringi (juz zlokalizowane) — pusta lista oznacza flow gotowy do zapisu.
  validate() {
    this._normalizeEdgePorts();
    const errors = [];
    const nodeById = new Map(this.nodes.map((n) => [n.id, n]));
    for (const edge of this.edges) {
      const from = nodeById.get(edge.from_node);
      const to = nodeById.get(edge.to_node);
      if (!from || !to) {
        errors.push(I18n.t('flows_builder.edge_dangling'));
        continue;
      }
      const fromTpl = this.templates.get(from.type);
      const toTpl = to ? this.templates.get(to.type) : null;
      const fromPort = edge.from_port || 'full';
      const toPort = edge.to_port || 'in';
      if (fromTpl && Array.isArray(fromTpl.output_ports) && fromTpl.output_ports.length > 0) {
        if (!fromTpl.output_ports.includes(fromPort)) {
          errors.push(I18n.t('flows_builder.invalid_port', { node_type: from.type, port: fromPort }));
        }
      }
      if (toTpl && Array.isArray(toTpl.input_ports) && toTpl.input_ports.length > 0) {
        if (!toTpl.input_ports.includes(toPort)) {
          errors.push(I18n.t('flows_builder.invalid_port', { node_type: to.type, port: toPort }));
        }
      }
    }
    // Wykrywanie cykli — DFS z kolorowaniem; zwracamy pojedynczy blad jesli
    // jakikolwiek cykl istnieje, bo lista cykli nie wnosi wiecej dla usera.
    const adj = new Map();
    for (const e of this.edges) {
      if (!adj.has(e.from_node)) adj.set(e.from_node, []);
      adj.get(e.from_node).push(e.to_node);
    }
    const WHITE = 0, GRAY = 1, BLACK = 2;
    const color = new Map(this.nodes.map((n) => [n.id, WHITE]));
    const dfs = (id) => {
      color.set(id, GRAY);
      for (const next of adj.get(id) || []) {
        const c = color.get(next);
        if (c === GRAY) return true;
        if (c === WHITE && dfs(next)) return true;
      }
      color.set(id, BLACK);
      return false;
    };
    for (const n of this.nodes) {
      if (color.get(n.id) === WHITE && dfs(n.id)) {
        errors.push(I18n.t('flows_builder.cycle_detected'));
        break;
      }
    }
    return errors;
  }

  _normalizeEdgePorts() {
    if (!Array.isArray(this.edges) || this.edges.length === 0) return;
    const nodeById = new Map(this.nodes.map((n) => [n.id, n]));
    for (const edge of this.edges) {
      const from = nodeById.get(edge.from_node);
      const to = nodeById.get(edge.to_node);
      if (from) {
        const fromTpl = this.templates.get(from.type);
        const { outputs } = portsForNode(from, fromTpl);
        const outputNames = outputs.map((p) => p.name);
        const currentFrom = edge.from_port || 'full';
        if (!outputNames.includes(currentFrom)) {
          if (outputNames.length === 1) edge.from_port = outputNames[0];
          else if (outputNames.includes('full')) edge.from_port = 'full';
        } else {
          edge.from_port = currentFrom;
        }
      }
      if (to) {
        const toTpl = this.templates.get(to.type);
        const { inputs } = portsForNode(to, toTpl);
        const inputNames = inputs.map((p) => p.name);
        const currentTo = edge.to_port || 'in';
        if (!inputNames.includes(currentTo)) {
          if (inputNames.length === 1) edge.to_port = inputNames[0];
          else if (inputNames.includes('in')) edge.to_port = 'in';
        } else {
          edge.to_port = currentTo;
        }
      }
    }
  }

  _pushHistory() {
    // Odcinamy redo-branche po nowej operacji. structuredClone zamiast
    // JSON round-trip — szybszy binary copy + zachowuje typy Date/Map jesli
    // kiedys trafia do node.config (JSON.stringify gubi wszystko nieprymitywne).
    this.history = this.history.slice(0, this.historyIndex + 1);
    this.history.push(structuredClone({ nodes: this.nodes, edges: this.edges }));
    if (this.history.length > MAX_HISTORY) this.history.shift();
    this.historyIndex = this.history.length - 1;
  }

  undo() {
    if (this.historyIndex <= 0) return;
    this.historyIndex -= 1;
    const snap = structuredClone(this.history[this.historyIndex]);
    this.nodes = snap.nodes;
    this.edges = snap.edges;
    this.selectedIds.clear();
    this.selectedEdgeId = null;
    this.render();
    this.onChange();
  }

  redo() {
    if (this.historyIndex >= this.history.length - 1) return;
    this.historyIndex += 1;
    const snap = structuredClone(this.history[this.historyIndex]);
    this.nodes = snap.nodes;
    this.edges = snap.edges;
    this.render();
    this.onChange();
  }

  // -------------------------------------------------------------------------
  // CRUD nody / krawędzie
  // -------------------------------------------------------------------------
  addNodeFromTemplate(tpl, clientX, clientY) {
    const pt = this._clientToWorld(clientX, clientY);
    let defaultConfig = {};
    try { defaultConfig = JSON.parse(tpl.default_config || '{}'); } catch (_) {}
    const node = {
      id: 'n_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 6),
      type: tpl.node_type,
      label: '',
      x: Math.round((pt.x - NODE_WIDTH / 2) / GRID) * GRID,
      y: Math.round((pt.y - NODE_H_APPROX / 2) / GRID) * GRID,
      config: defaultConfig,
    };
    this.nodes.push(node);
    this._pushHistory();
    this.render();
    this.selectNode(node.id);
    this.onChange();
    return node;
  }

  updateNodeConfig(nodeId, patch) {
    const n = this.nodes.find((x) => x.id === nodeId);
    if (!n) return;
    // Custom elementy (tf-toggle/tf-select/tf-input) potrafia odpalic `change`
    // przy initial populate w FlowConfig.show — bez no-op guardu to
    // triggerowalo `_pushHistory + _renderSingleNode + onChange`, ktore
    // odbudowywaly DOM node'a i zrywaly selection ~sekunde po kliknieciu.
    let changed = false;
    for (const k of Object.keys(patch)) {
      if (n.config?.[k] !== patch[k]) { changed = true; break; }
    }
    if (!changed) return;
    n.config = { ...n.config, ...patch };
    this._pushHistory();
    this._renderSingleNode(n);
    this.onChange();
  }

  updateNodeLabel(nodeId, label) {
    const n = this.nodes.find((x) => x.id === nodeId);
    if (!n) return;
    if ((n.label || '') === (label || '')) return;
    n.label = label;
    this._pushHistory();
    this._renderSingleNode(n);
    this.onChange();
  }

  _normalizeNodeLabels() {
    if (!Array.isArray(this.nodes) || this.nodes.length === 0) return;
    for (const node of this.nodes) {
      const template = this.templates.get(node.type);
      if (isAutoNodeLabel(node.label, node.type, template?.label)) {
        node.label = '';
      }
    }
  }

  removeNodes(ids) {
    const idSet = new Set(ids);
    this.nodes = this.nodes.filter((n) => !idSet.has(n.id));
    this.edges = this.edges.filter((e) => !idSet.has(e.from_node) && !idSet.has(e.to_node));
    this.selectedIds.clear();
    this._pushHistory();
    this.render();
    this.onChange();
  }

  duplicateNodes(ids) {
    const idMap = new Map();
    const clones = [];
    for (const id of ids) {
      const n = this.nodes.find((x) => x.id === id);
      if (!n) continue;
      const clone = {
        ...n,
        id: 'n_' + Date.now().toString(36) + Math.random().toString(36).slice(2, 5),
        x: n.x + 30,
        y: n.y + 30,
        config: { ...n.config },
      };
      idMap.set(n.id, clone.id);
      clones.push(clone);
    }
    this.nodes.push(...clones);
    this.selectedIds = new Set(clones.map((c) => c.id));
    this._pushHistory();
    this.render();
    this.onChange();
  }

  deleteSelected() {
    if (this.selectedIds.size > 0) {
      this.removeNodes([...this.selectedIds]);
      return;
    }
    if (this.selectedEdgeId) {
      this.edges = this.edges.filter((e) => e.id !== this.selectedEdgeId);
      this.selectedEdgeId = null;
      this._pushHistory();
      this.render();
      this.onChange();
    }
  }

  // -------------------------------------------------------------------------
  // Selekcja
  // -------------------------------------------------------------------------
  selectNode(id, { additive = false } = {}) {
    if (!additive) this.selectedIds.clear();
    if (id) this.selectedIds.add(id);
    this.selectedEdgeId = null;
    this._applySelectionClasses();
    const node = this.selectedIds.size === 1
      ? this.nodes.find((n) => n.id === [...this.selectedIds][0])
      : null;
    this.onSelect(node);
  }

  clearSelection() {
    this.selectedIds.clear();
    this.selectedEdgeId = null;
    this._applySelectionClasses();
    this.onSelect(null);
  }

  _applySelectionClasses() {
    this.nodesLayer.querySelectorAll('.fb-node').forEach((el) => {
      el.classList.toggle('selected', this.selectedIds.has(el.dataset.nodeId));
    });
    this.svg.querySelectorAll('.fb-edge-path').forEach((p) => {
      p.classList.toggle('selected', p.dataset.edgeId === this.selectedEdgeId);
    });
  }

  _bringToFront(nodeEl) {
    this._zTop += 1;
    nodeEl.style.zIndex = String(this._zTop);
  }

  // -------------------------------------------------------------------------
  // Rendering
  // -------------------------------------------------------------------------
  render() {
    this._renderNodes();
    this._renderEdges();
    this._applyView();
    if (this.hintEl) this.hintEl.style.display = this.nodes.length === 0 ? 'flex' : 'none';
  }

  _renderNodes() {
    this.nodesLayer.innerHTML = '';
    for (const n of this.nodes) {
      this.nodesLayer.appendChild(this._buildNodeEl(n));
    }
    this._applySelectionClasses();
  }

  _renderSingleNode(n) {
    const old = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(n.id)}"]`);
    const fresh = this._buildNodeEl(n);
    if (old) old.replaceWith(fresh);
    else this.nodesLayer.appendChild(fresh);
    this._applySelectionClasses();
    this._renderEdges();
  }

  _buildNodeEl(n) {
    const div = document.createElement('div');
    div.className = 'fb-node';
    div.dataset.nodeId = n.id;
    div.style.left = `${n.x}px`;
    div.style.top = `${n.y}px`;
    div.style.width = `${NODE_WIDTH}px`;
    div.style.setProperty('--node-color', `var(${typeVar(n.type)})`);

    if (this._hasError(n)) div.classList.add('error');

    const iconId = TYPE_ICON[n.type] || 'chip';
    const cat = TYPE_CATEGORY[n.type] || n.type;
    const tmpl = this.templates.get(n.type);
    const title = getNodeDisplayTitle(n, tmpl);

    const { inputs, outputs } = portsForNode(n, tmpl);

    // Wysokosc nody musi pomiescic obie strony portow (in/out). Naszym
    // pivotem jest strona z wieksza liczba portow. Header to PORT_HEADER_OFFSET
    // (44px) + 14px stopka per port. Bez tego node ma sztywne CSS height i
    // przy 6+ portach (trigger) ostatnie wystaja na zewnatrz dolnej krawedzi.
    const portCount = Math.max(inputs.length, outputs.length, 1);
    const minHeight = PORT_HEADER_OFFSET + portCount * PORT_STEP + 14;
    div.style.minHeight = `${minHeight}px`;

    const inPortsHtml = inputs.map((p, i) => this._renderPortEl(n.id, p, i, 'in', inputs.length)).join('');
    const outPortsHtml = outputs.map((p, i) => this._renderPortEl(n.id, p, i, 'out', outputs.length)).join('');

    div.innerHTML = `
      <div class="fb-node-header">
        <div class="fb-node-badge"><svg><use href="#i-${iconId}"/></svg></div>
        <div class="fb-node-title">${escapeHtml(title)}</div>
        ${this._hasError(n)
          ? `<span class="fb-node-error-icon" title="${escapeAttr(I18n.t('flows_builder.node_error_tooltip'))}"><svg><use href="#i-alert"/></svg></span>`
          : `<span class="fb-node-type">${escapeHtml(cat)}</span>`}
      </div>
      <div class="fb-node-body">${this._renderNodeSummary(n)}</div>
      ${inPortsHtml}
      ${outPortsHtml}
    `;
    return div;
  }

  _renderPortEl(nodeId, port, idx, side, total) {
    const top = PORT_HEADER_OFFSET + idx * PORT_STEP;
    const showLabel = total >= 2;
    const tooltipKey = port.name === 'stream' ? 'flows_builder.port_stream'
      : port.name === 'full' ? 'flows_builder.port_full'
      : port.name === 'in' ? 'flows_builder.port_in'
      : null;
    const portLabel = tooltipKey ? I18n.t(tooltipKey) : port.name;
    const portType = (port.type || 'any').toLowerCase();
    // Tooltip pokazuje nazwe portu + typ danych zeby uzytkownik widzial
    // dlaczego port ma kolor X (np. "audio • Audio").
    const tooltip = `${portLabel} • ${portType}`;
    const labelHtml = showLabel
      ? `<span class="fb-port-label">${escapeHtml(port.name)}</span>`
      : '';
    const cls = `fb-port fb-port-${side === 'in' ? 'in' : 'out'} fb-port-type-${portType}`;
    return `<div class="${cls}" data-node-id="${escapeAttr(nodeId)}" data-port="${escapeAttr(port.name)}" data-port-kind="${escapeAttr(port.name)}" data-port-type="${escapeAttr(portType)}" data-port-idx="${idx}" style="top:${top}px;" title="${escapeAttr(tooltip)}">${labelHtml}</div>`;
  }

  _renderNodeSummary(n) {
    const c = n.config || {};
    const keys = Object.keys(c).filter((k) => c[k] !== null && c[k] !== undefined && c[k] !== '');
    if (keys.length === 0) return '<div class="fb-node-row"><span class="fb-key">—</span></div>';
    return keys.slice(0, 4).map((k) => {
      const v = c[k];
      const s = typeof v === 'object' ? JSON.stringify(v) : String(v);
      // Truncate w CSS przez ellipsę — tu tylko udostępniamy pełną wartość w title
      return `<div class="fb-node-row" title="${escapeAttr(k + ': ' + s)}"><span class="fb-key">${escapeHtml(k)}</span><span class="fb-val">${escapeHtml(s)}</span></div>`;
    }).join('');
  }

  _hasError(n) {
    const tmpl = this.templates.get(n.type);
    if (!tmpl || !tmpl.params_schema) return false;
    let schema;
    try { schema = typeof tmpl.params_schema === 'string' ? JSON.parse(tmpl.params_schema) : tmpl.params_schema; }
    catch (_) { return false; }
    const required = schema.required || [];
    for (const key of required) {
      const v = n.config?.[key];
      if (v === null || v === undefined || v === '') return true;
    }
    return false;
  }

  // Zwraca pozycję portu w WORLD coords (bez pan/zoom). Portu szukamy po
  // nazwie; jesli nie ma — bierzemy pierwszy z listy (fallback dla krawedzi
  // zapisanych pod inna nazwa portu, np. legacy "out"). Lokalizacja
  // geometryczna jest tylko sprawa rysowania — walidacja nazw odbywa sie
  // osobno w _validate().
  _getPortWorldPos(nodeId, portName, side) {
    const node = this.nodes.find((n) => n.id === nodeId);
    if (!node) return { x: 0, y: 0 };
    const tmpl = this.templates.get(node.type);
    const { inputs, outputs } = portsForNode(node, tmpl);
    const list = side === 'in' ? inputs : outputs;
    let idx = list.findIndex((p) => p.name === portName);
    if (idx < 0) idx = 0;
    const worldX = side === 'in' ? node.x : node.x + NODE_WIDTH;
    // +8 bo port ma 16px i jego centrum jest w top + 8
    const worldY = node.y + PORT_HEADER_OFFSET + idx * PORT_STEP + 8;
    return { x: worldX, y: worldY };
  }

  _renderEdges() {
    // Zbudujmy SVG na nowo — edges zwykle nieliczne.
    const svgNs = 'http://www.w3.org/2000/svg';
    this.svg.innerHTML = '';
    for (const e of this.edges) {
      const from = this.nodes.find((n) => n.id === e.from_node);
      const to = this.nodes.find((n) => n.id === e.to_node);
      if (!from || !to) continue;
      const fp = this._getPortWorldPos(e.from_node, e.from_port || 'full', 'out');
      const tp = this._getPortWorldPos(e.to_node, e.to_port || 'in', 'in');
      const d = this._bezierPath(fp.x, fp.y, tp.x, tp.y);
      // Hit area
      const hit = document.createElementNS(svgNs, 'path');
      hit.setAttribute('class', 'fb-edge-hit');
      hit.setAttribute('d', d);
      hit.dataset.edgeId = e.id;
      this.svg.appendChild(hit);
      // Visible path
      const p = document.createElementNS(svgNs, 'path');
      p.setAttribute('class', 'fb-edge-path');
      p.setAttribute('d', d);
      p.dataset.edgeId = e.id;
      const isSelected = this.selectedEdgeId === e.id;
      if (isSelected) p.classList.add('selected');
      this.svg.appendChild(p);
      // Animowana kropka przepływu — pointer-events:none w CSS, zeby nie
      // przesłaniała hit-area i nie blokowała kliku w edge.
      const dot = document.createElementNS(svgNs, 'circle');
      dot.setAttribute('class', 'fb-edge-flow');
      dot.setAttribute('r', '3');
      const anim = document.createElementNS(svgNs, 'animateMotion');
      anim.setAttribute('dur', '2.4s');
      anim.setAttribute('repeatCount', 'indefinite');
      anim.setAttribute('path', d);
      dot.appendChild(anim);
      this.svg.appendChild(dot);
      // Przycisk delete (X) na srodku krawedzi gdy selected — kazda
      // krawedz ma takze hover-interaktywny target przez .fb-edge-hit:hover
      // w CSS, ale realny przycisk jest renderowany dopiero po selekcji
      // (po pierwszym kliku w edge). Klik w X usuwa krawedz.
      if (isSelected) {
        const mid = this._bezierMidpoint(fp.x, fp.y, tp.x, tp.y);
        const g = document.createElementNS(svgNs, 'g');
        g.setAttribute('class', 'fb-edge-delete');
        g.dataset.edgeId = e.id;
        g.setAttribute('transform', `translate(${mid.x}, ${mid.y})`);
        const bg = document.createElementNS(svgNs, 'circle');
        bg.setAttribute('r', '11');
        bg.setAttribute('class', 'fb-edge-delete-bg');
        const x1 = document.createElementNS(svgNs, 'line');
        x1.setAttribute('x1', '-4'); x1.setAttribute('y1', '-4');
        x1.setAttribute('x2', '4'); x1.setAttribute('y2', '4');
        x1.setAttribute('class', 'fb-edge-delete-stroke');
        const x2 = document.createElementNS(svgNs, 'line');
        x2.setAttribute('x1', '-4'); x2.setAttribute('y1', '4');
        x2.setAttribute('x2', '4'); x2.setAttribute('y2', '-4');
        x2.setAttribute('class', 'fb-edge-delete-stroke');
        g.appendChild(bg);
        g.appendChild(x1);
        g.appendChild(x2);
        this.svg.appendChild(g);
      }
    }
    // Tymczasowa linia podczas łączenia
    if (this._connecting) {
      const { fromNode, fromPort, currentX, currentY } = this._connecting;
      const fp = this._getPortWorldPos(fromNode.id, fromPort, 'out');
      const d = this._bezierPath(fp.x, fp.y, currentX, currentY);
      const p = document.createElementNS(svgNs, 'path');
      p.setAttribute('class', 'fb-edge-path temp');
      p.setAttribute('d', d);
      this.svg.appendChild(p);
    }
  }

  _bezierPath(x1, y1, x2, y2) {
    const dx = Math.max(40, Math.abs(x2 - x1) * 0.5);
    return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
  }

  /// Punkt na krzywej Beziera w t=0.5 — dla cubic z punktami kontrolnymi
  /// `(P0, P1, P2, P3)` gdzie P1=(x1+dx,y1) i P2=(x2-dx,y2). Wzor cubic
  /// Bezier dla t=0.5: B = (P0+3P1+3P2+P3)/8.
  _bezierMidpoint(x1, y1, x2, y2) {
    const dx = Math.max(40, Math.abs(x2 - x1) * 0.5);
    const c1x = x1 + dx, c1y = y1;
    const c2x = x2 - dx, c2y = y2;
    return {
      x: (x1 + 3 * c1x + 3 * c2x + x2) / 8,
      y: (y1 + 3 * c1y + 3 * c2y + y2) / 8,
    };
  }

  // -------------------------------------------------------------------------
  // Pan / zoom
  // -------------------------------------------------------------------------
  _applyView() {
    this.world.style.transform = `translate(${this.view.x}px, ${this.view.y}px) scale(${this.view.zoom})`;
    this.onViewChange(this.view);
  }

  setZoom(zoom, cx, cy) {
    const rect = this.root.getBoundingClientRect();
    if (cx === undefined) cx = rect.width / 2;
    if (cy === undefined) cy = rect.height / 2;
    const z = Math.max(0.2, Math.min(3, zoom));
    // Zoom względem punktu (cx,cy) w lokalnych współrz. canvasu
    const worldX = (cx - this.view.x) / this.view.zoom;
    const worldY = (cy - this.view.y) / this.view.zoom;
    this.view.x = cx - worldX * z;
    this.view.y = cy - worldY * z;
    this.view.zoom = z;
    this._applyView();
  }

  zoomBy(factor) {
    this.setZoom(this.view.zoom * factor);
  }

  resetZoom() {
    this.view = { x: 0, y: 0, zoom: 1 };
    this._applyView();
  }

  fitToContent() {
    if (this.nodes.length === 0) { this.resetZoom(); return; }
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const n of this.nodes) {
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + NODE_WIDTH);
      maxY = Math.max(maxY, n.y + NODE_H_APPROX);
    }
    const pad = 40;
    const rect = this.root.getBoundingClientRect();
    const w = maxX - minX + pad * 2;
    const h = maxY - minY + pad * 2;
    const z = Math.min(1, Math.min(rect.width / w, rect.height / h));
    this.view.zoom = z;
    this.view.x = (rect.width - (maxX + minX) * z) / 2;
    this.view.y = (rect.height - (maxY + minY) * z) / 2;
    this._applyView();
  }

  _clientToWorld(clientX, clientY) {
    const rect = this.root.getBoundingClientRect();
    return {
      x: (clientX - rect.left - this.view.x) / this.view.zoom,
      y: (clientY - rect.top - this.view.y) / this.view.zoom,
    };
  }

  // -------------------------------------------------------------------------
  // Zdarzenia pointer
  // -------------------------------------------------------------------------
  _onPointerDown(ev) {
    if (ev.button !== undefined && ev.button !== 0) return;
    this._activePointers.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });

    // Pinch start
    if (this._activePointers.size === 2) {
      const pts = [...this._activePointers.values()];
      const dist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
      const cx = (pts[0].x + pts[1].x) / 2;
      const cy = (pts[0].y + pts[1].y) / 2;
      const rect = this.root.getBoundingClientRect();
      this._pinch = { startDist: dist, startZoom: this.view.zoom, cx: cx - rect.left, cy: cy - rect.top };
      this._draggingNode = null;
      this._panning = null;
      this._connecting = null;
      return;
    }

    // 1) Port out → start łączenia. setPointerCapture jest tu potrzebne
    // bo pointermove musi miec gwarancje delivery do roota nawet gdy
    // kursor wyjdzie poza canvas (drag-line do drugiego node'a).
    const portOut = ev.target.closest('.fb-port-out');
    if (portOut) {
      const nodeId = portOut.dataset.nodeId;
      const node = this.nodes.find((n) => n.id === nodeId);
      if (node) {
        const pt = this._clientToWorld(ev.clientX, ev.clientY);
        this._connecting = {
          fromNode: node,
          fromPort: portOut.dataset.port || 'full',
          currentX: pt.x,
          currentY: pt.y,
          pointerId: ev.pointerId,
        };
        try { this.root.setPointerCapture(ev.pointerId); } catch (_) {}
        // Wyłącz hit-paths edge'ów na czas łączenia, żeby elementFromPoint trafiał w port-in
        this.root.classList.add('connecting');
        ev.preventDefault();
      }
      return;
    }

    // 2) Port in → nic, obsłużony przy release łączenia
    if (ev.target.closest('.fb-port-in')) return;

    // 3) Klik w krawedz (SVG fb-edge-hit) — NIE rob panning, NIE rob
    // setPointerCapture, NIE preventDefault. Zostawiamy przegladarce
    // native dispatch zeby click event mial poprawny target = edge-hit
    // (setPointerCapture przekierowywaloby click na roota).
    if (ev.target.closest('.fb-edge-hit') || ev.target.closest('.fb-edge-delete')) {
      return;
    }

    // 4) Node → drag (ignoruj interaktywne elementy wewnątrz). Brak
    // setPointerCapture: pointermove/pointerup sa juz podpiete na window
    // (linia 236-238), wiec drag dziala bez capture'a, a click na koniec
    // ma poprawny ev.target.
    const nodeEl = ev.target.closest('.fb-node');
    if (nodeEl) {
      if (ev.target.closest('input, select, textarea, button, tf-button, [data-no-drag]')) return;
      const id = nodeEl.dataset.nodeId;
      const additive = ev.shiftKey;
      if (!this.selectedIds.has(id)) this.selectNode(id, { additive });
      this._bringToFront(nodeEl);
      const origs = new Map();
      for (const sid of this.selectedIds) {
        const nd = this.nodes.find((n) => n.id === sid);
        if (nd) origs.set(sid, { x: nd.x, y: nd.y });
      }
      this._draggingNode = {
        ids: [...this.selectedIds],
        startClientX: ev.clientX,
        startClientY: ev.clientY,
        origs,
        moved: false,
        pointerId: ev.pointerId,
      };
      // preventDefault tylko zeby zatrzymac native text selection na node
      // labelach przy dlugim drag — NIE blokuje click bo standard W3C.
      ev.preventDefault();
      return;
    }

    // 5) Pusty canvas → pan. Bez setPointerCapture (jak w node-drag).
    this._panning = {
      startClientX: ev.clientX,
      startClientY: ev.clientY,
      origView: { ...this.view },
      pointerId: ev.pointerId,
      moved: false,
    };
    this.root.classList.add('panning');
    ev.preventDefault();
  }

  _onPointerMove(ev) {
    if (this._activePointers.has(ev.pointerId)) {
      this._activePointers.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });
    }

    if (this._pinch && this._activePointers.size === 2) {
      const pts = [...this._activePointers.values()];
      const dist = Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
      const factor = dist / this._pinch.startDist;
      this.setZoom(this._pinch.startZoom * factor, this._pinch.cx, this._pinch.cy);
      return;
    }

    this._pointerMovePending = { clientX: ev.clientX, clientY: ev.clientY };
    if (!this._rafPending) {
      this._rafPending = true;
      requestAnimationFrame(() => {
        this._rafPending = false;
        const p = this._pointerMovePending;
        if (!p) return;
        this._flushPointerMove(p.clientX, p.clientY);
      });
    }
  }

  _flushPointerMove(clientX, clientY) {
    if (this._draggingNode) {
      const dx = (clientX - this._draggingNode.startClientX) / this.view.zoom;
      const dy = (clientY - this._draggingNode.startClientY) / this.view.zoom;
      if (!this._draggingNode.moved && Math.hypot(dx, dy) * this.view.zoom < DRAG_THRESHOLD) return;
      this._draggingNode.moved = true;
      for (const id of this._draggingNode.ids) {
        const n = this.nodes.find((x) => x.id === id);
        const orig = this._draggingNode.origs.get(id);
        if (!n || !orig) continue;
        n.x = Math.round((orig.x + dx) / GRID) * GRID;
        n.y = Math.round((orig.y + dy) / GRID) * GRID;
        const el = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(id)}"]`);
        if (el) {
          el.style.left = `${n.x}px`;
          el.style.top = `${n.y}px`;
          el.classList.add('dragging');
        }
      }
      this._renderEdges();
      return;
    }

    if (this._connecting) {
      const pt = this._clientToWorld(clientX, clientY);
      this._connecting.currentX = pt.x;
      this._connecting.currentY = pt.y;
      this._renderEdges();
      // Hover target port
      const under = document.elementFromPoint(clientX, clientY);
      document.querySelectorAll('.fb-port.drop-target').forEach((p) => p.classList.remove('drop-target'));
      const targetPort = under?.closest('.fb-port-in');
      if (targetPort) targetPort.classList.add('drop-target');
      return;
    }

    if (this._panning) {
      const ddx = clientX - this._panning.startClientX;
      const ddy = clientY - this._panning.startClientY;
      if (!this._panning.moved && Math.hypot(ddx, ddy) < DRAG_THRESHOLD) return;
      this._panning.moved = true;
      this.view.x = this._panning.origView.x + ddx;
      this.view.y = this._panning.origView.y + ddy;
      this._applyView();
    }
  }

  _onPointerUp(ev) {
    this._activePointers.delete(ev.pointerId);
    if (this._activePointers.size < 2) this._pinch = null;

    if (this._draggingNode && this._draggingNode.pointerId === ev.pointerId) {
      this.nodesLayer.querySelectorAll('.fb-node.dragging').forEach((el) => el.classList.remove('dragging'));
      if (this._draggingNode.moved) {
        this._pushHistory();
        this.onChange();
        this._suppressNextClick = true;
      }
      this._draggingNode = null;
    }

    if (this._connecting && this._connecting.pointerId === ev.pointerId) {
      // Zdjęcie klasy musi być bezwarunkowe — także gdy release jest poza portem (cancel)
      this.root.classList.remove('connecting');
      const target = document.elementFromPoint(ev.clientX, ev.clientY);
      const portIn = target?.closest?.('.fb-port-in');
      if (portIn) {
        const toNodeId = portIn.dataset.nodeId;
        const toPort = portIn.dataset.port || 'in';
        const fromNode = this._connecting.fromNode;
        const fromPort = this._connecting.fromPort;
        if (toNodeId && toNodeId !== fromNode.id) {
          const toNode = this.nodes.find((n) => n.id === toNodeId);
          const fromTpl = this.templates.get(fromNode.type);
          const toTpl = toNode ? this.templates.get(toNode.type) : null;
          const fromOutputs = (fromTpl && Array.isArray(fromTpl.output_ports) && fromTpl.output_ports.length > 0) ? fromTpl.output_ports : null;
          const toInputs = (toTpl && Array.isArray(toTpl.input_ports) && toTpl.input_ports.length > 0) ? toTpl.input_ports : null;
          let rejectMsg = null;
          if (fromOutputs && !fromOutputs.includes(fromPort)) {
            rejectMsg = I18n.t('flows_builder.invalid_port', { node_type: fromNode.type, port: fromPort });
          } else if (toInputs && !toInputs.includes(toPort)) {
            rejectMsg = I18n.t('flows_builder.invalid_port', { node_type: toNode.type, port: toPort });
          } else {
            // Walidacja typed (lustrzana R8 z backendu): port producenta i
            // konsumenta musza miec kompatybilne typy. `any` na ktorejkolwiek
            // stronie to wildcard. Bez tego user moglby polaczyc Audio do
            // Text node'a a backend rzucilby blad dopiero przy save.
            const fromIdx = fromOutputs ? fromOutputs.indexOf(fromPort) : -1;
            const toIdx = toInputs ? toInputs.indexOf(toPort) : -1;
            const fromType = (fromIdx >= 0 && Array.isArray(fromTpl?.output_port_types))
              ? fromTpl.output_port_types[fromIdx] : 'any';
            const toType = (toIdx >= 0 && Array.isArray(toTpl?.input_port_types))
              ? toTpl.input_port_types[toIdx] : 'any';
            if (!arePortTypesCompatible(fromType, toType)) {
              rejectMsg = I18n.t('flows_builder.invalid_port_type', {
                from_type: fromType,
                to_type: toType,
              }) || `Niekompatybilne typy: ${fromType} → ${toType}`;
            }
          }
          if (rejectMsg) {
            this.opts.onInvalidConnection?.(rejectMsg);
          } else {
            const exists = this.edges.some((e) =>
              e.from_node === fromNode.id
              && e.to_node === toNodeId
              && e.from_port === fromPort
              && e.to_port === toPort);
            if (!exists) {
              this.edges.push({
                id: 'e_' + Date.now().toString(36),
                from_node: fromNode.id,
                to_node: toNodeId,
                from_port: fromPort,
                to_port: toPort,
              });
              this._pushHistory();
              this.onChange();
            }
          }
        }
      }
      document.querySelectorAll('.fb-port.drop-target').forEach((p) => p.classList.remove('drop-target'));
      this._connecting = null;
      this._renderEdges();
    }

    if (this._panning && this._panning.pointerId === ev.pointerId) {
      if (this._panning.moved) this._suppressNextClick = true;
      this._panning = null;
      this.root.classList.remove('panning');
    }
  }

  _onClick(ev) {
    if (this._suppressNextClick) { this._suppressNextClick = false; return; }
    // Klik w X-przycisk na srodku selected edge'a → usun krawedz.
    const deleteBtn = ev.target.closest('.fb-edge-delete');
    if (deleteBtn) {
      const edgeId = deleteBtn.dataset.edgeId;
      this.edges = this.edges.filter((e) => e.id !== edgeId);
      this.selectedEdgeId = null;
      this._pushHistory();
      this._renderEdges();
      this.onChange();
      return;
    }
    const hit = ev.target.closest('.fb-edge-hit');
    if (hit) {
      this.selectedIds.clear();
      this.selectedEdgeId = hit.dataset.edgeId;
      this._applySelectionClasses();
      this._renderEdges();
      this.onSelect(null);
      return;
    }
    const nodeEl = ev.target.closest('.fb-node');
    if (nodeEl) {
      this.selectNode(nodeEl.dataset.nodeId, { additive: ev.shiftKey });
      return;
    }
    // Pusty canvas → deselect
    this.clearSelection();
  }

  _onWheel(ev) {
    // Wheel = zoom zawsze (modifier klawiszowy pominięty — wygoda edytora).
    ev.preventDefault();
    const factor = ev.deltaY < 0 ? 1.1 : 1 / 1.1;
    const rect = this.root.getBoundingClientRect();
    this.setZoom(this.view.zoom * factor, ev.clientX - rect.left, ev.clientY - rect.top);
  }
}
