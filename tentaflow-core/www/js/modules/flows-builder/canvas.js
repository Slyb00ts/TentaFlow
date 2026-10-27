// =============================================================================
// Plik: modules/flows-builder/canvas.js
// Opis: Canvas Flow Buildera - renderowanie nodów jako HTML (position absolute),
//       krawędzi jako SVG bezier w WORLD coords, pan/zoom (wheel = zoom),
//       pointer-events z rAF throttle. Historia undo/redo in-memory.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { getNodeDisplayTitle, isAutoNodeLabel } from '/js/modules/flows-builder/node-i18n.js';
import { nodeIconId, nodeColorVar } from '/js/modules/flows-builder/node-visuals.js';
import { ModelModalities } from '/js/modules/flows-builder/model-modalities.js';
import { processToCanvas, canvasToProcess, processBody, cloneProcessBody, cloneProcessActivityIo, processNodeKind, processNodeConfig, processBoundaryKind, processServiceKind, processNodeWarnings, processWarningText, processHandlerName, processBoundaryHosts, processBoundaryRefusals, processEscalationPathProblem, processHasTimer, processTimezoneCanonical, processTimezoneGaps, processTimezoneProblem, processTimezoneText } from './bpmn.js';
import { planProcessLabels } from './label-layout.js';
import '/js/components/tf-menu.js';

const NODE_WIDTH = 280;
const NODE_H_APPROX = 110;
// Próg czytelności widoku otwieranego automatycznie: przy 0.75 nazwa pola ma
// ~8 px, poniżej z bloku zostają same wartości.
const READABLE_MIN_ZOOM = 0.75;
const GRID = 10;
// Screen pixels kept between a revealed node and the canvas edge.
const REVEAL_MARGIN = GRID * 2;

function snapGridCoordinate(value) {
  const snapped = Math.round(value / GRID) * GRID;
  return Object.is(snapped, -0) ? 0 : snapped;
}

const MAX_HISTORY = 30;
const ESCALATION_PATH_KEYS = ['terminal_before_wait', 'scope_entry', 'call_entry', 'no_durable_wait'];
// Layout portów: header ma ~44px, porty zaczynają się od HEADER_OFFSET i są
// rozłożone co PORT_STEP pikseli.
const PORT_HEADER_OFFSET = 44;
const PORT_STEP = 22;
const DRAG_THRESHOLD = 4;
// Etykieta portu lezy WEWNATRZ noda, obok swojej kropki, wiec kolumna wartosci
// musi jej oddac pas — inaczej "stream" laduje na "16000". Pas placimy tylko po
// tej stronie, ktora faktycznie rysuje etykiety (>=2 porty).
const PORT_LABEL_LANE = 52;
// Margines miedzy skrajnym blokiem a krawedzia plotna — ten sam przy
// dopasowaniu widoku i przy ograniczeniu panoramy, wiec blok nigdy nie stoi
// przycięty przez paletę albo inspektora.
const CONTENT_MARGIN = 40;

// Kategoria -> krótka etykieta na nodzie
const TYPE_CATEGORY = {
  trigger: 'trigger', start: 'trigger',
  llm: 'ai', embeddings: 'ai', reranker: 'ai', stt: 'ai', tts: 'ai',
  memory: 'memory', conversation_history: 'memory', session_context: 'memory',
  speaker_context: 'memory', memory_analyzer: 'memory',
  condition: 'logic', switch: 'logic',
  template: 'transform', transform: 'transform', router: 'transform',
  pii_filter: 'filter', tts_clean: 'filter',
  output: 'output', end: 'output', activity_result: 'output',
  persist_turn: 'memory',
  spawn: 'agent', await_subagents: 'agent', subagent_status: 'agent', interval: 'agent',
  workspace_context: 'code', exec_command: 'code', delegate_cli: 'code',
  patch_review: 'logic',
};

// Generuje stabilny identyfikator regionu pętli (loop region). Region grupuje
// węzły o tym samym `node.region`; krawędź wsteczna tego regionu nosi
// `edge.kind="loop_back"`.
function genRegionId() {
  return 'loop_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 6);
}

// Zwraca listę portów wejściowych/wyjściowych dla nody. Priorytet: adapter
// metadata z backendu (`template.input_ports`/`template.output_ports` plus
// `input_port_types`/`output_port_types`), potem legacy pola
// `inputs`/`outputs`, a na koncu heurystyki.
// Adapter metadata jest autorytatywnym zrodlem — backend odrzuci krawedz z
// nazwa portu spoza listy, wiec UI musi pokazywac dokladnie te porty.
// Kazdy port ma `type` (string `text`/`audio`/`image`/`video`/`embedding`/
// `other`/`json`/`any`) — uzywany do kolorowania i walidacji polaczenia
// po stronie GUI (lustrzana R8: `any` na ktorejkolwiek stronie = wildcard).
/// Flags ports the node's CHOSEN MODEL cannot serve. The port keeps its type
/// and its identity — only `unsupported` is added, and the canvas dims it.
///
/// Only a model-driven node has anything to check: for everything else the
/// question is meaningless, so the flag is never set and nothing dims.
/// An unknown model (not in the catalog, e.g. typed by hand or newer than the
/// snapshot) leaves every port enabled — refusing to wire something we simply
/// have no record of would be worse than letting the run report it.
function markUnsupported(node, ports, side) {
  if (!node || (node.type !== 'llm' && node.type !== 'vision_llm')) return;
  const model = node.config?.model;
  if (!model || !ModelModalities.known(model)) return;
  const modalityOf = (p) => {
    const t = (p.type || '').toLowerCase();
    if (t === 'image' || t === 'audio') return t;
    // `stream`/`full`/`in` are the text channel whatever they are called.
    return 'text';
  };
  for (const p of ports) {
    const modality = modalityOf(p);
    const ok = side === 'in'
      ? ModelModalities.accepts(model, modality)
      : ModelModalities.emits(model, modality);
    if (ok === false) {
      p.unsupported = true;
      p.unsupportedReason = I18n.t('flows_builder.port_unsupported', { model, modality })
        || `${model}: brak obsługi ${modality}`;
    }
  }
}

