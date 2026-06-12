// =============================================================================
// File: sdk-runtime/data-tree-empty-renderer.js
// Description: Renderers for Tree (0x0213) — uses the <tf-tree> web component
//              (nodes/expandedIds/selectedId properties, expand/collapse/select
//              events bridged 1:1 to SDK events) — and EmptyCell (0x0214) as a
//              plain inline placeholder span. Keyboard nav and lazy_load live
//              in <tf-tree>.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/tables.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

const TREE_VARIANTS = new Set(['default', 'compact', 'with_icons']);
const EMPTY_CELL_VARIANTS = new Set(['dash', 'em_dash', 'n_a', 'none', 'loading']);
const NODE_MAX_DEPTH = 32;

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

function validateNode(node, ctx) {
  if (!node || typeof node !== 'object') throw new TypeError(`${ctx}: node must be object`);
  if (typeof node.id !== 'string' || node.id.length === 0) {
    throw new TypeError(`${ctx}.id must be non-empty string`);
  }
  if (node.label != null && typeof node.label !== 'string') {
    throw new TypeError(`${ctx}.label must be string if present`);
  }
  if (node.children != null && !Array.isArray(node.children)) {
    throw new TypeError(`${ctx}.children must be array if present`);
  }
  if (node.disabled != null && typeof node.disabled !== 'boolean') {
    throw new TypeError(`${ctx}.disabled must be boolean if present`);
  }
  if (node.has_children != null && typeof node.has_children !== 'boolean') {
    throw new TypeError(`${ctx}.has_children must be boolean if present`);
  }
}

// =============================================================================
// Tree (0x0213) — uses <tf-tree> web component
// =============================================================================

export const TREE_TAG = 0x0213;
const TREE_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderTree(component, ctx) {
  assertOnlyKnownFields(component.fields, TREE_FIELD_KEYS, 'Tree');

  const nodesPath = requirePath(ctx.readField(component.fields, 0), 'Tree.nodes_path');
  const expandedIdsBind = ctx.readField(component.fields, 1);
  if (expandedIdsBind == null) throw new TypeError('Tree.expanded_ids is required (BindRef)');
  const selectedIdBind = ctx.readField(component.fields, 2);
  const variant = requireEnum(ctx.readField(component.fields, 3), TREE_VARIANTS, 'Tree.variant');
  const lazyLoad = requireBool(ctx.readField(component.fields, 4), 'Tree.lazy_load');

  // Wrapper is the SDK event boundary: <tf-tree> emits generic
  // expand/collapse/select with `{ id }`, the wrapper re-emits the SDK shapes.
  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-tree-wrapper');

  const tfTree = document.createElement('tf-tree');
  tfTree.setAttribute('variant', variant);
  if (lazyLoad) tfTree.setAttribute('lazy', '');
  wrapper.appendChild(tfTree);

  const getExpandedSet = () => {
    const raw = resolveBindRef(expandedIdsBind, ctx.store);
    if (!Array.isArray(raw)) return new Set();
    return new Set(raw.filter((s) => typeof s === 'string'));
  };
  const getSelected = () => {
    if (selectedIdBind == null) return null;
    const v = resolveBindRef(selectedIdBind, ctx.store);
    return typeof v === 'string' ? v : null;
  };

  // Validates the full CBOR node tree and maps it to the <tf-tree> node shape
  // (camelCase keys, icons pre-rendered to DOM nodes).
  function mapNode(node, depth) {
    validateNode(node, `Tree.node[${node.id}]`);
    if (depth > NODE_MAX_DEPTH) {
      throw new TypeError(`Tree: node nesting > ${NODE_MAX_DEPTH} (id=${node.id})`);
    }
    const mapped = {
      id: node.id,
      label: node.label != null ? node.label : undefined,
      disabled: node.disabled === true,
      hasChildren: node.has_children === true,
    };
    if (variant === 'with_icons' && node.icon != null) {
      mapped.icon = renderIcon(node.icon, `Tree.node[${node.id}].icon`);
    }
    if (Array.isArray(node.children)) {
      mapped.children = node.children.map((c) => mapNode(c, depth + 1));
    }
    return mapped;
  }

  const rebuild = () => {
    let nodes;
    try { nodes = ctx.store.read(nodesPath); } catch { nodes = undefined; }
    tfTree.nodes = Array.isArray(nodes) ? nodes.map((n) => mapNode(n, 0)) : [];
    tfTree.expandedIds = getExpandedSet();
    tfTree.selectedId = getSelected();
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(nodesPath, rebuild));
  ctx.registerCleanup(subscribeBindRef(expandedIdsBind, ctx.store, rebuild));
  if (selectedIdBind != null) {
    ctx.registerCleanup(subscribeBindRef(selectedIdBind, ctx.store, rebuild));
  }

  const emit = (kind, detail) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)(kind, { bubbles: false, detail })
    );
  };
  const onExpand = (e) => emit('expand', { node_id: e.detail.id, lazy_load: lazyLoad });
  const onCollapse = (e) => emit('collapse', { node_id: e.detail.id });
  const onSelect = (e) => emit('select', { node_id: e.detail.id });
  tfTree.addEventListener('expand', onExpand);
  tfTree.addEventListener('collapse', onCollapse);
  tfTree.addEventListener('select', onSelect);
  ctx.registerCleanup(() => {
    tfTree.removeEventListener('expand', onExpand);
    tfTree.removeEventListener('collapse', onCollapse);
    tfTree.removeEventListener('select', onSelect);
  });

  return wrapper;
}

// =============================================================================
// EmptyCell (0x0214) — plain inline <span> placeholder (table cell context,
//                       not a full empty-state panel)
// =============================================================================

export const EMPTY_CELL_TAG = 0x0214;
const EMPTY_CELL_FIELD_KEYS = new Set([0]);

function renderEmptyCell(component, ctx) {
  assertOnlyKnownFields(component.fields, EMPTY_CELL_FIELD_KEYS, 'EmptyCell');

  const variant = requireEnum(ctx.readField(component.fields, 0), EMPTY_CELL_VARIANTS, 'EmptyCell.variant');

  // Simple inline variants stay as <span> (EmptyCell is a table cell placeholder,
  // not a full empty-state panel)
  const el = document.createElement('span');
  el.classList.add('tf-empty-cell');
  el.classList.add(`tf-empty-cell--${variant}`);
  switch (variant) {
    case 'dash':    el.textContent = '–'; break;
    case 'em_dash': el.textContent = '—'; break;
    case 'n_a':
      el.textContent = 'N/A';
      el.setAttribute('aria-label', 'Not available');
      break;
    case 'none':
      el.setAttribute('aria-hidden', 'true');
      el.textContent = '';
      break;
    case 'loading':
      el.setAttribute('role', 'status');
      el.setAttribute('aria-label', 'Loading');
      el.textContent = '…';
      break;
  }
  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerDataTreeEmptyRenderers() {
  if (!lookupComponentRenderer(TREE_TAG)) registerComponentRenderer(TREE_TAG, renderTree);
  if (!lookupComponentRenderer(EMPTY_CELL_TAG)) registerComponentRenderer(EMPTY_CELL_TAG, renderEmptyCell);
}
