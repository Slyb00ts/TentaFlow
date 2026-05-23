// =============================================================================
// Plik: sdk-runtime/data-diff-dl-json-renderer.js
// Opis: Renderery Diff (0x021F), DataDefinitionList (0x0221), JsonViewer
// (0x0222) — chunk 3.3d-14.
//
// Diff: dwa BindRef-y do before/after text przez StatePath. Renderuje
// LCS-based line diff (split/inline/unified variants). word_wrap +
// show_line_numbers konfiguracyjne. language opcjonalny (przekazany jako
// data-attr; bez kolorowania składni — to robi dedykowany syntax-highlight
// addon).
//
// DataDefinitionList: <dl> z parą term/definition per item. Layout
// stacked (term nad definition) lub two_column (term lewa kolumna,
// definition prawa).
//
// JsonViewer: read-only tree explorer. Subscribe na value_path, rebuild
// drzewa przy każdej zmianie. collapsed_depth: nodes <= depth są
// rozwinięte initially. searchable: input filtruje klucze/value (substring
// match), pokazuje matched + ancestors. max_height_px wymusza scroll.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/{markdown.rs,progress.rs}.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  requireEnum, requireBool, requireU8, requireU16,
  requirePath, requireString, assertOnlyKnownFields,
} from './data-chart-shared.js';

// =============================================================================
// Diff (0x021F)
// =============================================================================

export const DIFF_TAG = 0x021F;
const DIFF_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const DIFF_VARIANTS = new Set(['split', 'inline', 'unified']);

