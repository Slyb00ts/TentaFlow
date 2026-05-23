// =============================================================================
// Plik: sdk-runtime/data-tree-empty-renderer.js
// Opis: Renderery §4 Data Display tree+empty — chunk 3.3d-4:
//   - Tree      (0x0213) — hierarchical drzewo z lazy_load, keyboard nav,
//                          expand/collapse/select events
//   - EmptyCell (0x0214) — placeholder dla nullish wartości w komórkach
//                          tabel/list (5 wariantów: dash/em_dash/n_a/none/loading)
//
// Tree expected node shape w store (per spec):
//   { id, label, children?, icon?, disabled?, has_children? (dla lazy_load) }
// expanded_ids: BindRef → Array<string>. selected_id: BindRef → string|null.
// lazy_load=true: emit `expand` z node_id, host dosyłuje children przez patch
// pod node.children.
//
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
// Tree node id grammar: każdy non-empty string (id pochodzi z addona, brak
// gramatyki ograniczającej w spec).
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

/// Waliduje pojedynczy node ze store. Akceptuje obiekt z `id` (string), opcjonalnie
/// `label` (string), `children` (array), `icon` (IconRef shape), `disabled`,
/// `has_children` (boolean — używane przy lazy_load żeby pokazać caret zanim
/// dzieci są załadowane).
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
// Tree (0x0213)
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-tree');
  wrapper.classList.add(`tf-tree--variant-${variant}`);
  wrapper.setAttribute('role', 'tree');
  if (lazyLoad) wrapper.classList.add('tf-tree--lazy');

  // Cache mapy id → element żeby keyboard nav mógł znaleźć aktualny węzeł.
  let nodeElements = new Map();
  let flatVisible = [];  // lista node id w kolejności DOM (do nav)
  // Per-rebuild cleanups (row listeners). Czyszczone PRZED każdym kolejnym
  // rebuildem żeby nie hold'ować listenerów na usuniętych DOM node'ach.
  // ctx.registerCleanup w destroy uruchomi finalne czyszczenie też.
  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

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

  const emit = (kind, detail) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)(kind, { bubbles: false, detail })
    );
  };

  /// Renderuje pojedynczy node + rekurencyjnie children (jeśli expanded).
  function renderNode(node, depth, parentIds) {
    validateNode(node, `Tree.node[${node.id}]`);
    if (depth > NODE_MAX_DEPTH) {
      throw new TypeError(`Tree: node nesting > ${NODE_MAX_DEPTH} (id=${node.id})`);
    }
    const li = document.createElement('li');
    li.classList.add('tf-tree__node');
    li.setAttribute('role', 'treeitem');
    li.setAttribute('data-node-id', node.id);
    li.setAttribute('data-depth', String(depth));
    if (node.disabled === true) {
      li.classList.add('tf-tree__node--disabled');
      li.setAttribute('aria-disabled', 'true');
    }
    nodeElements.set(node.id, li);

    const row = document.createElement('div');
    row.classList.add('tf-tree__row');
    row.style.paddingLeft = `${depth * 1.25}em`;
    row.setAttribute('tabindex', '-1');

    const expandedSet = getExpandedSet();
    const isExpanded = expandedSet.has(node.id);
    const hasChildren = (Array.isArray(node.children) && node.children.length > 0)
      || (lazyLoad && node.has_children === true);

    // Caret indicator dla expandable nodes.
    const caret = document.createElement('span');
    caret.classList.add('tf-tree__caret');
    caret.setAttribute('aria-hidden', 'true');
    if (hasChildren) {
      caret.textContent = isExpanded ? '▾' : '▸';
      caret.classList.add('tf-tree__caret--clickable');
    } else {
      caret.textContent = ' ';
      caret.classList.add('tf-tree__caret--empty');
    }
    row.appendChild(caret);

    // Icon (variant=with_icons).
    if (variant === 'with_icons' && node.icon != null) {
      const ic = renderIcon(node.icon, `Tree.node[${node.id}].icon`);
      ic.classList.add('tf-tree__icon');
      row.appendChild(ic);
    }

    const label = document.createElement('span');
    label.classList.add('tf-tree__label');
    label.textContent = node.label != null ? node.label : node.id;
    row.appendChild(label);

    // Selected state.
    const selectedId = getSelected();
    if (selectedId === node.id) {
      li.classList.add('tf-tree__node--selected');
      li.setAttribute('aria-selected', 'true');
    }
    if (hasChildren) {
      li.setAttribute('aria-expanded', isExpanded ? 'true' : 'false');
    }

    // Row click — select node + caret area handles expand/collapse.
    const onCaretClick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (!hasChildren) return;
      if (node.disabled === true) return;
      if (isExpanded) {
        emit('collapse', { node_id: node.id });
      } else {
        emit('expand', { node_id: node.id, lazy_load: lazyLoad });
      }
    };
    const onRowClick = (e) => {
      e.preventDefault();
      if (node.disabled === true) return;
      // Klik na caret → toggle expand; klik gdzie indziej → select.
      if (e.target === caret || caret.contains(e.target)) {
        onCaretClick(e);
        return;
      }
      emit('select', { node_id: node.id });
    };
    row.addEventListener('click', onRowClick);
    rebuildCleanups.push(() => row.removeEventListener('click', onRowClick));

    li.appendChild(row);
    flatVisible.push({ id: node.id, depth, hasChildren, expanded: isExpanded, disabled: node.disabled === true });

    if (hasChildren && isExpanded && Array.isArray(node.children) && node.children.length > 0) {
      const childList = document.createElement('ul');
      childList.classList.add('tf-tree__children');
      childList.setAttribute('role', 'group');
      const nextPath = [...parentIds, node.id];
      for (const child of node.children) {
        childList.appendChild(renderNode(child, depth + 1, nextPath));
      }
      li.appendChild(childList);
    }
    return li;
  }

  // Root list.
  const rootList = document.createElement('ul');
  rootList.classList.add('tf-tree__root');
  rootList.setAttribute('role', 'group');
  wrapper.appendChild(rootList);

  // Render — full rebuild przy każdej zmianie nodes/expanded/selected.
  // Tree state'y są zwykle nieduże, więc rebuild jest tani; pamiętamy
  // focus offset żeby przywrócić aktywny node po patch'u.
  let focusedId = null;
  const captureFocus = () => {
    const active = document.activeElement;
    if (!active) return;
    // Scope check — focused row musi być WEWNĄTRZ tego wrappera. Inaczej
    // (np. inna instancja Tree z tym samym node_id) rebuild ukrad'by jej
    // focus.
    if (!wrapper.contains(active)) return;
    const row = active.closest('.tf-tree__row');
    if (!row || !wrapper.contains(row)) return;
    const li = row.parentElement;
    if (li && li.hasAttribute('data-node-id')) {
      focusedId = li.getAttribute('data-node-id');
    }
  };
  const restoreFocus = () => {
    if (focusedId == null) return;
    const li = nodeElements.get(focusedId);
    if (!li) return;
    const row = li.querySelector('.tf-tree__row');
    if (row) try { row.focus(); } catch {}
  };

  const rebuild = () => {
    captureFocus();
    runRebuildCleanups();
    rootList.replaceChildren();
    nodeElements = new Map();
    flatVisible = [];
    let nodes;
    try { nodes = ctx.store.read(nodesPath); } catch { nodes = undefined; }
    if (!Array.isArray(nodes)) return;
    for (const n of nodes) {
      rootList.appendChild(renderNode(n, 0, []));
    }
    restoreFocus();
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(nodesPath, rebuild));
  ctx.registerCleanup(subscribeBindRef(expandedIdsBind, ctx.store, rebuild));
  if (selectedIdBind != null) {
    ctx.registerCleanup(subscribeBindRef(selectedIdBind, ctx.store, rebuild));
  }

  // Keyboard nav na root wrapper — używamy aria-activedescendant byłoby
  // skomplikowane przy rebuild, więc trzymamy focus na <row> i Up/Down
  // przechodzą po flatVisible.
  const focusNode = (id) => {
    const li = nodeElements.get(id);
    if (!li) return;
    const row = li.querySelector('.tf-tree__row');
    if (!row) return;
    row.setAttribute('tabindex', '0');
    // Pozostali tracą tabindex=0.
    for (const el of wrapper.querySelectorAll('.tf-tree__row')) {
      if (el !== row) el.setAttribute('tabindex', '-1');
    }
    try { row.focus(); } catch {}
  };

  const onKey = (e) => {
    const active = document.activeElement;
    const activeRow = active && active.closest && active.closest('.tf-tree__row');
    if (!activeRow || !wrapper.contains(activeRow)) return;
    const activeLi = activeRow.parentElement;
    const id = activeLi && activeLi.getAttribute('data-node-id');
    if (id == null) return;
    const idx = flatVisible.findIndex((n) => n.id === id);
    const cur = flatVisible[idx];
    if (!cur) return;
    switch (e.key) {
      case 'ArrowDown': {
        e.preventDefault();
        if (idx + 1 < flatVisible.length) focusNode(flatVisible[idx + 1].id);
        return;
      }
      case 'ArrowUp': {
        e.preventDefault();
        if (idx > 0) focusNode(flatVisible[idx - 1].id);
        return;
      }
      case 'ArrowRight': {
        if (!cur.hasChildren) return;
        e.preventDefault();
        if (cur.disabled) return;
        if (!cur.expanded) emit('expand', { node_id: cur.id, lazy_load: lazyLoad });
        else if (idx + 1 < flatVisible.length && flatVisible[idx + 1].depth > cur.depth) {
          focusNode(flatVisible[idx + 1].id);
        }
        return;
      }
      case 'ArrowLeft': {
        e.preventDefault();
        if (cur.expanded) {
          if (cur.disabled) return;
          emit('collapse', { node_id: cur.id });
        } else if (cur.depth > 0) {
          // Find parent — focus nav nie wymaga disabled check (selection
          // sam w sobie nie wykonuje akcji na node'ze).
          for (let i = idx - 1; i >= 0; i--) {
            if (flatVisible[i].depth < cur.depth) { focusNode(flatVisible[i].id); break; }
          }
        }
        return;
      }
      case 'Enter':
      case ' ': {
        e.preventDefault();
        if (cur.disabled) return;
        emit('select', { node_id: cur.id });
        return;
      }
      case 'Home': {
        e.preventDefault();
        if (flatVisible.length > 0) focusNode(flatVisible[0].id);
        return;
      }
      case 'End': {
        e.preventDefault();
        if (flatVisible.length > 0) focusNode(flatVisible[flatVisible.length - 1].id);
        return;
      }
    }
  };
  wrapper.addEventListener('keydown', onKey);
  ctx.registerCleanup(() => wrapper.removeEventListener('keydown', onKey));

  return wrapper;
}

// =============================================================================
// EmptyCell (0x0214)
// =============================================================================

export const EMPTY_CELL_TAG = 0x0214;
const EMPTY_CELL_FIELD_KEYS = new Set([0]);

function renderEmptyCell(component, ctx) {
  assertOnlyKnownFields(component.fields, EMPTY_CELL_FIELD_KEYS, 'EmptyCell');

  const variant = requireEnum(ctx.readField(component.fields, 0), EMPTY_CELL_VARIANTS, 'EmptyCell.variant');

  const el = document.createElement('span');
  el.classList.add('tf-empty-cell');
  el.classList.add(`tf-empty-cell--${variant}`);
  // Wszystkie warianty oprócz `none` mają widoczny placeholder; `none` jest
  // celowo blank (aria-hidden). `loading` ma animowany spinner via CSS.
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
      // CSS spinner; tutaj tylko sr-only text.
      el.textContent = '…';
      break;
  }
  return el;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataTreeEmptyRenderers() {
  if (!lookupComponentRenderer(TREE_TAG)) registerComponentRenderer(TREE_TAG, renderTree);
  if (!lookupComponentRenderer(EMPTY_CELL_TAG)) registerComponentRenderer(EMPTY_CELL_TAG, renderEmptyCell);
}
