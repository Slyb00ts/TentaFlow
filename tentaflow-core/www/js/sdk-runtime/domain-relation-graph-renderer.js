// =============================================================================
// Plik: sdk-runtime/domain-relation-graph-renderer.js
// Opis: Renderer RelationGraph (0x0703) — mapper na <tf-relation-graph>.
// Waliduje CBOR FieldMap (nodes_path/edges_path/layout/interactive/max_nodes),
// czyta i sanityzuje dane grafu ze store (kształt GraphNode/GraphEdge z
// inline.rs: node {id,label,node_type,icon?,tone?}, edge {id,source_id,
// target_id,label?,weight?,tone?}; opcjonalny klucz stanu `style:"dashed"`
// rysuje krawędź kreskowaną — podobieństwo semantyczne z mockupu n02) i
// subskrybuje oba StatePath. Eventy komponentu (node_click/edge_click z
// {node_id}/{edge_id} w detail) idą wprost do dispatcher-a przez
// applyEventHandlers — komponent JEST root elementem.
//
// Spec ref: docs/ADDON_UI_COMPONENT_CATALOG_v1.md §0x0703 + inline.rs
// GraphNode/GraphEdge. RelationGraph nie ma typed structu w sdk-spec —
// FieldMap jest walidowany tutaj wg katalogu.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requirePath, assertOnlyKnownFields,
} from './data-chart-shared.js';

export const RELATION_GRAPH_TAG = 0x0703;

const RELATION_GRAPH_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const GRAPH_LAYOUTS = new Set(['force_directed', 'hierarchical', 'radial', 'manual']);

function requireU32(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFFFFFn) throw new TypeError(`${ctx}: expected u32, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFFFFFF) {
    throw new TypeError(`${ctx}: expected u32, got ${v}`);
  }
  return v;
}

// State labels are usually plain strings; addons that mirror the GraphNode
// wire shape may store a BindRef object instead — resolve it via the store.
function readLabel(raw, store) {
  if (typeof raw === 'string') return raw;
  if (raw && typeof raw === 'object' && typeof raw.kind === 'string') {
    const v = resolveBindRef(raw, store);
    return v == null ? '' : String(v);
  }
  return '';
}

function toneOrNull(raw) {
  return typeof raw === 'string' && TONES.has(raw) ? raw : null;
}

function finiteOrNull(raw) {
  if (typeof raw === 'bigint') return Number(raw);
  return typeof raw === 'number' && Number.isFinite(raw) ? raw : null;
}

/// Reads + sanitises graph nodes from the store. Malformed entries (no
/// string id) are skipped — state is live addon data, not validated wire.
export function readGraphNodes(store, path) {
  let arr;
  try { arr = store.read(path); } catch { arr = undefined; }
  if (!Array.isArray(arr)) return [];
  const out = [];
  const seen = new Set();
  for (const n of arr) {
    if (n == null || typeof n !== 'object' || typeof n.id !== 'string' || n.id.length === 0) {
      continue;
    }
    if (seen.has(n.id)) continue;
    seen.add(n.id);
    out.push({
      id: n.id,
      label: readLabel(n.label, store),
      nodeType: typeof n.node_type === 'string' ? n.node_type : '',
      tone: toneOrNull(n.tone),
      icon: n.icon ?? null,
      x: finiteOrNull(n.x),
      y: finiteOrNull(n.y),
    });
  }
  return out;
}

/// Reads + sanitises graph edges from the store (GraphEdge wire shape).
export function readGraphEdges(store, path) {
  let arr;
  try { arr = store.read(path); } catch { arr = undefined; }
  if (!Array.isArray(arr)) return [];
  const out = [];
  const seen = new Set();
  for (const e of arr) {
    if (
      e == null || typeof e !== 'object'
      || typeof e.id !== 'string' || e.id.length === 0
      || typeof e.source_id !== 'string' || typeof e.target_id !== 'string'
    ) {
      continue;
    }
    if (seen.has(e.id)) continue;
    seen.add(e.id);
    out.push({
      id: e.id,
      sourceId: e.source_id,
      targetId: e.target_id,
      label: readLabel(e.label, store),
      weight: finiteOrNull(e.weight),
      tone: toneOrNull(e.tone),
      dashed: e.style === 'dashed',
    });
  }
  return out;
}

function renderRelationGraph(component, ctx) {
  assertOnlyKnownFields(component.fields, RELATION_GRAPH_FIELD_KEYS, 'RelationGraph');

  const nodesPathRaw = ctx.readField(component.fields, 0);
  if (nodesPathRaw == null) throw new TypeError('RelationGraph.nodes_path is required');
  const nodesPath = requirePath(nodesPathRaw, 'RelationGraph.nodes_path');
  const edgesPathRaw = ctx.readField(component.fields, 1);
  if (edgesPathRaw == null) throw new TypeError('RelationGraph.edges_path is required');
  const edgesPath = requirePath(edgesPathRaw, 'RelationGraph.edges_path');
  const layout = requireEnum(
    ctx.readField(component.fields, 2), GRAPH_LAYOUTS, 'RelationGraph.layout'
  );
  const interactive = requireBool(
    ctx.readField(component.fields, 3), 'RelationGraph.interactive'
  );
  const maxNodes = requireU32(
    ctx.readField(component.fields, 4), 'RelationGraph.max_nodes'
  );
  // Field 5 (optional): selected_path — StatePath of the selected node id.
  // Lets the addon drive/clear the highlight (e.g. detail-panel rows or a
  // BFS-depth reset) without re-rendering the canvas.
  const selectedPathRaw = ctx.readField(component.fields, 5);
  const selectedPath = selectedPathRaw == null
    ? null
    : requirePath(selectedPathRaw, 'RelationGraph.selected_path');

  const el = document.createElement('tf-relation-graph');
  el.classList.add('tf-relation-graph');
  el.layout = layout;
  el.interactive = interactive;
  el.maxNodes = maxNodes;
  el.reducedMotion =
    typeof window !== 'undefined' && typeof window.matchMedia === 'function'
      ? window.matchMedia('(prefers-reduced-motion: reduce)').matches
      : false;

  // Selection is only applied when the id exists in the CURRENT node set —
  // a stale id (node filtered out between patches) would otherwise keep the
  // pulse loop running with no visible selection. Re-validated on every
  // nodes update as well, since either path can change first.
  const syncSelected = () => {
    if (!selectedPath) return;
    let v;
    try { v = ctx.store.read(selectedPath); } catch { v = null; }
    const id = typeof v === 'string' && v.length > 0 ? v : null;
    el.selectedNodeId = id && el.nodes.some((n) => n.id === id) ? id : null;
  };
  const syncNodes = () => {
    el.nodes = readGraphNodes(ctx.store, nodesPath);
    syncSelected();
  };
  const syncEdges = () => { el.edges = readGraphEdges(ctx.store, edgesPath); };
  syncNodes();
  syncEdges();
  ctx.registerCleanup(ctx.store.subscribe(nodesPath, syncNodes));
  ctx.registerCleanup(ctx.store.subscribe(edgesPath, syncEdges));
  if (selectedPath) {
    ctx.registerCleanup(ctx.store.subscribe(selectedPath, syncSelected));
  }

  return el;
}

export function registerDomainRelationGraphRenderer() {
  if (!lookupComponentRenderer(RELATION_GRAPH_TAG)) {
    registerComponentRenderer(RELATION_GRAPH_TAG, renderRelationGraph);
  }
}