function portsForNode(node, template) {
  if (node.type.startsWith('bpmn_')) {
    return { inputs: (['bpmn_start', 'bpmn_timer_start', 'bpmn_message_start', 'bpmn_link_catch'].includes(node.type) || processBoundaryKind(node.type)) ? [] : [{ name: 'in', type: 'any' }],
      outputs: ['bpmn_end', 'bpmn_error_end', 'bpmn_terminate_end', 'bpmn_link_throw'].includes(node.type) ? [] : [{ name: 'full', type: 'any' }] };
  }
  // `start` has no seeded `flow_node_templates` row (legacy in-memory type),
  // so it needs the literal fallback; every other entry-style node — trigger,
  // `bus_consume`, `on_subagent_complete`, and any future one — is tagged
  // `category: "trigger"` server-side (db/seed.rs) from the SAME signal the
  // adapter registry uses for its real, possibly-empty `input_ports()`. Keying
  // off that category instead of an ever-growing type-name list is what lets a
  // node with zero real inputs suppress the synthesized phantom `in` port below.
  const isTrigger = node.type === 'trigger' || node.type === 'start' || template?.category === 'trigger';
  const isOutput = node.type === 'output' || node.type === 'end' || node.type === 'activity_result';

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
  markUnsupported(node, inputs, 'in');

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

  markUnsupported(node, outputs, 'out');
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

// A code such as INVALID_CUSTOMER_ADDRESS has no break opportunity of its own,
// so the label would split it mid-word; the marker offers one after each
// separator and the mid-word split stays the last resort.
function breakableHtml(text) {
  return escapeHtml(text).replace(/([_./-])/g, '$1<wbr>');
}

// The tag drawn above a boundary event that is on no task (flows-builder.css
// .fb-unattached::after): it is a pseudo-element, so its box is estimated.
export const UNATTACHED_TAG = { width: 140, height: 20, offset: 10 };

export class FlowCanvas {
  constructor(rootEl, opts = {}) {
    this.root = rootEl;
    this.opts = opts;
    this.mode = opts.mode || 'flow';
    this.processPath = [];
    this.selectedProcessId = null;
    this.nodes = [];
    this.edges = [];
    this.selectedIds = new Set();
    this.selectedEdgeId = null;
    this.view = { x: 0, y: 0, zoom: 1 };
    this._pendingFitMinZoom = null;
    this.history = [];
    this.historyIndex = -1;
    this.onChange = opts.onChange || (() => {});
    this.onSelect = opts.onSelect || (() => {});
    // `restored` is true for undo/redo (same model lineage), false when a different model was loaded.
    this.onModelReplace = opts.onModelReplace || (() => {});
    this.onViewChange = opts.onViewChange || (() => {});
    // Read-only canvas (system flows): selection, pan and zoom stay, every
    // graph mutation is refused at the state level, not only hidden in CSS.
    this.readOnly = !!opts.readOnly;
    this.templates = new Map(); // node_type -> template
    // Typy dostępne w kilku presetach (np. `agent`): dla nich sama nazwa typu
    // nie identyfikuje bloku, więc node musi zapamiętać etykietę presetu.
    this.presetTypes = new Set();
    this._zTop = 1;
    const edgeMarkerPrefix = `fb-edge-${crypto.randomUUID().replaceAll('-', '')}`;
    this._edgeMarkerId = `${edgeMarkerPrefix}-normal`;

    this._rafPending = false;
    this._draggingNode = null;     // { ids, startClientX, startClientY, origs, moved, pointerId }
    this._connecting = null;        // { fromNode, fromPort, currentX, currentY, pointerId }
    this._panning = null;           // { startClientX, startClientY, origView, pointerId }
    this._pinch = null;             // { startDist, startZoom, cx, cy }
    this._pointerMovePending = null;
    this._suppressNextClick = false;

    this._buildDom();
    this._bindEvents();
    this._resizeObserver = new ResizeObserver(() => {
      if (this._pendingFitMinZoom !== null) this.fitToContent(this._pendingFitMinZoom);
      // Labels are placed from measured sizes; a canvas that had no layout (a
      // hidden tab) is placed once it has one.
      if (this._labelsUnplaced) this._layoutProcessLabels();
    });
    this._resizeObserver.observe(this.root);
  }

  setTemplates(list) {
    this.templates.clear();
    this.presetTypes.clear();
    for (const t of list || []) {
      // A drop-in Service task shares its node_type with the plain element and
      // must not replace how that element is drawn.
      if (t.preset && this.templates.has(t.node_type)) continue;
      if (this.templates.has(t.node_type)) this.presetTypes.add(t.node_type);
      this.templates.set(t.node_type, t);
    }
    this._normalizeNodeLabels();
    this._normalizeEdgePorts();
  }

  _buildDom() {
    this.root.classList.add('fb-canvas');
    this.root.innerHTML = `
      <div class="fb-canvas-world">
        <div class="fb-regions-layer"></div>
        <svg class="fb-edges" xmlns="http://www.w3.org/2000/svg"></svg>
        <div class="fb-nodes-layer"></div>
      </div>
      <div class="fb-canvas-hint" data-role="hint">
        <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M12 5v14M5 12h14"/></svg>
        <div>${escapeHtml(I18n.t('flows_builder.canvas_hint'))}</div>
      </div>
    `;
    this.world = this.root.querySelector('.fb-canvas-world');
    this.regionsLayer = this.root.querySelector('.fb-regions-layer');
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
      cm: this._onContextMenu.bind(this),
    };
    this.root.addEventListener('pointerdown', this._h.pd);
    window.addEventListener('pointermove', this._h.pm);
    window.addEventListener('pointerup', this._h.pu);
    window.addEventListener('pointercancel', this._h.pu);
    this.root.addEventListener('wheel', this._h.wh, { passive: false });
    this.root.addEventListener('click', this._h.ck);
    this.root.addEventListener('contextmenu', this._h.cm);
    this._activePointers = new Map();
  }

  destroy() {
    this._resizeObserver.disconnect();
    if (this._h) {
      this.root.removeEventListener('pointerdown', this._h.pd);
      window.removeEventListener('pointermove', this._h.pm);
      window.removeEventListener('pointerup', this._h.pu);
      window.removeEventListener('pointercancel', this._h.pu);
      this.root.removeEventListener('wheel', this._h.wh);
      this.root.removeEventListener('click', this._h.ck);
      this.root.removeEventListener('contextmenu', this._h.cm);
    }
    this._closeContextMenu();
    this.root.innerHTML = '';
  }

  // Menu kontekstowe canvas: akcje regionu petli (zgrupuj / rozgrupuj) na
  // biezacym zaznaczeniu. Otwierane prawym przyciskiem nad wezlem.
  _onContextMenu(ev) {
    if (this.mode === 'bpmn') return;
    const nodeEl = ev.target.closest('.fb-node');
    if (!nodeEl || this.readOnly) { this._closeContextMenu(); return; }
    ev.preventDefault();
    const id = nodeEl.dataset.nodeId;
    if (!this.selectedIds.has(id)) this.selectNode(id, { additive: ev.shiftKey });

    this._closeContextMenu();
    const ids = [...this.selectedIds];
    const hasRegion = this.selectionHasRegion();
    const anchor = document.createElement('div');
    anchor.className = 'fb-context-anchor';
    anchor.style.position = 'fixed';
    anchor.style.left = `${ev.clientX}px`;
    anchor.style.top = `${ev.clientY}px`;
    anchor.style.zIndex = '9999';
    const menu = document.createElement('tf-menu');
    menu.setAttribute('placement', 'bottom-start');
    menu.innerHTML = `
      <tf-menu-item action="group" icon="rotate">${escapeHtml(I18n.t('flows_builder.group_loop_region'))}</tf-menu-item>
      ${hasRegion ? `<tf-menu-item action="ungroup" icon="trash">${escapeHtml(I18n.t('flows_builder.ungroup_loop_region'))}</tf-menu-item>` : ''}
    `;
    anchor.appendChild(menu);
    document.body.appendChild(anchor);
    this._contextAnchor = anchor;
    menu.addEventListener('action', (e) => {
      const act = e.detail?.action;
      if (act === 'group') this.groupIntoRegion(ids);
      else if (act === 'ungroup') this.ungroupRegion(ids);
      this._closeContextMenu();
    });
    menu.addEventListener('close', () => this._closeContextMenu());
    menu.open();
  }

  _closeContextMenu() {
    if (this._contextAnchor) {
      if (this._contextAnchor.isConnected) this._contextAnchor.remove();
      this._contextAnchor = null;
    }
  }

  // -------------------------------------------------------------------------
  // Dane
  // -------------------------------------------------------------------------
  setData(nodes, edges, { reset = true } = {}) {
    if (this.mode === 'bpmn') {
      this.processPath = [];
      this.selectedProcessId = nodes.processId;
      this.processModel = structuredClone(nodes);
      this.onModelReplace(false);
      const data = processToCanvas(nodes, this.processPath, this.selectedProcessId);
      nodes = data.nodes;
      edges = data.edges;
    }
    // Seed/backend pisze pozycje jako `{position: {x, y}}` (zagniezdzone),
    // canvas renderuje przez flat `n.x`/`n.y`. Bez tej normalizacji wszystkie
    // nodes lecialy na (0,0) — w GUI wygladalo to jak pojedynczy node.
    this.nodes = (nodes || []).map((n) => {
      const pos = n.position || {};
      const x = typeof n.x === 'number' ? n.x : (typeof pos.x === 'number' ? pos.x : 0);
      const y = typeof n.y === 'number' ? n.y : (typeof pos.y === 'number' ? pos.y : 0);
      // `region` (harness loop region id) must round-trip: keep it as a string
      // or drop it entirely so an empty string never poisons the box grouping.
      const region = typeof n.region === 'string' && n.region ? n.region : undefined;
      return { ...n, x, y, region, config: n.config || {} };
    });
    // Krawedzie z backendu (flow_json w DB) nie maja `id` — to pole jest
    // wewnetrznym uchwytem GUI dla selekcji + delete. Generujemy stable
    // `e_<idx>_<from>_<to>_<from_port>_<to_port>` zeby ten sam graf po
    // round-tripie (load → save → load) konsystentnie zaznaczal te same
    // krawedzie. Dla nowych krawedzi `_onPointerUp` nadal generuje
    // unikalne `e_<timestamp>` przy tworzeniu.
    this.edges = (edges || []).map((raw, idx) => {
      // The engine also accepts from/to and source/target; the editor and the
      // serializer know only from_node/to_node, so any other spelling must be
      // folded in here or the connection is counted but never drawn.
      const { from, to, source, target, ...rest } = raw;
      const e = { ...rest, from_node: raw.from_node ?? from ?? source, to_node: raw.to_node ?? to ?? target };
      const id = e.id || `e${idx}_${e.from_node || ''}-${e.to_node || ''}_${e.from_port || ''}-${e.to_port || ''}`;
      return { ...e, id };
    });
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
    // Po załadowaniu flow dopasuj widok do nodów (wyśrodkuj + zoom-to-fit),
    // żeby nie startować z pustego rogu canvasa i nie szukać nodów panem.
    // reset=false to undo/redo — wtedy zachowujemy bieżący widok użytkownika.
    if (reset) this.fitToContent(READABLE_MIN_ZOOM);
  }

  getData() {
    if (this.mode === 'bpmn') {
      this._keepGeometryFinite();
      this.processModel = canvasToProcess(this.processModel, this.nodes, this.edges,
        (edge) => this._processEdgePoints(edge), this.processPath, this.selectedProcessId);
      const data = structuredClone(this.processModel);
      // A zone only means something to a process with a timer or a calendar, and
      // the server refuses one on any other, so it is left out of what is sent.
      // The editor keeps it: deleting the last timer and adding another must not
      // make the field forget it at the next autosave.
      for (const body of [data, ...(data.additionalProcesses || [])]) {
        if (body.timerTimezone != null && !body.workCalendar && !processHasTimer({ ...body, additionalProcesses: [] })) delete body.timerTimezone;
      }
      return data;
    }
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
        const out = {
          ...rest,
          position: { x: Number(x) || 0, y: Number(y) || 0 },
          config: { ...n.config },
        };
        // `region` is the loop-region marker (Part 5). Drop it when empty so
        // ungrouped nodes round-trip byte-identically (backend serde(default)).
        if (!out.region) delete out.region;
        return out;
      }),
      edges: this.edges.map((e) => {
        const out = { ...e };
        // `id` jest wewnetrznym uchwytem GUI generowanym przy load'zie z
        // tupli (idx, from, to, ports). Backend FlowEdge ma `id: Option<String>`
        // ale nasze syntetyczne klucze nie maja sensu w DB — czyscimy zeby
        // round-trip do flow_json byl byte-identyczny ze zrodlem.
        if (typeof out.id === 'string' && out.id.startsWith('e')) {
          delete out.id;
        }
        if (out.from_port === 'full') delete out.from_port;
        if (out.to_port === 'in') delete out.to_port;
        // `kind` carries "loop_back" for the region back-edge (Part 5). Drop it
        // when absent/empty so plain edges keep their canonical shape.
        if (!out.kind) delete out.kind;
        return out;
      }),
    };
  }

  // A coordinate that is not a finite number cannot be stored as a number (the
  // server refuses `null` for an f64), so it falls back to the origin or the
  // default size before it reaches the model.
  _keepGeometryFinite() {
    for (const node of this.nodes) {
      if (!Number.isFinite(node.x)) node.x = 0;
      if (!Number.isFinite(node.y)) node.y = 0;
      if (node.width !== undefined && !(Number.isFinite(node.width) && node.width > 0)) node.width = 240;
      if (node.height !== undefined && !(Number.isFinite(node.height) && node.height > 0)) node.height = 96;
    }
  }

  navigateProcessBody(path, processId = this.selectedProcessId) {
    if (this.mode !== 'bpmn') return;
    const complete = this.getData();
    processBody(complete, path, processId);
    this.selectedProcessId = processId;
    this.processPath = [...path];
    const graph = processToCanvas(complete, path, processId);
    this.nodes = graph.nodes;
    this.edges = graph.edges;
    this._normalizeNodeLabels();
    this._normalizeEdgePorts();
    this.selectedIds.clear();
    this.selectedEdgeId = null;
    this.onSelect(null);
    this.render();
    this.fitToContent(READABLE_MIN_ZOOM);
  }

  addProcessBody() {
    if (this.readOnly || this.mode !== 'bpmn') return;
    const model = this.getData();
    if ((model.additionalProcesses?.length || 0) >= 15) return;
    const suffix = crypto.randomUUID().replaceAll('-', '_');
    const processId = `Process_${suffix}`;
    const start = `Start_${suffix}`, end = `End_${suffix}`;
    model.additionalProcesses ||= [];
    model.additionalProcesses.push({
      processId, nodes: [{ id: start, name: '', kind: 'Start' },
        { id: end, name: '', kind: 'End' }],
      sequenceFlows: [{ id: `Flow_${suffix}`, sourceId: start,
        targetId: end, condition: null }],
      variables: {}, diagram: { shapes: [
        { elementId: start, x: 80, y: 160, width: 56, height: 56 },
        { elementId: end, x: 400, y: 160, width: 56, height: 56 },
      ], edges: [{ sequenceFlowId: `Flow_${suffix}`,
        waypoints: [{ x: 136, y: 188 }, { x: 400, y: 188 }] }] },
    });
    this.processModel = model;
    this.navigateProcessBody([], processId);
    this._pushHistory();
    this.onChange();
  }

  updateSelectedProcessName(name) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    this.getData();
    const body = processBody(this.processModel, [], this.selectedProcessId);
    if (body.processName === name) return;
    body.processName = name;
    this._pushHistory();
    this.onChange();
  }

  // What an author calls the element in a message: its name, else its kind.
  _nodeName(node) {
    return getNodeDisplayTitle(node, this.templates.get(node.type));
  }

  // Waliduje klient-side przed zapisem: kazdy edge musi wskazywac istniejace
  // node'y i porty obecne w adapter metadata. Zwraca liste bledow jako
  // stringi (juz zlokalizowane) — pusta lista oznacza flow gotowy do zapisu.
  validate() {
    if (this.mode === 'bpmn') {
      const errors = [];
      const selectedBody = processBody(this.processModel, [], this.selectedProcessId);
      const model = this.getData();
      for (const gap of processTimezoneGaps(model)) errors.push(processTimezoneText(gap));
      for (const body of [model, ...(model.additionalProcesses || [])]) {
        if (processTimezoneProblem(body.timerTimezone)) errors.push(I18n.t('bpmn.timer_timezone_invalid', { zone: body.timerTimezone }));
      }
      for (const node of this.nodes) {
        const condition = node.type === 'bpmn_service_task' && node.config.verification?.Condition;
        if (condition && !(condition.expression || '').trim()) {
          errors.push(I18n.t('bpmn.verification_expression_required', { node: this._nodeName(node) }));
        }
        const called = node.type === 'bpmn_call_activity' ? node.config : null;
        if (called && !(called.localBody ? called.localBody.processId
          : called.calledDefinitionId && Number.isInteger(called.calledVersion) && called.calledVersion >= 1
            && called.calledElement?.processId)) {
          errors.push(I18n.t('bpmn.call_target_required', { node: this._nodeName(node) }));
        }
        if (['bpmn_boundary_error', 'bpmn_boundary_escalation'].includes(node.type)
          && !this.nodes.some((candidate) => candidate.id === node.config.attachedToId)) {
          errors.push(I18n.t('bpmn.boundary_required_named', { node: this._nodeName(node) }));
        }
        if (processBoundaryKind(node.type) && node.config.attachedToId) {
          const host = this.nodes.find((candidate) => candidate.id === node.config.attachedToId);
          const refusal = host?.repeat ? processBoundaryRefusals(this, processNodeKind(node.type)).get(host.id) : null;
          if (refusal === 'error_type' || refusal === 'escalation_type') {
            errors.push(I18n.t(`bpmn.boundary_attached_refused_${refusal}`, { node: this._nodeName(node), host: this._nodeName(host) }));
          }
        }
        const problem = node.type === 'bpmn_boundary_escalation' ? processEscalationPathProblem(this, node.id) : null;
        if (problem) errors.push(I18n.t(`bpmn.routing_warning_escalation_path_${ESCALATION_PATH_KEYS.includes(problem.reason) ? problem.reason : 'other'}`, { names: this._nodeName(node) }));
        if (!['bpmn_timer_start', 'bpmn_timer_catch', 'bpmn_boundary_timer'].includes(node.type)) continue;
        const [kind, timer] = Object.entries(node.config.timer)[0];
        if (kind === 'Date' && (!timer.at || !timer.at.trim())) {
          errors.push(I18n.t('bpmn.timer_date_required', { node: this._nodeName(node) }));
        }
        if (processBoundaryKind(node.type)) {
          const parent = this.nodes.find((candidate) => candidate.id === node.config.attachedToId);
          if (!parent || !(['bpmn_user_task', 'bpmn_service_task', 'bpmn_sub_process', 'bpmn_call_activity'].includes(parent.type)
            || ['bpmn_script_task', 'bpmn_send_task', 'bpmn_manual_task', 'bpmn_receive_task'].includes(parent.type))) {
            errors.push(I18n.t('bpmn.boundary_required_named', { node: this._nodeName(node) }));
          }
          if (!['Date', 'Duration', 'WorkingDuration'].includes(kind) || this.edges.some((edge) => edge.to_node === node.id)) {
            errors.push(I18n.t('bpmn.boundary_timer_invalid', { node: this._nodeName(node) }));
          }
        }
        const fields = kind === 'Daily'
          ? [['hour', 'timer_hour', 0, 23], ['minute', 'timer_minute', 0, 59]]
          : ['Duration', 'Cycle', 'WorkingDuration'].includes(kind) ? [['seconds', kind === 'WorkingDuration' ? 'timer_working_seconds' : 'timer_seconds', kind === 'Cycle' ? 300 : 1, 31536000]] : [];
        if (kind === 'WorkingDuration' && !selectedBody.workCalendar) {
          errors.push(I18n.t('bpmn.calendar_required'));
        }
        if (['Cycle', 'Daily'].includes(kind) && timer.totalFirings != null) fields.push(['totalFirings', 'timer_total', 1, 4294967295]);
        for (const [field, label, min, max] of fields) {
          if (!Number.isInteger(timer[field]) || timer[field] < min || timer[field] > max) {
            errors.push(I18n.t('bpmn.timer_number_invalid', { node: this._nodeName(node), field: I18n.t(`bpmn.${label}`), min, max }));
          }
        }
      }
      for (const node of this.nodes.filter((candidate) => candidate.type === 'bpmn_boundary_message')) {
        const parent = this.nodes.find((candidate) => candidate.id === node.config.attachedToId);
        if (!parent || !(['bpmn_user_task', 'bpmn_service_task', 'bpmn_sub_process', 'bpmn_call_activity'].includes(parent.type)
          || ['bpmn_script_task', 'bpmn_send_task', 'bpmn_manual_task', 'bpmn_receive_task'].includes(parent.type))) {
          errors.push(I18n.t('bpmn.boundary_required_named', { node: this._nodeName(node) }));
        }
      }
      for (const gateway of this.nodes.filter((candidate) => candidate.type === 'bpmn_event_based_gateway')) {
        const branches = this.edges.filter((edge) => edge.from_node === gateway.id)
          .map((edge) => this.nodes.find((node) => node.id === edge.to_node)).filter(Boolean);
        if (branches.some((node) => node.type === 'bpmn_receive_task')
          && branches.some((node) => node.type === 'bpmn_message_catch')) {
          errors.push(I18n.t('bpmn.event_gateway_receive_message_mix', { node: this._nodeName(gateway) }));
        }
        for (const branch of branches) {
          if (this.edges.filter((edge) => edge.to_node === branch.id).length !== 1
            || this.edges.filter((edge) => edge.from_node === branch.id).length !== 1) {
            errors.push(I18n.t('bpmn.event_gateway_branch_incoming', { node: this._nodeName(branch) }));
          }
          if (branch.type === 'bpmn_receive_task'
            && this.nodes.some((node) => processBoundaryKind(node.type) && node.config.attachedToId === branch.id)) {
            errors.push(I18n.t('bpmn.event_gateway_receive_boundary', { node: this._nodeName(branch) }));
          }
        }
      }
      return errors;
    }
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
    // Integralnosc regionu petli (lustro backendowej R11): krawedz `loop_back`
    // musi laczyc wylacznie wezly tego samego `region`. To jest jedyny
    // dozwolony cykl — kazda inna krawedz wsteczna jest bledem.
    for (const edge of this.edges) {
      if (edge.kind !== 'loop_back') continue;
      const from = nodeById.get(edge.from_node);
      const to = nodeById.get(edge.to_node);
      if (!from || !to) continue; // juz zlapane wyzej jako dangling
      if (!from.region || from.region !== to.region) {
        errors.push(I18n.t('flows_builder.loop_back_cross_region'));
      }
    }

    // Wykrywanie cykli — DFS z kolorowaniem; zwracamy pojedynczy blad jesli
    // jakikolwiek cykl istnieje, bo lista cykli nie wnosi wiecej dla usera.
    // Krawedzie `loop_back` SA pomijane w grafie sasiedztwa: region petli ma
    // jedna jawna krawedz wsteczna, ktora backend (cache.rs Kahn) tez wylacza
    // z in-degree, wiec zewnetrzny DAG pozostaje acykliczny. Bez tego wyjatku
    // walidacja klienta odrzucalaby kazdy poprawny region.
    const adj = new Map();
    for (const e of this.edges) {
      if (e.kind === 'loop_back') continue;
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
    if (this.mode === 'bpmn') return;
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
          if (inputNames.length === 1) {
            edge.to_port = inputNames[0];
          } else if (inputNames.includes('in')) {
            edge.to_port = 'in';
          } else if (from) {
            // Multi-port consumer (np. typed output node) bez `in` — auto-map
            // po typie producenta. Bierzemy from_port type i znajdujemy
            // input port konsumenta z tym samym typem. Bez tego legacy edge
            // z `to_port="in"` (default) walil walidacje przy save.
            const fromTpl = this.templates.get(from.type);
            const { outputs: fromOutputs } = portsForNode(from, fromTpl);
            const fromPortObj = fromOutputs.find((p) => p.name === edge.from_port);
            const fromType = (fromPortObj?.type || 'any').toLowerCase();
            const matched = inputs.find((p) => (p.type || 'any').toLowerCase() === fromType);
            if (matched) {
              edge.to_port = matched.name;
            } else {
              // Fallback: pierwszy input port. Walidacja serwera moze
              // jeszcze odrzucic na typed mismatch, ale przynajmniej damy
              // prawidlowa nazwe portu zeby user wiedzial gdzie kierowac.
              edge.to_port = inputNames[0];
            }
          } else {
            edge.to_port = inputNames[0];
          }
        } else {
          edge.to_port = currentTo;
        }
      }
    }
  }

  _clearRemovedDefaults() {
    if (this.mode !== 'bpmn') return;
    const ids = new Set(this.edges.map((edge) => edge.id));
    for (const node of this.nodes) {
      if (['bpmn_exclusive_gateway', 'bpmn_inclusive_gateway'].includes(node.type) && !ids.has(node.config.defaultFlowId)) node.config.defaultFlowId = null;
    }
  }

  updateEdge(edgeId, patch) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    const edge = this.edges.find((value) => value.id === edgeId);
    if (!edge || Object.entries(patch).every(([key, value]) => edge[key] === value)) return;
    Object.assign(edge, patch);
    this._pushHistory();
    this._renderEdges();
    this.onChange();
  }

  _processEdgePoints(edge) {
    const from = this.nodes.find((node) => node.id === edge.from_node);
    const to = this.nodes.find((node) => node.id === edge.to_node);
    const endpoints = [from?.x, from?.y, to?.x, to?.y];
    if (edge.waypoints?.length >= 2 && edge.originalEndpoints?.every((value, index) => value === endpoints[index])) return structuredClone(edge.waypoints);
    return [this._getPortWorldPos(edge.from_node, 'full', 'out'), this._getPortWorldPos(edge.to_node, 'in', 'in')];
  }

  _pushHistory() {
    // Odcinamy redo-branche po nowej operacji. structuredClone zamiast
    // JSON round-trip — szybszy binary copy + zachowuje typy Date/Map jesli
    // kiedys trafia do node.config (JSON.stringify gubi wszystko nieprymitywne).
    // A process snapshot is the whole model: the scope being viewed is not part
    // of the step, so an undo never moves the author between bodies.
    this.history = this.history.slice(0, this.historyIndex + 1);
    this.history.push(structuredClone(this.mode === 'bpmn'
      ? { processModel: this.getData() } : { nodes: this.nodes, edges: this.edges }));
    if (this.history.length > MAX_HISTORY) this.history.shift();
    this.historyIndex = this.history.length - 1;
  }

  undo() {
    if (this.readOnly || this.historyIndex <= 0) return;
    this._restoreHistory(this.historyIndex - 1);
  }

  redo() {
    if (this.readOnly || this.historyIndex >= this.history.length - 1) return;
    this._restoreHistory(this.historyIndex + 1);
  }

  // A process step restores the model and redraws the scope that is on screen;
  // only when that scope no longer exists (the step removed the sub-process or
  // body being viewed) does the view climb to the nearest one that does. A step
  // that left the shown scope unchanged says where it happened instead of
  // looking like a dead button.
  _restoreHistory(index) {
    const forward = index > this.historyIndex;
    this.historyIndex = index;
    const snap = structuredClone(this.history[index]);
    let hidden = false;
    let settingsChanged = false;
    if (this.mode === 'bpmn') {
      const before = this.getData();
      const model = snap.processModel;
      const known = model.processId === this.selectedProcessId
        || (model.additionalProcesses || []).some((body) => body.processId === this.selectedProcessId);
      const processId = known ? this.selectedProcessId : model.processId;
      let path = known ? [...this.processPath] : [];
      for (; path.length > 0; path = path.slice(0, -1)) {
        try { processBody(model, path, processId); break; } catch (_) { /* sub-process is gone */ }
      }
      const graph = processToCanvas(model, path, processId);
      this.processModel = model;
      this.processPath = path;
      this.selectedProcessId = processId;
      this.nodes = graph.nodes;
      this.edges = graph.edges;
      try { hidden = JSON.stringify(processToCanvas(before, path, processId)) === JSON.stringify(graph); } catch (_) { /* the scope is new */ }
      // What changed instead of the drawing: the settings of the process (zone,
      // calendar, variables, declarations), which have no place on the canvas.
      const withoutDrawing = (full) => JSON.stringify(full, (key, value) => (['nodes', 'sequenceFlows', 'diagram'].includes(key) ? undefined : value));
      settingsChanged = hidden && withoutDrawing(before) !== withoutDrawing(model);
    } else {
      this.nodes = snap.nodes;
      this.edges = snap.edges;
    }
    if (this.mode === 'bpmn') this.onModelReplace(true);
    this.selectedIds.clear();
    this.selectedEdgeId = null;
    this.onSelect(null);
    this.render();
    this.onChange();
    if (hidden) toast(I18n.t(settingsChanged ? (forward ? 'bpmn.redo_settings' : 'bpmn.undo_settings') : 'bpmn.undo_elsewhere'), 'info');
  }

  // -------------------------------------------------------------------------
  // CRUD nody / krawędzie
  // -------------------------------------------------------------------------
  updateProcessTimezone(value) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    const body = processBody(this.processModel, [], this.selectedProcessId);
    const zone = String(value ?? '').trim();
    const canonical = processTimezoneCanonical(zone);
    if (canonical === null) return;
    if (canonical === '') delete body.timerTimezone;
    else body.timerTimezone = canonical;
    this._pushHistory();
    this.onChange();
  }

  updateProcessVariables(values) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    this.getData();
    processBody(this.processModel, this.processPath, this.selectedProcessId).variables = structuredClone(values);
    this._pushHistory();
    this.onChange();
  }

  updateProcessCalendar(calendar) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    const body = processBody(this.processModel, [], this.selectedProcessId);
    if (calendar === null) {
      delete body.workCalendar;
      delete body.calendarPin;
    } else {
      body.workCalendar = structuredClone(calendar);
    }
    this._pushHistory();
    this.onChange();
  }

  updateProcessDeclarations({ messages, errors, escalations, signals = [], targetNamespace }) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    this.processModel.messages = structuredClone(messages);
    this.processModel.errors = structuredClone(errors);
    this.processModel.escalations = structuredClone(escalations);
    if (signals.length) this.processModel.signals = structuredClone(signals);
    else delete this.processModel.signals;
    if (targetNamespace) this.processModel.targetNamespace = targetNamespace;
    else delete this.processModel.targetNamespace;
    this._pushHistory();
    this.onChange();
  }

  updateProcessModeling({ modeling, modelingShapes, modelingEdges, collaboration, dataStores }) {
    if (this.readOnly || this.mode !== 'bpmn') return;
    this.getData();
    const body = processBody(this.processModel, this.processPath, this.selectedProcessId);
    if (modeling) body.modeling = structuredClone(modeling);
    else delete body.modeling;
    if (modelingShapes.length) body.diagram.modelingShapes = structuredClone(modelingShapes);
    else delete body.diagram.modelingShapes;
    if (modelingEdges.length) body.diagram.modelingEdges = structuredClone(modelingEdges);
    else delete body.diagram.modelingEdges;
    if (collaboration) this.processModel.collaboration = structuredClone(collaboration);
    else delete this.processModel.collaboration;
    if (dataStores.length) this.processModel.dataStores = structuredClone(dataStores);
    else delete this.processModel.dataStores;
    this._pushHistory();
    this.render();
    this.onChange();
  }

  adoptPublishedProcessModel(model) {
    if (this.mode !== 'bpmn') return;
    this.processModel = structuredClone(model);
    for (const snapshot of this.history) {
      for (const body of [snapshot.processModel, ...(snapshot.processModel.additionalProcesses || [])]) {
        const published = [model, ...(model.additionalProcesses || [])]
          .find((candidate) => candidate.processId === body.processId);
        if (published && body.workCalendar) body.calendarPin = structuredClone(published.calendarPin ?? null);
      }
    }
    if (this.historyIndex >= 0) {
      this.history[this.historyIndex].processModel = structuredClone(model);
    }
  }

  /**
   * `cascade` is for adds that name no position (palette click or Enter):
   * every one would otherwise land on the same viewport centre and stack.
   */
  addNodeFromTemplate(tpl, clientX, clientY, { cascade = false } = {}) {
    if (this.readOnly) return;
    if (this.mode === 'bpmn' && this.processPath.length > 0
      && ['bpmn_timer_start', 'bpmn_message_start'].includes(tpl.node_type)) return;
    if (this.mode === 'bpmn' && this.processPath.length >= 3 && tpl.node_type === 'bpmn_sub_process') return;
    const rect = this.root.getBoundingClientRect();
    const pt = this._clientToWorld(Number.isFinite(clientX) ? clientX : rect.left + rect.width / 2,
      Number.isFinite(clientY) ? clientY : rect.top + rect.height / 2);
    if (!Number.isFinite(pt.x) || !Number.isFinite(pt.y)) { pt.x = 0; pt.y = 0; }
    let defaultConfig = {};
    try { defaultConfig = JSON.parse(tpl.default_config || '{}'); } catch (_) {}
    const node = {
      id: 'n_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 6),
      type: tpl.node_type,
      label: this.presetTypes.has(tpl.node_type) ? String(tpl.label || '') : '',
      x: snapGridCoordinate(pt.x - NODE_WIDTH / 2),
      y: snapGridCoordinate(pt.y - NODE_H_APPROX / 2),
      config: defaultConfig,
    };
    if (this.mode === 'bpmn') {
      node.config = { ...processNodeConfig(processNodeKind(node.type)), ...structuredClone(tpl.preset ?? {}) };
      if (tpl.preset) node.label = String(tpl.label || '');
      node.width = tpl.width;
      node.height = tpl.height;
      node.x = snapGridCoordinate(pt.x - node.width / 2);
      node.y = snapGridCoordinate(pt.y - node.height / 2);
      if (!cascade && processBoundaryKind(node.type)) this._attachDroppedBoundary(node, pt);
    }
    const classic = this.mode !== 'bpmn';
    // A drop that would land on another node, or a boundary event the task had no
    // room for, takes the nearest free spot instead of hiding what is there.
    if (!cascade && this.mode === 'bpmn' && !node.config.attachedToId && this._coversNode(node)) {
      this._placeCascade(node, classic);
    } else if (!cascade) {
      if (!node.config?.attachedToId) this._clearOfObstacles(node);
      this.nodes.push(node);
      this.render();
      // A classic node's height is only known once it is rendered, so a drop is
      // centred on the cursor with the measured size.
      const real = classic ? this._renderedHeight(node) : 0;
      if (real > 0) {
        node.y = snapGridCoordinate(pt.y - real / 2);
        if (!node.config?.attachedToId) this._clearOfObstacles(node, real);
        this.render();
      }
      // A drop at the edge of the canvas would leave the node half outside it.
      this._panToReveal(node, this._footprint(node, classic));
    } else {
      this._placeCascade(node, classic);
    }
    this._pushHistory();
    this.selectNode(node.id);
    this.onChange();
    return node;
  }

  // What floats over the drawing area (the minimap, on a phone the open
  // inspector), as rectangles in world units. A node placed under one of them
  // could not be seen or reached.
  _obstacleWorldRects() {
    const boxes = this.opts.obstacles?.() ?? [];
    if (!boxes.length) return [];
    const rect = this.root.getBoundingClientRect();
    const zoom = this._positiveZoom();
    return boxes.map((box) => ({ x: (box.left - rect.left - this.view.x) / zoom, y: (box.top - rect.top - this.view.y) / zoom,
      w: box.width / zoom, h: box.height / zoom }));
  }

  // A drop that lands under an obstacle moves to the nearer free side of it.
  _clearOfObstacles(node, measuredHeight) {
    const width = node.width || NODE_WIDTH;
    const height = node.height || measuredHeight || NODE_H_APPROX;
    for (const box of this._obstacleWorldRects()) {
      if (!(node.x < box.x + box.w && box.x < node.x + width && node.y < box.y + box.h && box.y < node.y + height)) continue;
      const left = Math.floor((box.x - width - GRID) / GRID) * GRID;
      const above = Math.floor((box.y - height - GRID) / GRID) * GRID;
      if (node.x - left <= node.y - above) node.x = left; else node.y = above;
    }
  }

  _coversNode(node) {
    return this._nodeRects().some((other) => node.x < other.x + other.w && other.x < node.x + node.width
      && node.y < other.y + other.h && other.y < node.y + node.height);
  }

  // A cascade add searches from where the add asked to land. The node is
  // placed with its estimated size, rendered, then measured once: a classic
  // node is taller than the estimate, a BPMN task may carry an external label
  // outside its box. Only a footprint that outgrew the estimate is searched
  // again, from the original target against the same neighbour rectangles.
  _placeCascade(node, classic) {
    const origin = { x: node.x, y: node.y };
    const rects = [...this._nodeRects(), ...this._obstacleWorldRects()];
    let placed = this._moveToFreeSpot(node, this._firstPassSize(node, rects), rects, origin);
    this.nodes.push(node);
    this.render();
    const real = this._footprint(node, classic);
    if (real.h > placed.h || real.w > placed.w || real.ox > placed.ox || real.oy > placed.oy) {
      placed = this._moveToFreeSpot(node, real, rects, origin);
      this.render();
    }
    this._panToReveal(node, this._footprint(node, classic));
  }

  // Rendered height in world units: offsetHeight ignores CSS transforms, so
  // it is already unscaled by the zoom.
  _renderedHeight(node) {
    const element = this.nodesLayer?.querySelector(`[data-node-id="${CSS.escape(node.id)}"]`);
    return element ? element.offsetHeight || 0 : 0;
  }

  // Where a label sits outside its element, relative to the element. A deferred
  // label has no placement (it only appears on hover and selection) and an
  // unplaced one has no offsets, so neither may take part in any geometry.
  _placedLabelBox(node) {
    const element = this.nodesLayer?.querySelector(`[data-node-id="${CSS.escape(node.id)}"]`);
    if (!element || element.classList.contains('fb-label-deferred')) return null;
    const label = element.querySelector('.fb-process-label');
    if (!label || label.hidden || !(label.offsetWidth > 0) || !(label.offsetHeight > 0)) return null;
    const left = Number.parseFloat(label.style.left);
    const top = Number.parseFloat(label.style.top);
    if (!Number.isFinite(left) || !Number.isFinite(top)) return null;
    return { left, top, width: label.offsetWidth, height: label.offsetHeight };
  }

  // The rectangle a node occupies, as the box plus the offsets (ox, oy) from
  // the footprint corner to the node corner: a classic node by its rendered
  // height, a BPMN node together with its external label, which hangs outside
  // the box and is positioned by _layoutProcessLabels relative to the node.
  _footprint(node, classic) {
    if (classic) {
      const rendered = this._renderedHeight(node);
      return { w: NODE_WIDTH, h: rendered || NODE_H_APPROX, ox: 0, oy: 0 };
    }
    const width = node.width || 240;
    const height = node.height || 96;
    const label = this._placedLabelBox(node);
    let left = 0; let top = 0; let right = width; let bottom = height;
    if (label) {
      left = Math.min(0, label.left); top = Math.min(0, label.top);
      right = Math.max(width, label.left + label.width);
      bottom = Math.max(height, label.top + label.height);
    }
    const ox = Math.ceil(-left / GRID) * GRID;
    const oy = Math.ceil(-top / GRID) * GRID;
    return { w: right + ox, h: bottom + oy, ox, oy };
  }

  // Rectangles of every node, measured in one pass; a classic node that is not
  // rendered yet gets the tallest measured height.
  _nodeRects() {
    if (this.mode === 'bpmn') {
      return this.nodes.map((n) => ({ x: n.x, y: n.y, w: n.width || 240, h: n.height || 96 }));
    }
    const heights = this.nodes.map((n) => this._renderedHeight(n));
    const typical = heights.reduce((max, h) => Math.max(max, h), NODE_H_APPROX);
    return this.nodes.map((n, i) => ({ x: n.x, y: n.y, w: NODE_WIDTH, h: heights[i] || typical }));
  }

  _firstPassSize(node, rects) {
    if (this.mode === 'bpmn') return { w: node.width || 240, h: node.height || 96, ox: 0, oy: 0 };
    return { w: NODE_WIDTH, h: rects.reduce((max, r) => Math.max(max, r.h), NODE_H_APPROX), ox: 0, oy: 0 };
  }

  // Scans a bounded grid of candidate corners, nearest to the requested spot
  // first, for one whose footprint (plus a gap) intersects no rectangle.
  // Corners inside the visible canvas win; with no free one there, the node
  // goes right of the rightmost node so it never overlaps (the caller then
  // pans the view to it). The inset from the canvas edge is the pan margin
  // converted to world units, so a spot found here never triggers a pan.
  _moveToFreeSpot(node, size, rects, origin) {
    const gap = GRID * 2;
    const zoom = this._positiveZoom();
    const inset = Math.max(gap, Math.ceil(REVEAL_MARGIN / zoom / GRID) * GRID);
    const free = (x, y) => rects.every((o) => x >= o.x + o.w + gap || x + size.w + gap <= o.x
      || y >= o.y + o.h + gap || y + size.h + gap <= o.y);
    const rect = this.root.getBoundingClientRect();
    const known = rect.width > 0 && rect.height > 0;
    const left = known ? -this.view.x / zoom : origin.x - 800;
    const top = known ? -this.view.y / zoom : origin.y - 500;
    const width = known ? rect.width / zoom : 1600;
    const height = known ? rect.height / zoom : 1000;
    const step = Math.max(gap, snapGridCoordinate(Math.max(width, height) / 100));
    const ceilGrid = (value) => Math.ceil(value / GRID) * GRID;
    const candidates = [];
    for (let y = ceilGrid(top + inset); y + size.h + inset <= top + height; y += step) {
      for (let x = ceilGrid(left + inset); x + size.w + inset <= left + width; x += step) {
        candidates.push([x, y]);
      }
    }
    const distance = ([x, y]) => Math.hypot(x + size.ox - origin.x, y + size.oy - origin.y);
    candidates.sort((p, q) => distance(p) - distance(q));
    const spot = candidates.find(([x, y]) => free(x, y));
    if (spot) {
      node.x = spot[0] + size.ox;
      node.y = spot[1] + size.oy;
    } else {
      const right = rects.reduce((max, o) => Math.max(max, o.x + o.w), origin.x);
      node.x = snapGridCoordinate(right + gap) + size.ox;
      node.y = origin.y;
    }
    return size;
  }

  // Minimal pan that brings the whole footprint into view; one larger than the
  // canvas is aligned to its top-left corner instead.
  _panToReveal(node, size) {
    const rect = this.root.getBoundingClientRect();
    if (!(rect.width > 0 && rect.height > 0)) return;
    const margin = REVEAL_MARGIN;
    const zoom = this._positiveZoom();
    const axis = (position, length, view, extent) => {
      const lo = position * zoom + view;
      const hi = (position + length) * zoom + view;
      if (hi - lo + 2 * margin > extent || lo < margin) return view + margin - lo;
      if (hi > extent - margin) return view + extent - margin - hi;
      return view;
    };
    this.view.x = axis(node.x - size.ox, size.w, this.view.x, rect.width);
    this.view.y = axis(node.y - size.oy, size.h, this.view.y, rect.height);
    this._applyView();
  }

  updateNodeConfig(nodeId, patch) {
    if (this.readOnly) return;
    const n = this.nodes.find((x) => x.id === nodeId);
    if (!n) return;
    // Custom elementy (tf-toggle/tf-select/tf-input) potrafia odpalic `change`
    // przy initial populate w FlowConfig.show — bez no-op guardu to
    // triggerowalo `_pushHistory + _renderSingleNode + onChange`, ktore
    // odbudowywaly DOM node'a i zrywaly selection ~sekunde po kliknieciu.
    let changed = false;
    for (const k of Object.keys(patch)) {
      const current = k === 'repeat' ? n.repeat
        : k === 'activityIo' ? n.activityIo : n.config?.[k];
      if (current !== patch[k]) { changed = true; break; }
    }
    if (!changed) return;
    if (Object.hasOwn(patch, 'repeat')) {
      if (patch.repeat == null) delete n.repeat;
      else n.repeat = structuredClone(patch.repeat);
    }
    if (Object.hasOwn(patch, 'activityIo')) {
      if (patch.activityIo == null) delete n.activityIo;
      else n.activityIo = structuredClone(patch.activityIo);
    }
    const configPatch = Object.fromEntries(Object.entries(patch)
      .filter(([key]) => key !== 'repeat' && key !== 'activityIo'));
    if (this.mode === 'bpmn' && n.type === 'bpmn_call_activity'
      && (Object.hasOwn(configPatch, 'localBody') || Object.hasOwn(configPatch, 'calledDefinitionId'))) {
      n.config = { inputMapping: n.config.inputMapping, outputMapping: n.config.outputMapping,
        ...configPatch };
    } else {
      n.config = { ...n.config, ...configPatch };
    }
    if (this.mode === 'bpmn' && processBoundaryKind(n.type) && Object.hasOwn(patch, 'attachedToId')) {
      this._positionBoundary(n);
    }
    this._pushHistory();
    this._renderSingleNode(n);
    this.onChange();
  }

  /**
   * Attaches a new error or escalation boundary event to a Service task as one
   * undo step. `ref` is the declaration it catches, null for any. The event is
   * named after the code it catches so the table and the canvas read the same.
   */
  addBoundaryNode(parentId, type, ref = null) {
    if (this.readOnly || this.mode !== 'bpmn') return null;
    const parent = this.nodes.find((candidate) => candidate.id === parentId);
    if (!parent || parent.type !== 'bpmn_service_task'
      || !['bpmn_boundary_error', 'bpmn_boundary_escalation'].includes(type)) return null;
    const template = this.templates.get(type);
    const declarations = type === 'bpmn_boundary_error'
      ? { list: this.processModel.errors, id: 'errorId', code: 'errorCode' }
      : { list: this.processModel.escalations, id: 'escalationId', code: 'escalationCode' };
    const code = ref == null ? null : (declarations.list || []).find((entry) => entry[declarations.id] === ref)?.[declarations.code] ?? null;
    const node = {
      id: 'n_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 6),
      type, label: processHandlerName(type, code), x: parent.x, y: parent.y,
      width: template?.width || 56, height: template?.height || 56,
      config: { ...processNodeConfig(processNodeKind(type)), attachedToId: parentId,
        ...(type === 'bpmn_boundary_error' ? { errorRef: ref } : { escalationRef: ref }) },
    };
    if (!this._placeNewBoundary(node, parent)) return null;
    this.nodes.push(node);
    this.render();
    this._pushHistory();
    this.onChange();
    return node;
  }

  // Where the task's own decorations sit, read from the custom properties the
  // stylesheet places them with, so the room left for boundary events cannot
  // drift from what is drawn. The fallbacks only serve a document without
  // layout; they equal the values in flows-builder.css.
  _decorationMetrics(parent) {
    const element = this.nodesLayer?.querySelector(`[data-node-id="${CSS.escape(parent.id)}"]`);
    const style = element && typeof getComputedStyle === 'function' ? getComputedStyle(element) : null;
    const read = (name, fallback) => {
      const value = parseFloat(style?.getPropertyValue(name));
      return Number.isFinite(value) ? value : fallback;
    };
    const size = read('--fb-deco-size', 20);
    const gap = read('--fb-deco-gap', 4);
    const iconLeft = read('--fb-task-icon-left', 10);
    return {
      gap,
      // The icon plus the margin it keeps on its own left, so an event on the top
      // edge clears the icon by as much as the icon clears the edge.
      iconClear: 2 * iconLeft + read('--fb-task-icon-size', 22),
      // The kind badge and the warning marker, both of which may appear later.
      reserve: read('--fb-deco-inset', 6) + 2 * size + 2 * gap,
    };
  }

  // The places on a task's border a boundary event of this size can take, in
  // the order they fill: along the bottom edge from left to right, then along
  // the top edge between the task icon and the badges. The side edges belong to
  // the task's ports, so a task has room for a fixed number of events.
  _boundarySlots(parent, size) {
    const metrics = this._decorationMetrics(parent);
    const half = size.width / 2;
    const pitch = size.width + 4;
    const along = (start, end) => Array.from({ length: Math.max(0, Math.floor((end - start - size.width) / pitch) + 1) },
      (_, index) => parent.x + start + half + index * pitch);
    return [
      ...along(0, parent.width).map((x) => [x, parent.y + parent.height]),
      ...along(metrics.iconClear, parent.width - metrics.reserve).map((x) => [x, parent.y]),
    ];
  }

  _freeBoundarySlots(parent, size, ignore = null) {
    const siblings = this.nodes.filter((candidate) => candidate !== ignore && processBoundaryKind(candidate.type)
      && candidate.config.attachedToId === parent.id);
    const near = (x, y, other) => Math.abs(other.x + other.width / 2 - x) < size.width && Math.abs(other.y + other.height / 2 - y) < size.height;
    return this._boundarySlots(parent, size).filter(([x, y]) => !siblings.some((sibling) => near(x, y, sibling)));
  }

  /** How many more boundary events fit on the task's border. */
  boundaryRoom(parentId) {
    const parent = this.nodes.find((candidate) => candidate.id === parentId);
    if (!parent) return 0;
    const template = this.templates.get('bpmn_boundary_error');
    return this._freeBoundarySlots(parent, { width: template?.width || 56, height: template?.height || 56 }).length;
  }

  // A new boundary event takes the first free slot (or the one nearest to
  // `near`, where it was dropped), preferring a slot no other node covers.
  // False when the border is full: nothing is placed on top of the icon, the
  // ports or another event.
  _placeNewBoundary(node, parent, near = null) {
    const slots = this._freeBoundarySlots(parent, node, node);
    if (!slots.length) return false;
    if (near) slots.sort((left, right) => Math.hypot(left[0] - near.x, left[1] - near.y) - Math.hypot(right[0] - near.x, right[1] - near.y));
    const half = node.width / 2;
    const others = this.nodes.filter((candidate) => candidate !== parent && candidate !== node
      && !(processBoundaryKind(candidate.type) && candidate.config.attachedToId === parent.id));
    const covers = (x, y, other) => x + half > other.x && x - half < other.x + other.width
      && y + node.height / 2 > other.y && y - node.height / 2 < other.y + other.height;
    const slot = slots.find(([x, y]) => !others.some((other) => covers(x, y, other))) ?? slots[0];
    node.x = slot[0] - half;
    node.y = slot[1] - node.height / 2;
    return true;
  }

  // A boundary event dropped from the palette attaches to the activity it lands
  // on (or touches), at the free slot nearest the drop. Dropped anywhere else,
  // or on a task with a full border, it stays unattached and says so.
  _attachDroppedBoundary(node, point) {
    const margin = node.width / 2;
    const hosts = processBoundaryHosts(this, processNodeKind(node.type)).filter((candidate) =>
      point.x >= candidate.x - margin && point.x <= candidate.x + candidate.width + margin
      && point.y >= candidate.y - margin && point.y <= candidate.y + candidate.height + margin);
    if (!hosts.length) {
      this._explainRefusedDrop(node, point, margin);
      return;
    }
    const gap = (candidate) => Math.hypot(
      Math.max(candidate.x - point.x, 0, point.x - candidate.x - candidate.width),
      Math.max(candidate.y - point.y, 0, point.y - candidate.y - candidate.height));
    const host = hosts.sort((left, right) => gap(left) - gap(right))[0];
    node.config.attachedToId = host.id;
    if (!this._placeNewBoundary(node, host, point)) {
      node.config.attachedToId = null;
      toast(I18n.t('bpmn.boundary_no_room', { node: this._nodeName(host) }), 'warning');
    }
  }

  // A boundary event dropped on an activity that cannot take it stays unattached;
  // the toast says why, so it is not left to be worked out from the "Not attached" tag.
  _explainRefusedDrop(node, point, margin) {
    const refusals = processBoundaryRefusals(this, processNodeKind(node.type));
    const target = this.nodes.filter((candidate) => refusals.get(candidate.id)
      && point.x >= candidate.x - margin && point.x <= candidate.x + candidate.width + margin
      && point.y >= candidate.y - margin && point.y <= candidate.y + candidate.height + margin)[0];
    if (target) toast(I18n.t(`bpmn.boundary_refused_${refusals.get(target.id)}`, { node: this._nodeName(target) }), 'warning');
  }

  _positionBoundary(node) {
    const parent = this.nodes.find((candidate) => candidate.id === node.config?.attachedToId);
    if (!parent || !['bpmn_user_task', 'bpmn_service_task', 'bpmn_sub_process', 'bpmn_call_activity',
      'bpmn_send_task', 'bpmn_manual_task', 'bpmn_receive_task', 'bpmn_script_task'].includes(parent.type)) return;
    const ports = [this._getPortWorldPos(parent.id, 'in', 'in'), this._getPortWorldPos(parent.id, 'full', 'out')];
    const clearsPorts = (x, y) => ports.every((port) => Math.hypot(x - port.x, y - port.y) >= Math.max(node.width, node.height) / 2 + 16);
    const centerX = node.x + node.width / 2;
    const centerY = node.y + node.height / 2;
    let x = Math.max(parent.x, Math.min(centerX, parent.x + parent.width));
    let y = Math.max(parent.y, Math.min(centerY, parent.y + parent.height));
    if (x === centerX && y === centerY) {
      const sides = [
        [centerX - parent.x, parent.x, centerY],
        [parent.x + parent.width - centerX, parent.x + parent.width, centerY],
        [centerY - parent.y, centerX, parent.y],
        [parent.y + parent.height - centerY, centerX, parent.y + parent.height],
      ];
      sides.sort((left, right) => left[0] - right[0]);
      [, x, y] = sides[0];
    }
    const occupied = this.nodes.filter((candidate) => candidate !== node && processBoundaryKind(candidate.type)
      && candidate.config.attachedToId === parent.id);
    if (!clearsPorts(x, y) || occupied.some((candidate) => Math.hypot(candidate.x + candidate.width / 2 - x, candidate.y + candidate.height / 2 - y) < node.width)) {
      const positions = [];
      for (let offset = 0; offset <= parent.width; offset += node.width) {
        positions.push([parent.x + offset, parent.y], [parent.x + offset, parent.y + parent.height]);
      }
      positions.push([parent.x + parent.width, parent.y], [parent.x + parent.width, parent.y + parent.height]);
      for (let offset = 0; offset <= parent.height; offset += node.height) {
        positions.push([parent.x, parent.y + offset], [parent.x + parent.width, parent.y + offset]);
      }
      positions.sort((left, right) => Math.hypot(left[0] - x, left[1] - y) - Math.hypot(right[0] - x, right[1] - y));
      const free = positions.find(([candidateX, candidateY]) => clearsPorts(candidateX, candidateY) && occupied.every((candidate) => Math.hypot(
        candidate.x + candidate.width / 2 - candidateX,
        candidate.y + candidate.height / 2 - candidateY,
      ) >= node.width));
      if (free) [x, y] = free;
    }
    node.x = x - node.width / 2;
    node.y = y - node.height / 2;
  }

  updateNodeLabel(nodeId, label) {
    if (this.readOnly) return;
    const n = this.nodes.find((x) => x.id === nodeId);
    if (!n) return;
    if ((n.label || '') === (label || '')) return;
    n.label = label;
    this._pushHistory();
    this._renderSingleNode(n);
    this.onChange();
  }

  _normalizeNodeLabels() {
    if (this.mode === 'bpmn') return;
    if (!Array.isArray(this.nodes) || this.nodes.length === 0) return;
    for (const node of this.nodes) {
      // Preset dzieli node_type z innymi presetami, więc jego etykieta jest
      // jedynym śladem, który z nich wybrano — nie wolno jej wyzerować.
      if (this.presetTypes.has(node.type)) continue;
      const template = this.templates.get(node.type);
      if (isAutoNodeLabel(node.label, node.type, template?.label)) {
        node.label = '';
      }
    }
  }

  removeNodes(ids) {
    if (this.readOnly) return;
    const idSet = new Set(ids);
    if (this.mode === 'bpmn') {
      for (const node of this.nodes) {
        if (processBoundaryKind(node.type) && idSet.has(node.config.attachedToId)) idSet.add(node.id);
      }
    }
    this.nodes = this.nodes.filter((n) => !idSet.has(n.id));
    this.edges = this.edges.filter((e) => !idSet.has(e.from_node) && !idSet.has(e.to_node));
    this._clearRemovedDefaults();
    this.selectedIds.clear();
    this._pushHistory();
    this.render();
    this.onChange();
  }

  duplicateNodes(ids) {
    if (this.readOnly) return;
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
        config: structuredClone(n.config),
        ...(n.repeat == null ? {} : { repeat: structuredClone(n.repeat) }),
        ...(n.activityIo == null ? {} : { activityIo: cloneProcessActivityIo(n.activityIo) }),
      };
      idMap.set(n.id, clone.id);
      if (this.mode === 'bpmn' && ['bpmn_exclusive_gateway', 'bpmn_inclusive_gateway'].includes(clone.type)) clone.config.defaultFlowId = null;
      if (this.mode === 'bpmn' && clone.type === 'bpmn_sub_process') clone.config.body = cloneProcessBody(clone.config.body);
      clones.push(clone);
    }
    if (this.mode === 'bpmn') {
      for (const clone of clones) {
        if (!processBoundaryKind(clone.type)) continue;
        clone.config.attachedToId = idMap.get(clone.config.attachedToId) || clone.config.attachedToId;
      }
    }
    this.nodes.push(...clones);
    if (this.mode === 'bpmn') clones.filter((clone) => processBoundaryKind(clone.type)).forEach((clone) => this._positionBoundary(clone));
    this.selectedIds = new Set(clones.map((c) => c.id));
    this._pushHistory();
    this.render();
    this.onChange();
    if (clones.length === 1) this.selectNode(clones[0].id);
    else this.onSelect(null);
  }

  // Czy dodanie krawedzi from→to zamknie cykl? Sprawdzamy czy `from` jest juz
  // osiagalny z `to` przez krawedzie FORWARD (pomijajac loop_back). Jesli tak,
  // nowa krawedz from→to jest wsteczna. Uzywane do auto-oznaczenia loop_back.
  _wouldFormCycle(fromId, toId) {
    const adj = new Map();
    for (const e of this.edges) {
      if (e.kind === 'loop_back') continue;
      if (!adj.has(e.from_node)) adj.set(e.from_node, []);
      adj.get(e.from_node).push(e.to_node);
    }
    const seen = new Set();
    const stack = [toId];
    while (stack.length) {
      const cur = stack.pop();
      if (cur === fromId) return true;
      if (seen.has(cur)) continue;
      seen.add(cur);
      for (const next of adj.get(cur) || []) stack.push(next);
    }
    return false;
  }

  // Grupuje wybrane wezly we wspolny region petli (Part 5). Jesli ktorykolwiek
  // z zaznaczonych wezlow ma juz region, dolaczamy reszte do niego; w
  // przeciwnym razie generujemy nowy `region` id. Zwraca id regionu.
  groupIntoRegion(ids) {
    if (this.readOnly) return;
    const idSet = new Set(ids);
    const members = this.nodes.filter((n) => idSet.has(n.id));
    if (members.length === 0) return null;
    const existing = members.map((n) => n.region).find((r) => r);
    const regionId = existing || genRegionId();
    for (const n of members) n.region = regionId;
    this._pushHistory();
    this.render();
    this.onChange();
    return regionId;
  }

  // Usuwa wezly z regionu petli. Jesli krawedz `loop_back` traci przynaleznosc
  // (jeden z koncow opuscil region), degradujemy ja do zwyklej krawedzi, bo
  // loop_back poza regionem jest niewalidowalna (R11).
  ungroupRegion(ids) {
    if (this.readOnly) return;
    const idSet = new Set(ids);
    let touched = false;
    for (const n of this.nodes) {
      if (idSet.has(n.id) && n.region) { n.region = undefined; touched = true; }
    }
    if (!touched) return;
    for (const e of this.edges) {
      if (e.kind !== 'loop_back') continue;
      const from = this.nodes.find((n) => n.id === e.from_node);
      const to = this.nodes.find((n) => n.id === e.to_node);
      if (!from?.region || from.region !== to?.region) delete e.kind;
    }
    this._pushHistory();
    this.render();
    this.onChange();
  }

  // Loop-region role of a node: 'entry' (target of the region's loop_back edge,
  // carries the region-level loop config), 'exit' (source of the loop_back edge),
  // 'member' (in the region but neither endpoint) or null (no region). The config
  // panel surfaces loop_max_iterations/loop_final_pass only on the entry, and the
  // region box labels entry/exit ports — both read this single source of truth.
  regionRole(nodeId) {
    const node = this.nodes.find((n) => n.id === nodeId);
    if (!node || !node.region) return null;
    for (const e of this.edges) {
      if (e.kind !== 'loop_back') continue;
      const from = this.nodes.find((n) => n.id === e.from_node);
      const to = this.nodes.find((n) => n.id === e.to_node);
      if (!from?.region || from.region !== to?.region) continue;
      if (from.region !== node.region) continue;
      if (e.to_node === nodeId) return 'entry';
      if (e.from_node === nodeId) return 'exit';
    }
    return 'member';
  }

  // Czy zaznaczenie zawiera wezel z regionem? Steruje pozycjami menu
  // kontekstowego (Zgrupuj / Rozgrupuj).
  selectionHasRegion() {
    for (const id of this.selectedIds) {
      const n = this.nodes.find((x) => x.id === id);
      if (n?.region) return true;
    }
    return false;
  }

  deleteSelected() {
    if (this.readOnly) return;
    if (this.selectedIds.size > 0) {
      this.removeNodes([...this.selectedIds]);
      return;
    }
    if (this.selectedEdgeId) {
      this.edges = this.edges.filter((e) => e.id !== this.selectedEdgeId);
      this._clearRemovedDefaults();
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

  selectEdge(id) {
    this.selectedIds.clear();
    this.selectedEdgeId = id;
    this._applySelectionClasses();
    this._renderEdges();
    this.onSelect(null, this.edges.find((edge) => edge.id === id));
  }

  connectNodes(sourceId, targetId, fromPort = 'full', toPort = 'in') {
    if (this.readOnly || sourceId === targetId) return false;
    const source = this.nodes.find((node) => node.id === sourceId);
    const target = this.nodes.find((node) => node.id === targetId);
    if (!source || !target) return false;
    if (this.mode === 'bpmn' && (['bpmn_end', 'bpmn_error_end', 'bpmn_terminate_end', 'bpmn_link_throw'].includes(source.type) || (['bpmn_start', 'bpmn_timer_start', 'bpmn_message_start', 'bpmn_link_catch'].includes(target.type) || processBoundaryKind(target.type)))) return false;
    if (this.edges.some((edge) => edge.from_node === sourceId && edge.to_node === targetId && edge.from_port === fromPort && edge.to_port === toPort)) return false;
    const edge = { id: `e_${crypto.randomUUID().replaceAll('-', '_')}`, from_node: sourceId, to_node: targetId, from_port: fromPort, to_port: toPort };
    if (this.mode === 'bpmn') edge.condition = null;
    if (source.region && source.region === target.region && this._wouldFormCycle(sourceId, targetId)) edge.kind = 'loop_back';
    this.edges.push(edge);
    this._pushHistory();
    this._renderEdges();
    this.onChange();
    return true;
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
    this._renderRegions();
    this._renderNodes();
    this._renderProcessModeling();
    this._layoutProcessLabels();
    this._renderEdges();
    this._applyView();
    if (this.hintEl) this.hintEl.style.display = this.nodes.length === 0 ? 'flex' : 'none';
  }

  _renderProcessModeling() {
    if (this.mode !== 'bpmn') return;
    const body = processBody(this.processModel, this.processPath, this.selectedProcessId);
    const modeling = body.modeling;
    const shapes = body.diagram.modelingShapes || [];
    const byId = new Map(shapes.map((shape) => [shape.elementId, shape]));
    const place = (element, shape) => {
      element.style.position = 'absolute';
      element.style.left = `${shape.x}px`;
      element.style.top = `${shape.y}px`;
      element.style.width = `${shape.width}px`;
      element.style.height = `${shape.height}px`;
      element.style.boxSizing = 'border-box';
      element.style.pointerEvents = 'none';
      element.style.overflow = 'visible';
      element.style.overflowWrap = 'anywhere';
      element.style.whiteSpace = 'pre-wrap';
    };
    const collaboration = this.processModel.collaboration;
    if (!this.processPath.length && collaboration) {
      const pools = new Map(collaboration.diagram.modelingShapes.map((shape) => [shape.elementId, shape]));
      for (const participant of collaboration.participants) {
        const shape = pools.get(participant.id);
        if (!shape) continue;
        const element = document.createElement('div');
        element.dataset.modelingId = participant.id;
        element.style.border = '3px solid var(--color-border, #64748b)';
        element.style.background = 'rgba(100, 116, 139, 0.035)';
        element.style.color = 'var(--color-text, #334155)';
        element.style.padding = '6px';
        element.textContent = participant.name ?? participant.id;
        place(element, shape);
        this.regionsLayer.appendChild(element);
      }
    }
    const drawLaneSets = (sets) => {
      for (const set of sets) for (const lane of set.lanes) {
        const shape = byId.get(lane.id);
        if (shape) {
          const element = document.createElement('div');
          element.dataset.modelingId = lane.id;
          element.style.border = '2px solid var(--color-border, #64748b)';
          element.style.background = 'rgba(100, 116, 139, 0.05)';
          element.style.color = 'var(--color-text, #334155)';
          element.style.padding = '6px';
          element.textContent = lane.name ?? lane.id;
          place(element, shape);
          this.regionsLayer.appendChild(element);
        }
        drawLaneSets(lane.childLaneSets || []);
      }
    };
    drawLaneSets(modeling?.laneSets || []);
    for (const item of [...(modeling?.dataObjectReferences || []), ...(modeling?.dataStoreReferences || []),
      ...(modeling?.textAnnotations || [])]) {
      const shape = byId.get(item.id);
      if (!shape) continue;
      const element = document.createElement('div');
      element.dataset.modelingId = item.id;
      element.style.border = item.text == null ? '2px solid var(--color-border, #64748b)' : '2px dashed var(--color-border, #64748b)';
      element.style.borderRadius = '4px';
      element.style.background = 'var(--color-surface, #fff)';
      element.style.color = 'var(--color-text, #334155)';
      element.style.padding = '6px';
      element.textContent = item.text ?? item.name ?? item.id;
      place(element, shape);
      this.nodesLayer.appendChild(element);
    }

  }

  // Rysuje wizualne kontenery regionow petli: prostokat-tlo obejmujacy bounding
  // box wszystkich wezlow o tym samym `node.region`, z etykieta. Kontener jest
  // za wezlami (osobna warstwa `.fb-regions-layer`, pointer-events: none), wiec
  // nie blokuje przeciagania ani laczenia portow.
  _renderRegions() {
    if (!this.regionsLayer) return;
    this.regionsLayer.innerHTML = '';
    const byRegion = new Map();
    for (const n of this.nodes) {
      if (!n.region) continue;
      if (!byRegion.has(n.region)) byRegion.set(n.region, []);
      byRegion.get(n.region).push(n);
    }
    const pad = 28;
    for (const [regionId, members] of byRegion) {
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      for (const n of members) {
        minX = Math.min(minX, n.x);
        minY = Math.min(minY, n.y);
        maxX = Math.max(maxX, n.x + NODE_WIDTH);
        maxY = Math.max(maxY, n.y + NODE_H_APPROX);
      }
      const box = document.createElement('div');
      box.className = 'fb-region-box';
      box.dataset.regionId = regionId;
      box.style.left = `${minX - pad}px`;
      box.style.top = `${minY - pad - 18}px`;
      box.style.width = `${(maxX - minX) + pad * 2}px`;
      box.style.height = `${(maxY - minY) + pad * 2 + 18}px`;
      // Surface which member is the region entry (loop config + loop_back target)
      // and which is the exit (loop_back source) so the user knows where to wire
      // the loop. Derived from the same regionRole() the config panel reads.
      const entry = members.find((m) => this.regionRole(m.id) === 'entry');
      const exit = members.find((m) => this.regionRole(m.id) === 'exit');
      const roleHints = [];
      if (entry) {
        const name = getNodeDisplayTitle(entry, this.templates.get(entry.type));
        roleHints.push(`<span class="fb-region-role entry">${escapeHtml(I18n.t('flows_builder.loop_region_entry'))}: ${escapeHtml(name)}</span>`);
      }
      if (exit) {
        const name = getNodeDisplayTitle(exit, this.templates.get(exit.type));
        roleHints.push(`<span class="fb-region-role exit">${escapeHtml(I18n.t('flows_builder.loop_region_exit'))}: ${escapeHtml(name)}</span>`);
      }
      const label = document.createElement('div');
      label.className = 'fb-region-label';
      label.innerHTML = `<svg aria-hidden="true"><use href="#i-rotate"/></svg><span>${escapeHtml(I18n.t('flows_builder.loop_region_label', { id: regionId }))}</span>${roleHints.join('')}`;
      box.appendChild(label);
      this.regionsLayer.appendChild(box);
    }
  }

  _renderNodes() {
    this.nodesLayer.innerHTML = '';
    const ordered = this.mode === 'bpmn'
      ? [...this.nodes.filter((node) => !processBoundaryKind(node.type)),
        ...this.nodes.filter((node) => processBoundaryKind(node.type))]
      : this.nodes;
    for (const n of ordered) {
      const element = this._buildNodeEl(n);
      this.nodesLayer.appendChild(element);
      if (this.mode === 'bpmn' && processBoundaryKind(n.type)) this._bringToFront(element);
    }
    this._applySelectionClasses();
  }

  _renderSingleNode(n) {
    const old = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(n.id)}"]`);
    const fresh = this._buildNodeEl(n);
    if (old) old.replaceWith(fresh);
    else this.nodesLayer.appendChild(fresh);
    if (this.mode === 'bpmn' && processBoundaryKind(n.type)) this._bringToFront(fresh);
    this._layoutProcessLabels();
    this._applySelectionClasses();
    this._renderRegions();
    this._renderEdges();
  }

  _buildNodeEl(n) {
    const tmpl = this.templates.get(n.type);
    const div = document.createElement('div');
    div.className = 'fb-node';
    div.dataset.nodeId = n.id;
    div.style.left = `${n.x}px`;
    div.style.top = `${n.y}px`;
    div.style.width = `${NODE_WIDTH}px`;
    div.style.setProperty('--node-color', `var(${nodeColorVar(n.type, tmpl?.category)})`);

    if (this.mode === 'bpmn') {
      div.tabIndex = 0;
      div.setAttribute('role', 'button');
      div.setAttribute('aria-label', n.label || tmpl.label);
      div.addEventListener('keydown', (event) => {
        if (event.key !== 'Enter' && event.key !== ' ') return;
        event.preventDefault();
        this.selectNode(n.id);
      });
      div.classList.add('fb-process-node', n.type);
      div.style.width = `${n.width}px`;
      div.style.height = `${n.height}px`;
      const title = getNodeDisplayTitle(n, tmpl);
      const event = (['bpmn_start', 'bpmn_end', 'bpmn_error_end', 'bpmn_terminate_end', 'bpmn_timer_start', 'bpmn_timer_catch', 'bpmn_message_start', 'bpmn_message_catch', 'bpmn_message_throw', 'bpmn_signal_throw', 'bpmn_signal_catch', 'bpmn_link_throw', 'bpmn_link_catch'].includes(n.type) || processBoundaryKind(n.type));
      const markerless = ['bpmn_start', 'bpmn_end'].includes(n.type);
      if (processBoundaryKind(n.type)) {
        div.classList.add(n.type === 'bpmn_boundary_error' || n.config.cancelActivity ? 'fb-boundary-interrupting' : 'fb-boundary-noninterrupting');
      }
      const gateway = n.type.endsWith('_gateway');
      const repeatMarker = n.repeat?.StructuredLoop ? 'loop'
        : n.repeat?.MultiInstance?.mode === 'Sequential' ? 'sequential'
          : n.repeat?.MultiInstance?.mode === 'Parallel' ? 'parallel' : null;
      const serviceKind = n.type === 'bpmn_service_task' ? processServiceKind(n.config) : null;
      const showKind = serviceKind && (serviceKind !== 'flow' || !!n.config.flowId);
      const kindIcon = { flow: 'flow', block: 'chip', tool: 'puzzle', agent: 'bot' }[serviceKind];
      const warnings = this._processWarnings(n);
      // An event that is not on an activity sits on the canvas like one that is;
      // a dashed outline and its own tag say it is waiting for a host.
      if (warnings.includes('boundary_unattached')) {
        div.classList.add('fb-unattached');
        div.dataset.unattachedLabel = I18n.t('bpmn.boundary_not_attached');
      }
      const warningText = warnings.map((key) => this._processWarningText(n, key)).join(' ');
      div.innerHTML = `
        ${showKind ? `<span class="fb-process-kind" data-kind="${serviceKind}" role="img" title="${escapeAttr(I18n.t(`bpmn.kind_${serviceKind}`))}" aria-label="${escapeAttr(I18n.t(`bpmn.kind_${serviceKind}`))}"><svg aria-hidden="true"><use href="#i-${kindIcon}"/></svg></span>` : ''}
        ${warnings.length ? `<span class="fb-process-warn" data-warning="${warnings.join(' ')}" role="img" title="${escapeAttr(warningText)}" aria-label="${escapeAttr(warningText)}"><svg aria-hidden="true"><use href="#i-alert"/></svg></span>` : ''}
        <div class="fb-process-symbol">${markerless ? '' : `<svg aria-hidden="true"><use href="#i-${escapeAttr(tmpl.icon)}"/></svg>`}
          ${event || gateway ? '' : `<span>${escapeHtml(title)}</span>`}
          ${repeatMarker ? `<i class="fb-repeat-marker fb-repeat-marker-${repeatMarker}" aria-hidden="true">${repeatMarker === 'loop' ? '↻' : ''}</i>` : ''}</div>
        <div class="fb-process-label" title="${escapeAttr(title)}" ${event || gateway ? '' : 'hidden'}>${breakableHtml(title)}</div>
        <span class="fb-process-label-leader" aria-hidden="true"></span>
        ${(['bpmn_start', 'bpmn_timer_start', 'bpmn_message_start', 'bpmn_link_catch'].includes(n.type) || processBoundaryKind(n.type)) ? '' : this._renderPortEl(n.id, { name: 'in', type: 'any' }, 0, 'in', 1)}
        ${['bpmn_end', 'bpmn_error_end', 'bpmn_terminate_end', 'bpmn_link_throw'].includes(n.type) ? '' : this._renderPortEl(n.id, { name: 'full', type: 'any' }, 0, 'out', 1)}`;
      div.querySelectorAll('.fb-port').forEach((port) => { port.style.top = `${n.height / 2 - 8}px`; });
      div.setAttribute('aria-label', title);
      return div;
    }

    const missing = this._missingRequired(n);
    if (missing.length) div.classList.add('error');

    const iconId = nodeIconId(n.type, tmpl?.icon, tmpl?.category);
    const cat = TYPE_CATEGORY[n.type] || n.type;
    const title = getNodeDisplayTitle(n, tmpl);

    const { inputs, outputs } = portsForNode(n, tmpl);

    // Wysokosc nody musi pomiescic obie strony portow (in/out). Naszym
    // pivotem jest strona z wieksza liczba portow. Header to PORT_HEADER_OFFSET
    // (44px) + 14px stopka per port. Bez tego node ma sztywne CSS height i
    // przy 6+ portach (trigger) ostatnie wystaja na zewnatrz dolnej krawedzi.
    const portCount = Math.max(inputs.length, outputs.length, 1);
    const minHeight = PORT_HEADER_OFFSET + portCount * PORT_STEP + 14;
    div.style.minHeight = `${minHeight}px`;
    div.style.setProperty('--fb-lane-in', inputs.length >= 2 ? `${PORT_LABEL_LANE}px` : '0px');
    div.style.setProperty('--fb-lane-out', outputs.length >= 2 ? `${PORT_LABEL_LANE}px` : '0px');

    const inPortsHtml = inputs.map((p, i) => this._renderPortEl(n.id, p, i, 'in', inputs.length)).join('');
    const outPortsHtml = outputs.map((p, i) => this._renderPortEl(n.id, p, i, 'out', outputs.length)).join('');

    // Ostrzeżenie bez powodu straszy i nic nie mówi, więc obwódka i trójkąt
    // idą w parze z listą brakujących pól — w tooltipie i wprost w ciele bloku.
    const missingNames = missing.join(', ');
    div.innerHTML = `
      <div class="fb-node-header">
        <div class="fb-node-badge"><svg><use href="#i-${iconId}"/></svg></div>
        <div class="fb-node-title">${escapeHtml(title)}</div>
        ${missing.length
          ? `<span class="fb-node-error-icon" title="${escapeAttr(I18n.t('flows_builder.node_error_tooltip', { fields: missingNames }))}"><svg><use href="#i-alert"/></svg></span>`
          : `<span class="fb-node-type">${escapeHtml(cat)}</span>`}
      </div>
      <div class="fb-node-body">${this._renderMissingRows(missing)}${this._renderNodeSummary(n, missing.length === 0)}</div>
      ${inPortsHtml}
      ${outPortsHtml}
    `;
    return div;
  }

  // Runs in phases that each only read or only write the DOM: a read after a
  // write forces a layout, and one per node made an edit on a large diagram take
  // a fifth of a second.
  _layoutProcessLabels() {
    if (this.mode !== 'bpmn') return;
    // Sizes read as zero while the canvas has no layout (a hidden tab); placing
    // from them would strip every label, so the last placement stays until the
    // canvas is shown.
    // Safari before 17.4 has no checkVisibility; a box without client rects is
    // the same "not rendered" answer.
    this._labelsUnplaced = typeof this.root.checkVisibility === 'function'
      ? !this.root.checkVisibility() : this.root.getClientRects().length === 0;
    if (this._labelsUnplaced) return;
    const nodeRects = this.nodes.map((node) => ({
      id: node.id, left: node.x, top: node.y, right: node.x + node.width, bottom: node.y + node.height,
    }));
    const entries = [];
    for (const node of this.nodes) {
      const element = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(node.id)}"]`);
      const label = element?.querySelector('.fb-process-label');
      if (!label) continue;
      entries.push({ node, element, label, title: element.querySelector('.fb-process-symbol > span'),
        leader: element.querySelector('.fb-process-label-leader') });
    }
    // A title shows inside its box until it does not fit, so every title is
    // measured in its inner state before any element is moved outside.
    for (const { element } of entries) {
      element.classList.remove('fb-external-label', 'fb-label-deferred');
      element.removeAttribute('title');
    }
    for (const entry of entries) {
      const { title } = entry;
      entry.outside = !title || title.scrollHeight > title.clientHeight + 1 || title.scrollWidth > title.clientWidth + 1;
    }
    const labels = [];
    for (const entry of entries) {
      const { element, label, leader, title, outside } = entry;
      element.classList.toggle('fb-external-label', outside && !!title);
      label.hidden = !outside;
      leader.hidden = !outside;
      if (outside) labels.push(entry);
    }
    for (const entry of labels) {
      entry.width = entry.label.offsetWidth;
      entry.height = entry.label.offsetHeight;
    }
    labels.sort((left, right) => {
      const priority = ({ node }) => processBoundaryKind(node.type) ? 1
        : ['bpmn_user_task', 'bpmn_service_task'].includes(node.type) ? 0 : 2;
      return priority(left) - priority(right) || left.node.id.localeCompare(right.node.id);
    });
    // The "Not attached" tag sits above its event; a label must not land under it.
    const tags = this.nodes.filter((node) => this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(node.id)}"]`)?.classList.contains('fb-unattached'))
      .map((node) => ({ id: `${node.id}#tag`, left: node.x + node.width / 2 - UNATTACHED_TAG.width / 2, right: node.x + node.width / 2 + UNATTACHED_TAG.width / 2,
        top: node.y - UNATTACHED_TAG.offset - UNATTACHED_TAG.height, bottom: node.y - UNATTACHED_TAG.offset }));
    const plan = planProcessLabels({
      nodes: [...nodeRects, ...tags],
      labels: labels.filter(({ width, height }) => width && height).map(({ node, element, width, height }) => ({
        id: node.id, left: node.x, top: node.y, right: node.x + node.width, bottom: node.y + node.height, width, height,
        external: element.classList.contains('fb-external-label'),
        host: processBoundaryKind(node.type) ? nodeRects.find((candidate) => candidate.id === node.config.attachedToId) ?? null : null,
      })),
    });
    const writes = [];
    for (const { node, element, label, leader, title } of labels) {
      if (!plan.has(node.id)) continue;
      const placement = plan.get(node.id);
      if (!placement) {
        // Nowhere to put it without covering something or crossing an element. A
        // title returns inside its box (clipped, the full text stays in the
        // tooltip); an event has no title inside, so its name shows on hover and
        // selection and stays in the tooltip and the accessible name.
        leader.hidden = true;
        if (title) {
          label.hidden = true;
          element.classList.remove('fb-external-label');
        } else {
          writes.push({ label, leader, deferred: true, element, name: label.textContent });
        }
        continue;
      }
      const { rect, leader: line } = placement;
      writes.push({ label, leader, element, left: rect.left - node.x, top: rect.top - node.y,
        leaderLeft: line.sourceX - node.x, leaderTop: line.sourceY - node.y,
        distance: Math.hypot(line.targetX - line.sourceX, line.targetY - line.sourceY),
        angle: Math.atan2(line.targetY - line.sourceY, line.targetX - line.sourceX) });
    }
    for (const { label, leader, element, left, top, leaderLeft, leaderTop, distance, angle, deferred, name } of writes) {
      if (deferred) {
        element.classList.add('fb-label-deferred');
        label.style.left = '';
        label.style.top = '';
        element.title = name;
        continue;
      }
      label.style.left = `${left}px`;
      label.style.top = `${top}px`;
      leader.style.left = `${leaderLeft}px`;
      leader.style.top = `${leaderTop}px`;
      leader.style.width = `${distance}px`;
      leader.style.transform = `rotate(${angle}rad)`;
      leader.hidden = distance === 0;
    }
  }

  _renderMissingRows(missing) {
    return missing.map((field) => `
      <div class="fb-node-row" title="${escapeAttr(I18n.t('flows_builder.node_error_field', { field }))}">
        <span class="fb-key">${escapeHtml(field)}</span>
        <span class="fb-val missing">${escapeHtml(I18n.t('flows_builder.node_error_required'))}</span>
      </div>`).join('');
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
    // The tooltip must say WHY a port is dim, otherwise it reads as broken.
    const tooltip = port.unsupported === true
      ? `${portLabel} • ${portType} — ${port.unsupportedReason}`
      : `${portLabel} • ${portType}`;
    const labelHtml = showLabel
      ? `<span class="fb-port-label">${escapeHtml(port.name)}</span>`
      : '';
    const unsupported = port.unsupported === true;
    const cls = `fb-port fb-port-${side === 'in' ? 'in' : 'out'} fb-port-type-${portType}`
      + (unsupported ? ' fb-port-unsupported' : '');
    return `<div class="${cls}" data-node-id="${escapeAttr(nodeId)}" data-port="${escapeAttr(port.name)}" data-port-kind="${escapeAttr(port.name)}" data-port-type="${escapeAttr(portType)}" data-port-idx="${idx}" style="top:${top}px;" title="${escapeAttr(tooltip)}">${labelHtml}</div>`;
  }

  _renderNodeSummary(n, withPlaceholder = true) {
    const c = n.config || {};
    const keys = Object.keys(c).filter((k) => c[k] !== null && c[k] !== undefined && c[k] !== '');
    if (keys.length === 0) {
      return withPlaceholder ? '<div class="fb-node-row"><span class="fb-key">—</span></div>' : '';
    }
    return keys.slice(0, 4).map((k) => {
      const v = c[k];
      const s = typeof v === 'object' ? JSON.stringify(v) : String(v);
      // Truncate w CSS przez ellipsę — tu tylko udostępniamy pełną wartość w title
      return `<div class="fb-node-row" title="${escapeAttr(k + ': ' + s)}"><span class="fb-key">${escapeHtml(k)}</span><span class="fb-val">${escapeHtml(s)}</span></div>`;
    }).join('');
  }

  /** Etykiety wymaganych pól bez wartości, w kolejności ze schematu. */
  _missingRequired(n) {
    const tmpl = this.templates.get(n.type);
    if (!tmpl || !tmpl.params_schema) return [];
    let schema;
    try { schema = typeof tmpl.params_schema === 'string' ? JSON.parse(tmpl.params_schema) : tmpl.params_schema; }
    catch (_) { return []; }
    const missing = [];
    for (const key of schema.required || []) {
      const v = n.config?.[key];
      if (v === null || v === undefined || v === '') {
        missing.push(schema.properties?.[key]?.title || key);
      }
    }
    return missing;
  }

  /** Ile bloków na płótnie ma nieuzupełnione pola wymagane — czyli ile nosi
   *  czerwoną obwódkę i wiersz „wymagane". Topbar mówi o tym wprost, bo sam
   *  „Zapisano" przy trzech takich blokach przeczy temu, co widać na płótnie. */
  incompleteNodeCount() {
    return this.nodes.reduce((n, node) => n + (this._missingRequired(node).length ? 1 : 0), 0);
  }

  // Zwraca pozycję portu w WORLD coords (bez pan/zoom). Portu szukamy po
  // nazwie; jesli nie ma — bierzemy pierwszy z listy (fallback dla krawedzi
  // zapisanych pod inna nazwa portu, np. legacy "out"). Lokalizacja
  // geometryczna jest tylko sprawa rysowania — walidacja nazw odbywa sie
  // osobno w _validate().
  _getPortWorldPos(nodeId, portName, side) {
    const node = this.nodes.find((n) => n.id === nodeId);
    if (!node) return { x: 0, y: 0 };
    if (this.mode === 'bpmn') return { x: side === 'in' ? node.x : node.x + node.width, y: node.y + node.height / 2 };
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

  _appendBpmnEdgeMarkers() {
    if (this.mode !== 'bpmn') return;
    const svgNs = 'http://www.w3.org/2000/svg';
    const defs = document.createElementNS(svgNs, 'defs');
    const marker = document.createElementNS(svgNs, 'marker');
    marker.setAttribute('id', this._edgeMarkerId);
    marker.setAttribute('viewBox', '0 0 10 8');
    marker.setAttribute('markerWidth', '10');
    marker.setAttribute('markerHeight', '8');
    marker.setAttribute('refX', '9');
    marker.setAttribute('refY', '4');
    marker.setAttribute('markerUnits', 'userSpaceOnUse');
    marker.setAttribute('orient', 'auto');
    const path = document.createElementNS(svgNs, 'path');
    path.setAttribute('d', 'M 0 0 L 9 4 L 0 8 Z');
    path.setAttribute('fill', 'context-stroke');
    path.setAttribute('stroke', 'none');
    marker.appendChild(path);
    defs.appendChild(marker);
    this.svg.appendChild(defs);
  }

  _renderEdges() {
    // Zbudujmy SVG na nowo — edges zwykle nieliczne.
    const svgNs = 'http://www.w3.org/2000/svg';
    this.svg.innerHTML = '';
    this._appendBpmnEdgeMarkers();
    for (const e of this.edges) {
      const from = this.nodes.find((n) => n.id === e.from_node);
      const to = this.nodes.find((n) => n.id === e.to_node);
      if (!from || !to) continue;
      const fp = this._getPortWorldPos(e.from_node, e.from_port || 'full', 'out');
      const tp = this._getPortWorldPos(e.to_node, e.to_port || 'in', 'in');
      const points = this.mode === 'bpmn' ? this._processEdgePoints(e) : null;
      const d = points ? points.map((point, index) => `${index ? 'L' : 'M'} ${point.x} ${point.y}`).join(' ') : this._bezierPath(fp.x, fp.y, tp.x, tp.y);
      // Hit area
      const hit = document.createElementNS(svgNs, 'path');
      hit.setAttribute('class', 'fb-edge-hit');
      hit.setAttribute('d', d);
      hit.dataset.edgeId = e.id;
      if (this.mode === 'bpmn') {
        hit.setAttribute('tabindex', '0');
        hit.setAttribute('role', 'button');
        hit.setAttribute('aria-label', `${getNodeDisplayTitle(from, this.templates.get(from.type))} → ${getNodeDisplayTitle(to, this.templates.get(to.type))}`);
        hit.addEventListener('keydown', (event) => {
          if (event.key !== 'Enter' && event.key !== ' ') return;
          event.preventDefault();
          this.selectEdge(e.id);
        });
      }
      this.svg.appendChild(hit);
      // Visible path. Loop-back edges (region cykl) rysowane przerywana linia
      // (klasa `loop-back`) + znacznik "loop" na srodku, zeby odroznic je od
      // zwyklych krawedzi forward.
      const isLoopBack = e.kind === 'loop_back';
      const p = document.createElementNS(svgNs, 'path');
      p.setAttribute('class', isLoopBack ? 'fb-edge-path loop-back' : 'fb-edge-path');
      p.setAttribute('d', d);
      p.dataset.edgeId = e.id;
      const isSelected = this.selectedEdgeId === e.id;
      if (isSelected) p.classList.add('selected');
      if (this.mode === 'bpmn') {
        p.setAttribute('marker-end', `url(#${this._edgeMarkerId})`);
      }
      this.svg.appendChild(p);
      if (this.mode === 'bpmn') {
        const text = document.createElementNS(svgNs, 'text');
        const middle = points[Math.floor(points.length / 2)];
        text.setAttribute('x', String((fp.x + middle.x) / 2));
        text.setAttribute('y', String((fp.y + middle.y) / 2 - 12));
        text.setAttribute('class', 'fb-process-edge-label');
        text.textContent = from.config.defaultFlowId === e.id ? I18n.t('bpmn.default_path') : (e.condition || '');
        this.svg.appendChild(text);
      }
      if (isLoopBack) {
        const mid = this._bezierMidpoint(fp.x, fp.y, tp.x, tp.y);
        const badge = document.createElementNS(svgNs, 'g');
        badge.setAttribute('class', 'fb-edge-loop-badge');
        badge.setAttribute('transform', `translate(${mid.x}, ${mid.y})`);
        const bg = document.createElementNS(svgNs, 'rect');
        bg.setAttribute('x', '-20'); bg.setAttribute('y', '-9');
        bg.setAttribute('width', '40'); bg.setAttribute('height', '18');
        bg.setAttribute('rx', '9');
        bg.setAttribute('class', 'fb-edge-loop-bg');
        const txt = document.createElementNS(svgNs, 'text');
        txt.setAttribute('text-anchor', 'middle');
        txt.setAttribute('dy', '4');
        txt.setAttribute('class', 'fb-edge-loop-text');
        txt.textContent = 'loop';
        badge.appendChild(bg);
        badge.appendChild(txt);
        this.svg.appendChild(badge);
      }
      // Przepływ energii: nakładka path z animowanym stroke-dashoffset (CSS).
      // Pokazuje kierunek from→to. Czysty CSS — znika z innerHTML='' bez duchów
      // (w przeciwieństwie do starego SVG animateMotion).
      const flow = document.createElementNS(svgNs, 'path');
      flow.setAttribute('class', isSelected ? 'fb-edge-flow selected' : 'fb-edge-flow');
      flow.setAttribute('d', d);
      this.svg.appendChild(flow);
      // Przycisk delete (X) na srodku krawedzi gdy selected — kazda
      // krawedz ma takze hover-interaktywny target przez .fb-edge-hit:hover
      // w CSS, ale realny przycisk jest renderowany dopiero po selekcji
      // (po pierwszym kliku w edge). Klik w X usuwa krawedz.
      if (isSelected && this.mode !== 'bpmn') {
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
    if (this.mode === 'bpmn') {
      const body = processBody(this.processModel, this.processPath, this.selectedProcessId);
      const modelingEdges = [...(body.diagram.modelingEdges || []),
        ...(!this.processPath.length ? this.processModel.collaboration?.diagram.modelingEdges || [] : [])];
      for (const edge of modelingEdges) {
        const line = document.createElementNS(svgNs, 'polyline');
        line.setAttribute('points', edge.waypoints.map((point) => `${point.x},${point.y}`).join(' '));
        line.setAttribute('fill', 'none');
        line.setAttribute('stroke', 'var(--color-border, #64748b)');
        line.setAttribute('stroke-width', '2');
        line.setAttribute('stroke-dasharray', '6 4');
        line.style.pointerEvents = 'none';
        line.dataset.modelingId = edge.elementId;
        this.svg.appendChild(line);
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
    if (!this._connecting) this._syncEscalationBadges();
  }

  // Whether an escalation handler's path would be refused depends on the flows
  // after it, which change without the handler itself being redrawn, so its
  // badge is checked whenever the edges are.
  _syncEscalationBadges() {
    if (this.mode !== 'bpmn') return;
    for (const node of this.nodes) {
      if (node.type !== 'bpmn_boundary_escalation') continue;
      const element = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(node.id)}"]`);
      const shown = element?.querySelector('.fb-process-warn')?.dataset.warning ?? '';
      if (element && shown !== this._processWarnings(node).join(' ')) this._renderSingleNode(node);
    }
  }

  _processWarnings(node) {
    const warnings = processNodeWarnings(node, this.opts.processOptions?.serviceBlocks);
    if (node.type === 'bpmn_boundary_escalation' && processEscalationPathProblem(this, node.id)) warnings.push('escalation_path');
    return warnings;
  }

  _processWarningText(node, key) {
    if (key !== 'escalation_path') return processWarningText(node, key, this.opts.processOptions?.serviceBlocks);
    const reason = processEscalationPathProblem(this, node.id)?.reason;
    return I18n.t(`bpmn.routing_warning_escalation_path_${ESCALATION_PATH_KEYS.includes(reason) ? reason : 'other'}`, { names: this._nodeName(node) });
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
  _positiveZoom() {
    if (!Number.isFinite(this.view.zoom) || this.view.zoom <= 0) {
      throw new RangeError('Canvas zoom must be positive and finite');
    }
    return this.view.zoom;
  }

  /**
   * Bounding box wszystkich wezlow w world coords, albo `null` dla pustego
   * grafu. Uzywane i przez dopasowanie widoku, i przez ograniczenie panoramy.
   */
  _contentBounds() {
    const body = this.mode === 'bpmn' ? processBody(this.processModel, this.processPath, this.selectedProcessId) : null;
    const modelingShapes = body ? [...(body.diagram.modelingShapes || []),
      ...(!this.processPath.length ? this.processModel.collaboration?.diagram.modelingShapes || [] : [])] : [];
    if (this.nodes.length === 0 && modelingShapes.length === 0) return null;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const n of this.nodes) {
      if (!Number.isFinite(n.x) || !Number.isFinite(n.y)) continue;
      minX = Math.min(minX, n.x);
      minY = Math.min(minY, n.y);
      maxX = Math.max(maxX, n.x + (this.mode === 'bpmn' ? n.width : NODE_WIDTH));
      maxY = Math.max(maxY, n.y + (this.mode === 'bpmn' ? n.height : Math.max(NODE_H_APPROX, this._renderedHeight(n))));
      if (this.mode === 'bpmn') {
        const label = this._placedLabelBox(n);
        if (label) {
          minX = Math.min(minX, n.x + label.left);
          minY = Math.min(minY, n.y + label.top);
          maxX = Math.max(maxX, n.x + label.left + label.width);
          maxY = Math.max(maxY, n.y + label.top + label.height);
        }
      }
    }
    for (const shape of modelingShapes) {
      const label = this.regionsLayer.querySelector(`[data-modeling-id="${CSS.escape(shape.elementId)}"]`)
        || this.nodesLayer.querySelector(`[data-modeling-id="${CSS.escape(shape.elementId)}"]`);
      minX = Math.min(minX, shape.x);
      minY = Math.min(minY, shape.y);
      maxX = Math.max(maxX, shape.x + Math.max(shape.width, label?.scrollWidth || 0));
      maxY = Math.max(maxY, shape.y + Math.max(shape.height, label?.scrollHeight || 0));
    }
    return Number.isFinite(minX + minY + maxX + maxY) ? { minX, minY, maxX, maxY } : null;
  }

  /**
   * Widok nigdy nie stoi tak, ze skrajny blok dotyka krawedzi plotna: przy
   * kazdej osi zostaje CONTENT_MARGIN pikseli marginesu. Graf mieszczacy sie w
   * kadrze jest wiec ZAWSZE caly widoczny — a graf wiekszy niz kadr da sie
   * przesuwac tylko do granicy tresci, nie w pusta przestrzen za nia.
   */
  // What floats over the lower part of the canvas (the minimap) takes that much
  // of its height: the drawing is kept above it, so a name at the right edge is
  // never hidden under the panel.
  _bottomInset(rect) {
    let inset = 0;
    for (const box of this.opts.obstacles?.() ?? []) {
      if (box.top + box.height / 2 > rect.top + rect.height / 2) inset = Math.max(inset, rect.top + rect.height - box.top + 12);
    }
    return Math.min(inset, rect.height / 2);
  }

  _clampView() {
    const b = this._contentBounds();
    if (!b) return;
    const rect = this.root.getBoundingClientRect();
    if (!Number.isFinite(rect.width) || !Number.isFinite(rect.height)
      || rect.width <= 0 || rect.height <= 0) return;
    const inset = this._bottomInset(rect);
    const z = this._positiveZoom();
    const clampAxis = (value, lo, hi) => Math.min(Math.max(value, Math.min(lo, hi)), Math.max(lo, hi));
    this.view.x = clampAxis(
      this.view.x,
      CONTENT_MARGIN - b.minX * z,
      rect.width - CONTENT_MARGIN - b.maxX * z,
    );
    this.view.y = clampAxis(
      this.view.y,
      CONTENT_MARGIN - b.minY * z,
      rect.height - inset - CONTENT_MARGIN - b.maxY * z,
    );
  }

  // A view that is not a finite number would stop the world transform from
  // being written at all, so it never leaves this method.
  _applyView() {
    this._positiveZoom();
    if (!Number.isFinite(this.view.x)) this.view.x = 0;
    if (!Number.isFinite(this.view.y)) this.view.y = 0;
    this._clampView();
    if (!Number.isFinite(this.view.x)) this.view.x = 0;
    if (!Number.isFinite(this.view.y)) this.view.y = 0;
    this.world.style.transform = `translate(${this.view.x}px, ${this.view.y}px) scale(${this.view.zoom})`;
    this.onViewChange(this.view);
  }

  setZoom(zoom, cx, cy) {
    if (!Number.isFinite(zoom)) return;
    const rect = this.root.getBoundingClientRect();
    if (!Number.isFinite(cx)) cx = rect.width / 2;
    if (!Number.isFinite(cy)) cy = rect.height / 2;
    const z = Math.max(0.2, Math.min(3, zoom));
    // Zoom względem punktu (cx,cy) w lokalnych współrz. canvasu
    const currentZoom = this._positiveZoom();
    const worldX = (cx - this.view.x) / currentZoom;
    const worldY = (cy - this.view.y) / currentZoom;
    this.view.x = cx - worldX * z;
    this.view.y = cy - worldY * z;
    this.view.zoom = z;
    this._pendingFitMinZoom = null;
    this._applyView();
  }

  // Puts a point of the drawing at the middle of the canvas (the minimap's pan).
  centerOn(worldX, worldY) {
    if (!Number.isFinite(worldX) || !Number.isFinite(worldY)) return;
    const rect = this.root.getBoundingClientRect();
    const zoom = this._positiveZoom();
    this.view.x = rect.width / 2 - worldX * zoom;
    this.view.y = rect.height / 2 - worldY * zoom;
    this._applyView();
  }

  zoomBy(factor) {
    this.setZoom(this.view.zoom * factor);
  }

  resetZoom() {
    this.view = { x: 0, y: 0, zoom: 1 };
    this._pendingFitMinZoom = null;
    this._applyView();
  }

  /**
   * `minZoom` broni czytelności: przy 15 blokach na telefonie pełne
   * dopasowanie schodzi do ~27%, czyli tekst wielkości 3 px — z bloku zostają
   * gołe wartości bez nazw pól. Widok otwierany automatycznie trzyma więc próg
   * i zamiast centrować urwany środek grafu, kotwiczy się w jego lewym górnym
   * rogu. Przycisk "dopasuj" woła tę metodę bez progu — tam użytkownik prosi
   * wprost o całość.
   */
  fitToContent(minZoom = 0) {
    this._layoutProcessLabels();
    const bounds = this._contentBounds();
    if (!bounds) { this.resetZoom(); return; }
    const { minX, minY, maxX, maxY } = bounds;
    const pad = CONTENT_MARGIN;
    const rect = this.root.getBoundingClientRect();
    const w = maxX - minX + pad * 2;
    const h = maxY - minY + pad * 2;
    if (!Number.isFinite(rect.width) || !Number.isFinite(rect.height)
      || !Number.isFinite(w) || !Number.isFinite(h)
      || rect.width <= 0 || rect.height <= 0 || w <= 0 || h <= 0) {
      this._pendingFitMinZoom = minZoom;
      return;
    }
    const usableHeight = rect.height - this._bottomInset(rect);
    const fit = Math.min(1, Math.min(rect.width / w, usableHeight / h));
    const z = Math.max(fit, minZoom);
    if (!Number.isFinite(z) || z <= 0) {
      this._pendingFitMinZoom = minZoom;
      return;
    }
    this._pendingFitMinZoom = null;
    this.view.zoom = z;
    if (z > fit) {
      this.view.x = pad * z - minX * z;
      this.view.y = pad * z - minY * z;
    } else {
      this.view.x = (rect.width - (maxX + minX) * z) / 2;
      this.view.y = (usableHeight - (maxY + minY) * z) / 2;
    }
    this._applyView();
  }

  _clientToWorld(clientX, clientY) {
    const rect = this.root.getBoundingClientRect();
    const zoom = this._positiveZoom();
    return {
      x: (clientX - rect.left - this.view.x) / zoom,
      y: (clientY - rect.top - this.view.y) / zoom,
    };
  }

  // -------------------------------------------------------------------------
  // Zdarzenia pointer
  // -------------------------------------------------------------------------
  // The node and pan branches cancel the pointerdown default, which is also
  // what would move focus: a name field edited just before would keep it, and
  // Delete / undo would be taken for typing. A field inside the canvas (a node
  // control) keeps its focus.
  _takeFocusFromOutside() {
    let active = document.activeElement;
    while (active?.shadowRoot?.activeElement) active = active.shadowRoot.activeElement;
    if (!active || active === document.body || this.root.contains(active)) return;
    active.blur?.();
    if (!this.root.hasAttribute('tabindex')) this.root.tabIndex = -1;
    this.root.focus({ preventScroll: true });
  }

  _onPointerDown(ev) {
    if (ev.button !== undefined && ev.button !== 0) return;
    this._takeFocusFromOutside();
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
      if (this.readOnly) return;
      const nodeId = portOut.dataset.nodeId;
      const node = this.nodes.find((n) => n.id === nodeId);
      if (node) {
        const pt = this._clientToWorld(ev.clientX, ev.clientY);
        this._connecting = {
          fromNode: node,
          fromPort: portOut.dataset.port || 'full',
          fromType: portOut.dataset.portType || 'any',
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
      if (this.mode === 'bpmn') {
        for (const boundary of this.nodes.filter((candidate) => processBoundaryKind(candidate.type) && candidate.config.attachedToId === id)) {
          const boundaryEl = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(boundary.id)}"]`);
          if (boundaryEl) this._bringToFront(boundaryEl);
        }
      }
      if (this.readOnly) return;
      const origs = new Map();
      for (const sid of this.selectedIds) {
        const nd = this.nodes.find((n) => n.id === sid);
        if (nd) origs.set(sid, { x: nd.x, y: nd.y });
      }
      const dependents = new Map();
      if (this.mode === 'bpmn') {
        for (const boundary of this.nodes) {
          if (!processBoundaryKind(boundary.type) || this.selectedIds.has(boundary.id)) continue;
          if (origs.has(boundary.config.attachedToId)) {
            dependents.set(boundary.id, { x: boundary.x, y: boundary.y, parentId: boundary.config.attachedToId });
          }
        }
      }
      this._draggingNode = {
        ids: [...this.selectedIds],
        startClientX: ev.clientX,
        startClientY: ev.clientY,
        origs,
        dependents,
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
      const zoom = this._positiveZoom();
      const dx = (clientX - this._draggingNode.startClientX) / zoom;
      const dy = (clientY - this._draggingNode.startClientY) / zoom;
      if (!this._draggingNode.moved && Math.hypot(dx, dy) * zoom < DRAG_THRESHOLD) return;
      this._draggingNode.moved = true;
      for (const id of this._draggingNode.ids) {
        const n = this.nodes.find((x) => x.id === id);
        const orig = this._draggingNode.origs.get(id);
        if (!n || !orig) continue;
        n.x = snapGridCoordinate(orig.x + dx);
        n.y = snapGridCoordinate(orig.y + dy);
        if (this.mode === 'bpmn' && processBoundaryKind(n.type) && !this._draggingNode.origs.has(n.config.attachedToId)) {
          this._positionBoundary(n);
        }
        const el = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(id)}"]`);
        if (el) {
          el.style.left = `${n.x}px`;
          el.style.top = `${n.y}px`;
          el.classList.add('dragging');
        }
      }
      for (const [id, original] of this._draggingNode.dependents) {
        const parent = this.nodes.find((node) => node.id === original.parentId);
        const parentOriginal = this._draggingNode.origs.get(original.parentId);
        const boundary = this.nodes.find((node) => node.id === id);
        if (!parent || !parentOriginal || !boundary) continue;
        boundary.x = original.x + parent.x - parentOriginal.x;
        boundary.y = original.y + parent.y - parentOriginal.y;
        const el = this.nodesLayer.querySelector(`[data-node-id="${CSS.escape(id)}"]`);
        if (el) {
          el.style.left = `${boundary.x}px`;
          el.style.top = `${boundary.y}px`;
          el.classList.add('dragging');
        }
      }
      this._renderRegions();
      this._renderEdges();
      return;
    }

    if (this._connecting) {
      const pt = this._clientToWorld(clientX, clientY);
      this._connecting.currentX = pt.x;
      this._connecting.currentY = pt.y;
      this._renderEdges();
      // Hover target port — zielone gdy typy kompatybilne (lustrzana R8),
      // czerwone gdy nie, żeby user widział ZANIM puści, że się nie połączy.
      const under = document.elementFromPoint(clientX, clientY);
      document.querySelectorAll('.fb-port.drop-target, .fb-port.drop-incompatible')
        .forEach((p) => p.classList.remove('drop-target', 'drop-incompatible'));
      const targetPort = under?.closest('.fb-port-in');
      if (targetPort) {
        const toType = targetPort.dataset.portType || 'any';
        const toNodeId = targetPort.dataset.nodeId;
        const sameNode = toNodeId && this._connecting.fromNode
          && toNodeId === this._connecting.fromNode.id;
        const compatible = !sameNode
          && arePortTypesCompatible(this._connecting.fromType || 'any', toType);
        targetPort.classList.add(compatible ? 'drop-target' : 'drop-incompatible');
      }
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
        this._layoutProcessLabels();
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
            this.connectNodes(fromNode.id, toNodeId, fromPort, toPort);
          }
        }
      }
      document.querySelectorAll('.fb-port.drop-target, .fb-port.drop-incompatible')
        .forEach((p) => p.classList.remove('drop-target', 'drop-incompatible'));
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
    // Pobieramy realny element pod kursorem: ev.target moze byc rootem
    // canvasa gdy SVG path z pointer-events: stroke nie zostaje primary
    // target'em w niektorych przegladarkach (np. po zmianach DOM w
    // _renderEdges). elementFromPoint jest authoritative dla kliku.
    const hitTarget = document.elementFromPoint(ev.clientX, ev.clientY) || ev.target;

    // Klik w X-przycisk na srodku selected edge'a → usun krawedz.
    const deleteBtn = hitTarget.closest('.fb-edge-delete');
    if (deleteBtn && !this.readOnly) {
      const edgeId = deleteBtn.dataset.edgeId;
      this.edges = this.edges.filter((e) => e.id !== edgeId);
      this.selectedEdgeId = null;
      this._pushHistory();
      this._renderEdges();
      this.onChange();
      return;
    }
    const hit = hitTarget.closest('.fb-edge-hit');
    if (hit) {
      this.selectEdge(hit.dataset.edgeId);
      return;
    }
    const nodeEl = hitTarget.closest('.fb-node');
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