/// Klasyczna LCS table dla two-string array diff. Zwraca array operacji:
/// {op: 'equal'|'add'|'del', text}. Każda operacja to pojedynczy line.
const DIFF_MAX_LINES = 5000;
function computeLineDiff(beforeLines, afterLines) {
  const m = beforeLines.length;
  const n = afterLines.length;
  if (m > DIFF_MAX_LINES || n > DIFF_MAX_LINES) return null;
  const dp = Array.from({ length: m + 1 }, () => new Uint32Array(n + 1));
  for (let i = m - 1; i >= 0; i--) {
    for (let j = n - 1; j >= 0; j--) {
      if (beforeLines[i] === afterLines[j]) dp[i][j] = dp[i + 1][j + 1] + 1;
      else dp[i][j] = Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const ops = [];
  let i = 0, j = 0;
  while (i < m && j < n) {
    if (beforeLines[i] === afterLines[j]) {
      ops.push({ op: 'equal', text: beforeLines[i] });
      i++; j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      ops.push({ op: 'del', text: beforeLines[i] }); i++;
    } else {
      ops.push({ op: 'add', text: afterLines[j] }); j++;
    }
  }
  while (i < m) { ops.push({ op: 'del', text: beforeLines[i++] }); }
  while (j < n) { ops.push({ op: 'add', text: afterLines[j++] }); }
  return ops;
}

function readTextFromStore(store, path) {
  let v;
  try { v = store.read(path); } catch { return ''; }
  if (v == null) return '';
  if (typeof v !== 'string') return String(v);
  return v;
}

function renderDiff(component, ctx) {
  assertOnlyKnownFields(component.fields, DIFF_FIELD_KEYS, 'Diff');
  const beforePath = requirePath(ctx.readField(component.fields, 0), 'Diff.before_path');
  const afterPath = requirePath(ctx.readField(component.fields, 1), 'Diff.after_path');
  const variant = requireEnum(ctx.readField(component.fields, 2), DIFF_VARIANTS, 'Diff.variant');
  const language = ctx.readField(component.fields, 3);
  if (language != null) requireString(language, 'Diff.language');
  const wordWrap = requireBool(ctx.readField(component.fields, 4), 'Diff.word_wrap');
  const showLineNumbers = requireBool(ctx.readField(component.fields, 5), 'Diff.show_line_numbers');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-diff');
  wrapper.classList.add(`tf-diff--variant-${variant}`);
  if (wordWrap) wrapper.classList.add('tf-diff--word-wrap');
  if (showLineNumbers) wrapper.classList.add('tf-diff--line-numbers');
  if (language != null) wrapper.setAttribute('data-language', language);
  wrapper.setAttribute('role', 'group');
  wrapper.setAttribute('aria-label', 'Text diff');

  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const rebuild = () => {
    runRebuildCleanups();
    wrapper.replaceChildren();
    const before = readTextFromStore(ctx.store, beforePath);
    const after = readTextFromStore(ctx.store, afterPath);
    const beforeLines = before === '' ? [] : before.split('\n');
    const afterLines = after === '' ? [] : after.split('\n');
    const ops = computeLineDiff(beforeLines, afterLines);
    if (ops == null) {
      const msg = document.createElement('div');
      msg.classList.add('tf-diff__overflow');
      msg.textContent = `Diff too large (>${DIFF_MAX_LINES} lines). Use a dedicated diff tool.`;
      wrapper.appendChild(msg);
      return;
    }

    if (variant === 'split') {
      const grid = document.createElement('div');
      grid.classList.add('tf-diff__split');
      const leftCol = document.createElement('div');
      leftCol.classList.add('tf-diff__col', 'tf-diff__col--before');
      const rightCol = document.createElement('div');
      rightCol.classList.add('tf-diff__col', 'tf-diff__col--after');
      let lnBefore = 0, lnAfter = 0;
      for (const o of ops) {
        if (o.op === 'equal') {
          lnBefore++; lnAfter++;
          appendDiffLine(leftCol, 'equal', lnBefore, o.text, showLineNumbers);
          appendDiffLine(rightCol, 'equal', lnAfter, o.text, showLineNumbers);
        } else if (o.op === 'del') {
          lnBefore++;
          appendDiffLine(leftCol, 'del', lnBefore, o.text, showLineNumbers);
          appendDiffLine(rightCol, 'gap', null, '', showLineNumbers);
        } else {
          lnAfter++;
          appendDiffLine(leftCol, 'gap', null, '', showLineNumbers);
          appendDiffLine(rightCol, 'add', lnAfter, o.text, showLineNumbers);
        }
      }
      grid.appendChild(leftCol);
      grid.appendChild(rightCol);
      wrapper.appendChild(grid);
    } else {
      // inline / unified — różnica głównie wizualna (CSS).
      const list = document.createElement('div');
      list.classList.add('tf-diff__lines');
      let lnBefore = 0, lnAfter = 0;
      for (const o of ops) {
        if (o.op === 'equal') {
          lnBefore++; lnAfter++;
          appendDiffLine(list, 'equal', `${lnBefore}/${lnAfter}`, o.text, showLineNumbers);
        } else if (o.op === 'del') {
          lnBefore++;
          appendDiffLine(list, 'del', `${lnBefore}/–`, o.text, showLineNumbers);
        } else {
          lnAfter++;
          appendDiffLine(list, 'add', `–/${lnAfter}`, o.text, showLineNumbers);
        }
      }
      wrapper.appendChild(list);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(beforePath, rebuild));
  ctx.registerCleanup(ctx.store.subscribe(afterPath, rebuild));
  return wrapper;
}

function appendDiffLine(parent, kind, lineNo, text, showLineNumbers) {
  const row = document.createElement('div');
  row.classList.add('tf-diff__row', `tf-diff__row--${kind}`);
  if (showLineNumbers) {
    const ln = document.createElement('span');
    ln.classList.add('tf-diff__line-no');
    ln.textContent = lineNo == null ? '' : String(lineNo);
    row.appendChild(ln);
  }
  const marker = document.createElement('span');
  marker.classList.add('tf-diff__marker');
  marker.textContent = kind === 'add' ? '+' : kind === 'del' ? '−' : kind === 'gap' ? '' : ' ';
  row.appendChild(marker);
  const content = document.createElement('span');
  content.classList.add('tf-diff__content');
  content.textContent = text;
  row.appendChild(content);
  parent.appendChild(row);
}

// =============================================================================
// DataDefinitionList (0x0221)
// =============================================================================

export const DATA_DEFINITION_LIST_TAG = 0x0221;
const DL_FIELD_KEYS = new Set([0, 1]);
const DL_LAYOUTS = new Set(['stacked', 'two_column']);
const DL_ITEM_KEYS = new Set([0, 1]);

function parseDefItem(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: DefItem must be FieldMap`);
  const seen = new Set();
  let term, definition;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!DL_ITEM_KEYS.has(k)) throw new TypeError(`${ctx}: unknown DefItem key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) { assertBindRef(v, `${ctx}.term`); term = v; }
    else { assertBindRef(v, `${ctx}.definition`); definition = v; }
  }
  if (term == null) throw new TypeError(`${ctx}: term required`);
  if (definition == null) throw new TypeError(`${ctx}: definition required`);
  return { term, definition };
}

function renderDataDefinitionList(component, ctx) {
  assertOnlyKnownFields(component.fields, DL_FIELD_KEYS, 'DataDefinitionList');
  const rawItems = ctx.readField(component.fields, 0);
  if (rawItems === null) throw new TypeError('DataDefinitionList.items: explicit null not allowed');
  const items = rawItems === undefined ? [] : (() => {
    if (!Array.isArray(rawItems)) throw new TypeError('DataDefinitionList.items: expected Array<DefItem>');
    return rawItems.map((it, i) => parseDefItem(it, `DataDefinitionList.items[${i}]`));
  })();
  const layout = requireEnum(ctx.readField(component.fields, 1), DL_LAYOUTS, 'DataDefinitionList.layout');

  const dl = document.createElement('dl');
  dl.classList.add('tf-dl');
  dl.classList.add(`tf-dl--layout-${layout}`);

  for (let i = 0; i < items.length; i++) {
    const { term, definition } = items[i];
    const dt = document.createElement('dt');
    dt.classList.add('tf-dl__term');
    const dd = document.createElement('dd');
    dd.classList.add('tf-dl__definition');
    const applyTerm = () => {
      const v = resolveBindRef(term, ctx.store);
      dt.textContent = v == null ? '' : String(v);
    };
    const applyDef = () => {
      const v = resolveBindRef(definition, ctx.store);
      dd.textContent = v == null ? '' : String(v);
    };
    applyTerm();
    applyDef();
    ctx.registerCleanup(subscribeBindRef(term, ctx.store, applyTerm));
    ctx.registerCleanup(subscribeBindRef(definition, ctx.store, applyDef));
    dl.appendChild(dt);
    dl.appendChild(dd);
  }
  return dl;
}

// =============================================================================
// JsonViewer (0x0222)
// =============================================================================

export const JSON_VIEWER_TAG = 0x0222;
const JSON_VIEWER_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderJsonViewer(component, ctx) {
  assertOnlyKnownFields(component.fields, JSON_VIEWER_FIELD_KEYS, 'JsonViewer');
  const valuePath = requirePath(ctx.readField(component.fields, 0), 'JsonViewer.value_path');
  let collapsedDepth = ctx.readField(component.fields, 1);
  if (collapsedDepth === null) throw new TypeError('JsonViewer.collapsed_depth: explicit null not allowed');
  if (collapsedDepth === undefined) collapsedDepth = 2;
  collapsedDepth = requireU8(collapsedDepth, 'JsonViewer.collapsed_depth');
  const maxHeightPx = requireU16(ctx.readField(component.fields, 2), 'JsonViewer.max_height_px');
  if (maxHeightPx === 0) throw new TypeError('JsonViewer.max_height_px must be > 0');
  const searchable = requireBool(ctx.readField(component.fields, 3), 'JsonViewer.searchable');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-json-viewer');
  wrapper.style.maxHeight = `${maxHeightPx}px`;
  wrapper.setAttribute('role', 'tree');
  wrapper.setAttribute('aria-label', 'JSON viewer');

  let searchInput = null;
  let searchQuery = '';
  if (searchable) {
    const bar = document.createElement('div');
    bar.classList.add('tf-json-viewer__search');
    searchInput = document.createElement('input');
    searchInput.type = 'search';
    searchInput.classList.add('tf-json-viewer__search-input');
    searchInput.placeholder = 'Filter…';
    searchInput.setAttribute('aria-label', 'Filter JSON');
    bar.appendChild(searchInput);
    wrapper.appendChild(bar);
    const onInput = () => { searchQuery = searchInput.value.toLowerCase(); rebuild(); };
    searchInput.addEventListener('input', onInput);
    ctx.registerCleanup(() => searchInput.removeEventListener('input', onInput));
  }

  const treeRoot = document.createElement('div');
  treeRoot.classList.add('tf-json-viewer__tree');
  wrapper.appendChild(treeRoot);

  // Persistent collapsed state per-node-path — kept across rebuilds tak
  // długo jak ścieżka istnieje. Key: ścieżka jako string "a.b.0.c".
  const collapsedState = new Map();

  const rebuild = () => {
    treeRoot.replaceChildren();
    let value;
    try { value = ctx.store.read(valuePath); } catch { value = undefined; }
    if (value === undefined) {
      const empty = document.createElement('div');
      empty.classList.add('tf-json-viewer__empty');
      empty.textContent = '(no data)';
      treeRoot.appendChild(empty);
      return;
    }
    const root = renderJsonNode(value, '', 0, collapsedDepth, collapsedState, searchQuery, rebuild);
    if (root != null) treeRoot.appendChild(root);
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(valuePath, rebuild));
  return wrapper;
}

/// Checks recursively whether any descendant (or self) matches query.
function deepMatch(value, query, keyLabel) {
  if (!query) return true;
  const isArray = Array.isArray(value);
  const isObject = value != null && typeof value === 'object' && !isArray;
  if (!isArray && !isObject) {
    return matchQuery(keyLabel, formatJsonScalar(value), query);
  }
  if (matchQuery(keyLabel, null, query)) return true;
  const entries = isArray ? value.map((v, i) => [String(i), v]) : Object.entries(value);
  for (const [k, v] of entries) {
    if (deepMatch(v, query, k)) return true;
  }
  return false;
}

function renderJsonNode(value, pathStr, depth, collapsedDepth, collapsedState, query, rebuild, keyLabel) {
  // Early-reject entire subtree that has no match when searching.
  if (query && !deepMatch(value, query, keyLabel)) return null;

  const isArray = Array.isArray(value);
  const isObject = value != null && typeof value === 'object' && !isArray;
  const node = document.createElement('div');
  node.classList.add('tf-json-viewer__node');
  node.setAttribute('role', 'treeitem');
  node.setAttribute('data-path', pathStr);

  const header = document.createElement('div');
  header.classList.add('tf-json-viewer__header');
  node.appendChild(header);

  if (keyLabel != null) {
    const keyEl = document.createElement('span');
    keyEl.classList.add('tf-json-viewer__key');
    keyEl.textContent = `${keyLabel}: `;
    header.appendChild(keyEl);
  }

  if (isArray || isObject) {
    const entries = isArray
      ? value.map((v, i) => [String(i), v])
      : Object.entries(value);
    // Auto-expand when query matches a descendant.
    const forceExpand = query && deepMatch(value, query, keyLabel);
    const collapsed = forceExpand ? false
      : collapsedState.has(pathStr)
        ? collapsedState.get(pathStr)
        : depth >= collapsedDepth;
    const toggle = document.createElement('button');
    toggle.type = 'button';
    toggle.classList.add('tf-json-viewer__toggle');
    toggle.setAttribute('aria-expanded', String(!collapsed));
    toggle.textContent = collapsed ? '▶' : '▼';
    toggle.addEventListener('click', () => {
      collapsedState.set(pathStr, !collapsed);
      rebuild();
    });
    header.insertBefore(toggle, header.firstChild);
    const summary = document.createElement('span');
    summary.classList.add('tf-json-viewer__summary');
    summary.textContent = isArray
      ? `Array(${entries.length})`
      : `Object(${entries.length})`;
    header.appendChild(summary);
    if (!collapsed) {
      const children = document.createElement('div');
      children.classList.add('tf-json-viewer__children');
      children.setAttribute('role', 'group');
      for (const [k, v] of entries) {
        const childPath = pathStr === '' ? k : `${pathStr}.${k}`;
        const childNode = renderJsonNode(v, childPath, depth + 1, collapsedDepth, collapsedState, query, rebuild, k);
        if (childNode != null) children.appendChild(childNode);
      }
      node.appendChild(children);
    }
  } else {
    const valueEl = document.createElement('span');
    valueEl.classList.add('tf-json-viewer__value');
    valueEl.classList.add(`tf-json-viewer__value--${typeOfJson(value)}`);
    valueEl.textContent = formatJsonScalar(value);
    header.appendChild(valueEl);
  }
  return node;
}

function typeOfJson(v) {
  if (v === null) return 'null';
  if (typeof v === 'boolean') return 'bool';
  if (typeof v === 'number') return 'number';
  if (typeof v === 'string') return 'string';
  return 'other';
}

function formatJsonScalar(v) {
  if (v === null) return 'null';
  if (typeof v === 'string') return `"${v}"`;
  return String(v);
}

function matchQuery(key, value, query) {
  if (!query) return true;
  const k = key == null ? '' : String(key).toLowerCase();
  const v = value == null ? '' : String(value).toLowerCase();
  return k.includes(query) || v.includes(query);
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataDiffDlJsonRenderers() {
  if (!lookupComponentRenderer(DIFF_TAG)) registerComponentRenderer(DIFF_TAG, renderDiff);
  if (!lookupComponentRenderer(DATA_DEFINITION_LIST_TAG)) registerComponentRenderer(DATA_DEFINITION_LIST_TAG, renderDataDefinitionList);
  if (!lookupComponentRenderer(JSON_VIEWER_TAG)) registerComponentRenderer(JSON_VIEWER_TAG, renderJsonViewer);
}
