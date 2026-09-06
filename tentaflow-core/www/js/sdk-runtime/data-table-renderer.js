// =============================================================================
// Plik: sdk-runtime/data-table-renderer.js
// Opis: Renderer Table (0x0211) — uses <tf-table> + <tf-column> web components.
// Columns mapped to <tf-column> children, data set via .rows property.
// Sort, pagination, selection (incl. select-all), bulk actions, row actions,
// row double-click, sticky columns, expandable rows, empty state — all SDK
// features wired with reactive bindings.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/tables.rs Table +
// inline.rs (TableColumn, TablePagination, TableColumnWidth).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, formatValue } from './bind-resolver.js';

const TABLE_VARIANTS = new Set(['default', 'striped', 'borderless', 'compact']);
const TABLE_SELECT_MODES = new Set(['none', 'single', 'multi']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);
const SORT_DIRECTIONS = new Set(['asc', 'desc']);
const TEXT_ALIGNS = new Set(['start', 'center', 'end', 'justify']);
const COLUMN_RENDERS = new Set([
  'text', 'number', 'currency', 'percent', 'bytes',
  'date', 'time', 'datetime', 'relative',
  'badge', 'chip', 'tag', 'avatar', 'avatar_group', 'image', 'icon',
  'stat', 'trend', 'progress', 'rating', 'actions', 'link', 'custom',
]);
const COLUMN_WIDTH_KINDS = new Set(['auto', 'min_content', 'max_content', 'px', 'fr']);
const VALUE_FORMAT_KINDS = new Set([
  'number', 'currency', 'percent', 'bytes', 'duration',
  'date', 'time', 'datetime', 'relative', 'plain',
]);
const ID_RE = /^[a-z0-9_-]{1,64}$/;
// Reserved flattened-row property carrying the ORIGINAL row key. transformRows
// writes formatted cell values under column ids, so a column whose id equals
// row_key_field would clobber row[rowKeyField] with a display value — every
// row-key consumer reads this property instead.
const ROW_KEY_PROP = '__tfRowKey';
const TEMPLATE_ID_RE = /^[a-z0-9_-]{1,64}$/;
const EMPTY_STATE_TAG = 0x0003;
const BUTTON_TAG = 0x0401;
const COLUMN_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
const PAGINATION_KEYS = new Set([0, 1, 2]);

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
function requireU8(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) throw new TypeError(`${ctx}: expected u8, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
function requireU32(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFFFFFn) throw new TypeError(`${ctx}: expected u32`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFFFFFF) {
    throw new TypeError(`${ctx}: expected u32, got ${v}`);
  }
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
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
const VALUE_FORMAT_VARIANT_KEYS = {
  plain:    new Set(['kind']),
  number:   new Set(['kind', 'decimals', 'thousands_sep']),
  currency: new Set(['kind', 'code']),
  percent:  new Set(['kind', 'decimals']),
  bytes:    new Set(['kind', 'base']),
  duration: new Set(['kind', 'style']),
  date:     new Set(['kind', 'style']),
  time:     new Set(['kind', 'style']),
  datetime: new Set(['kind', 'style']),
  relative: new Set(['kind']),
};
function assertValueFormat(fmt, ctx, locale) {
  if (fmt == null) return;
  if (typeof fmt !== 'object' || Array.isArray(fmt)) {
    throw new TypeError(`${ctx}: ValueFormat must be object`);
  }
  if (typeof fmt.kind !== 'string' || !VALUE_FORMAT_KINDS.has(fmt.kind)) {
    throw new TypeError(`${ctx}: ValueFormat.kind invalid: ${fmt.kind}`);
  }
  const allowed = VALUE_FORMAT_VARIANT_KEYS[fmt.kind];
  for (const k of Object.keys(fmt)) {
    if (!allowed.has(k)) throw new TypeError(`${ctx}: unexpected key '${k}' for kind=${fmt.kind}`);
  }
  try { formatValue(0, fmt, locale); }
  catch (err) {
    throw new TypeError(`${ctx}: invalid ValueFormat — ${err && err.message ? err.message : err}`);
  }
}

function parseColumnWidth(raw, ctx) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new TypeError(`${ctx}: TableColumnWidth must be object`);
  }
  if (typeof raw.kind !== 'string' || !COLUMN_WIDTH_KINDS.has(raw.kind)) {
    throw new TypeError(`${ctx}.kind invalid: ${raw.kind}`);
  }
  switch (raw.kind) {
    case 'auto':
    case 'min_content':
    case 'max_content': {
      for (const k of Object.keys(raw)) if (k !== 'kind') throw new TypeError(`${ctx}: unexpected key '${k}'`);
      return { kind: raw.kind };
    }
    case 'px':
    case 'fr': {
      for (const k of Object.keys(raw)) if (k !== 'kind' && k !== 'value') throw new TypeError(`${ctx}: unexpected key '${k}'`);
      const max = raw.kind === 'px' ? 0xFFFFFFFF : 0xFF;
      let value = raw.value;
      if (typeof value === 'bigint') {
        if (value < 1n || value > BigInt(max)) {
          throw new TypeError(`${ctx}.value must be ${raw.kind === 'px' ? 'u32 >= 1' : 'u8 >= 1'}`);
        }
        value = Number(value);
      } else if (!Number.isInteger(value) || value < 1 || value > max) {
        throw new TypeError(`${ctx}.value must be ${raw.kind === 'px' ? 'u32 >= 1' : 'u8 >= 1'}`);
      }
      return { kind: raw.kind, value };
    }
  }
  throw new TypeError(`${ctx}: unreachable`);
}

function parseColumn(raw, ctx, locale) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: TableColumn must be FieldMap`);
  const seen = new Set();
  const col = { id: null, header: null, field_path: null, width: null, render: null, format: null, align: null, sortable: null, hidden_by_default: null, sticky_left: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!COLUMN_KEYS.has(k)) throw new TypeError(`${ctx}: unknown TableColumn key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: {
        const id = requireString(v, `${ctx}.id`);
        if (!ID_RE.test(id)) throw new TypeError(`${ctx}.id: invalid grammar`);
        col.id = id;
        break;
      }
      case 1: col.header = v; break;
      case 2: {
        const segs = requirePath(v, `${ctx}.field_path`);
        for (let si = 0; si < segs.length; si++) {
          const s = segs[si];
          if (!s || typeof s !== 'object' || Array.isArray(s)) {
            throw new TypeError(`${ctx}.field_path[${si}]: PathSegment must be object`);
          }
          if (s.kind !== 'key' && s.kind !== 'index') {
            throw new TypeError(`${ctx}.field_path[${si}].kind must be key/index, got ${s.kind}`);
          }
          if (s.kind === 'key' && typeof s.value !== 'string') {
            throw new TypeError(`${ctx}.field_path[${si}].value must be string for kind=key`);
          }
          if (s.kind === 'index') {
            if (typeof s.value === 'bigint') {
              s.value = Number(s.value);
            }
            if (!Number.isInteger(s.value)) {
              throw new TypeError(`${ctx}.field_path[${si}].value must be integer for kind=index`);
            }
          }
        }
        col.field_path = segs;
        break;
      }
      case 3: col.width = parseColumnWidth(v, `${ctx}.width`); break;
      case 4: col.render = requireEnum(v, COLUMN_RENDERS, `${ctx}.render`); break;
      case 5: if (v != null) { assertValueFormat(v, `${ctx}.format`, locale); col.format = v; } break;
      case 6: if (v != null) col.align = requireEnum(v, TEXT_ALIGNS, `${ctx}.align`); break;
      case 7: col.sortable = requireBool(v, `${ctx}.sortable`); break;
      case 8: col.hidden_by_default = requireBool(v, `${ctx}.hidden_by_default`); break;
      case 9: col.sticky_left = requireBool(v, `${ctx}.sticky_left`); break;
    }
  }
  if (col.id == null) throw new TypeError(`${ctx}: id required`);
  if (col.header == null) throw new TypeError(`${ctx}: header required`);
  if (col.field_path == null) throw new TypeError(`${ctx}: field_path required`);
  if (col.width == null) throw new TypeError(`${ctx}: width required`);
  if (col.render == null) throw new TypeError(`${ctx}: render required`);
  if (col.sortable == null) throw new TypeError(`${ctx}: sortable required`);
  if (col.hidden_by_default == null) throw new TypeError(`${ctx}: hidden_by_default required`);
  if (col.sticky_left == null) throw new TypeError(`${ctx}: sticky_left required`);
  return col;
}

function parsePagination(raw, ctx) {
  if (raw == null) return null;
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: TablePagination must be FieldMap`);
  const seen = new Set();
  const p = { page_size: null, current_page_path: null, show_size_picker: null };
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!PAGINATION_KEYS.has(k)) throw new TypeError(`${ctx}: unknown TablePagination key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: p.page_size = requireU32(v, `${ctx}.page_size`); break;
      case 1: p.current_page_path = requirePath(v, `${ctx}.current_page_path`); break;
      case 2: p.show_size_picker = requireBool(v, `${ctx}.show_size_picker`); break;
    }
  }
  if (p.page_size == null) throw new TypeError(`${ctx}: page_size required`);
  if (p.page_size === 0) throw new TypeError(`${ctx}: page_size must be > 0`);
  if (p.current_page_path == null) throw new TypeError(`${ctx}: current_page_path required`);
  if (p.show_size_picker == null) throw new TypeError(`${ctx}: show_size_picker required`);
  return p;
}

function assertComponentRef(c, expectedTag, ctxName) {
  if (!c || typeof c !== 'object' || Array.isArray(c)) {
    throw new TypeError(`${ctxName}: Component must be object`);
  }
  if (c.tag !== expectedTag) {
    throw new TypeError(`${ctxName}: expected tag 0x${expectedTag.toString(16)}, got 0x${(c.tag || 0).toString(16)}`);
  }
  if (typeof c.id !== 'string' || c.id.length === 0) {
    throw new TypeError(`${ctxName}.id must be non-empty string`);
  }
  if (!Array.isArray(c.fields)) throw new TypeError(`${ctxName}.fields must be Array`);
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function readRowField(row, segments) {
  let cur = row;
  for (const seg of segments) {
    if (cur == null) return undefined;
    if (typeof cur !== 'object') return undefined;
    if (seg.kind === 'key') cur = cur[seg.value];
    else if (seg.kind === 'index') cur = Array.isArray(cur) ? cur[seg.value] : undefined;
    else return undefined;
  }
  return cur;
}

function formatCellValue(value, render, format, locale) {
  if (value == null) return '';
  if (format != null) {
    try { return formatValue(value, format, locale); }
    catch { /* fall through */ }
  }
  switch (render) {
    case 'number':
    case 'percent':
    case 'bytes':
      try {
        return new Intl.NumberFormat(locale).format(typeof value === 'bigint' ? Number(value) : value);
      } catch { return String(value); }
    case 'currency': return String(value);
    case 'date':
    case 'time':
    case 'datetime':
      try {
        const opts = render === 'date' ? { dateStyle: 'medium' }
          : render === 'time' ? { timeStyle: 'medium' }
          : { dateStyle: 'medium', timeStyle: 'short' };
        return new Intl.DateTimeFormat(locale, opts).format(new Date(value));
      } catch { return String(value); }
    default: return typeof value === 'object' ? JSON.stringify(value) : String(value);
  }
}

/// Read a FieldMap value by u8 key from a decoded Component.fields array.
function readComponentField(fields, key) {
  if (!Array.isArray(fields)) return undefined;
  for (const entry of fields) {
    if (Array.isArray(entry) && entry.length === 2 && entry[0] === key) return entry[1];
  }
  return undefined;
}

/// Extract the first backend (or both) Handler from a Button's handlers map.
/// Row actions are backend-dispatched buttons; a row action without a backend
/// handler is a no-op and is skipped.
function extractBackendHandler(handlers) {
  if (!Array.isArray(handlers)) return null;
  for (const entry of handlers) {
    if (!Array.isArray(entry) || entry.length !== 2) continue;
    const handler = entry[1];
    if (handler && (handler.kind === 'backend' || handler.kind === 'both')) {
      return handler;
    }
  }
  return null;
}

/// Pre-parse row_actions Button components into renderable descriptors:
/// stable item id, reactive label BindRef and the backend handler. Parsing
/// once (not per row) keeps per-row menu construction cheap.
function parseRowActions(rowActions) {
  const descriptors = [];
  for (let i = 0; i < rowActions.length; i++) {
    const btn = rowActions[i];
    const labelBind = readComponentField(btn.fields, 2);
    if (labelBind == null) {
      throw new TypeError(`Table.row_actions[${i}]: Button.label (field 2) required`);
    }
    const handler = extractBackendHandler(btn.handlers);
    if (handler == null) {
      throw new TypeError(`Table.row_actions[${i}]: Button needs a backend/both handler`);
    }
    const iconRaw = readComponentField(btn.fields, 3);
    const iconName = (iconRaw && iconRaw.kind === 'named') ? iconRaw.name : null;
    const destructive = readComponentField(btn.fields, 0) === 'destructive';
    descriptors.push({
      itemId: btn.id || `row-action-${i}`,
      labelBind,
      handler,
      iconName,
      danger: destructive,
    });
  }
  return descriptors;
}

// =============================================================================
// Table (0x0211) — uses <tf-table> + <tf-column> web components
// =============================================================================

export const TABLE_TAG = 0x0211;
const TABLE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]);

function renderTable(component, ctx) {
  assertOnlyKnownFields(component.fields, TABLE_FIELD_KEYS, 'Table');

  const columnsRaw = ctx.readField(component.fields, 0);
  const columns = columnsRaw == null ? [] : (() => {
    if (!Array.isArray(columnsRaw)) throw new TypeError('Table.columns: expected Array<TableColumn>');
    return columnsRaw.map((c, i) => parseColumn(c, `Table.columns[${i}]`, ctx.locale));
  })();
  const colIds = new Set();
  for (const col of columns) {
    // ID_RE already rejects this name (uppercase letters), but the reserved
    // row-key property must never be writable via a column id — defensive.
    if (col.id === ROW_KEY_PROP) throw new TypeError(`Table.columns: id '${ROW_KEY_PROP}' is reserved`);
    if (colIds.has(col.id)) throw new TypeError(`Table.columns: duplicate id '${col.id}'`);
    colIds.add(col.id);
  }
  const rowsPath = requirePath(ctx.readField(component.fields, 1), 'Table.rows_path');
  const rowKeyField = requireString(ctx.readField(component.fields, 2), 'Table.row_key_field');
  if (rowKeyField.length === 0) throw new TypeError('Table.row_key_field must be non-empty');
  const variant = requireEnum(ctx.readField(component.fields, 3), TABLE_VARIANTS, 'Table.variant');
  const density = requireEnum(ctx.readField(component.fields, 4), DENSITIES, 'Table.density');
  const sortable = requireBool(ctx.readField(component.fields, 5), 'Table.sortable');
  const sortByBind = ctx.readField(component.fields, 6);
  const selectMode = requireEnum(ctx.readField(component.fields, 7), TABLE_SELECT_MODES, 'Table.selectable');
  const selectedIdsBind = ctx.readField(component.fields, 8);
  if (selectMode !== 'none' && selectedIdsBind == null) {
    throw new TypeError('Table.selectable != none requires selected_ids BindRef');
  }
  const stickyHeader = requireBool(ctx.readField(component.fields, 9), 'Table.sticky_header');
  const stickyColumns = requireU8(ctx.readField(component.fields, 10), 'Table.sticky_columns');
  if (stickyColumns > columns.length) {
    throw new TypeError(`Table.sticky_columns (${stickyColumns}) > columns.length (${columns.length})`);
  }
  const paginationRaw = ctx.readField(component.fields, 11);
  const pagination = parsePagination(paginationRaw, 'Table.pagination');
  const emptyStateRaw = ctx.readField(component.fields, 12);
  if (emptyStateRaw != null) assertComponentRef(emptyStateRaw, EMPTY_STATE_TAG, 'Table.empty_state');
  const rowActionsRaw = ctx.readField(component.fields, 13);
  const rowActions = rowActionsRaw == null ? [] : (() => {
    if (!Array.isArray(rowActionsRaw)) throw new TypeError('Table.row_actions: expected Array<Component>');
    for (let i = 0; i < rowActionsRaw.length; i++) {
      assertComponentRef(rowActionsRaw[i], BUTTON_TAG, `Table.row_actions[${i}]`);
    }
    return rowActionsRaw;
  })();
  const rowActionDescriptors = rowActions.length > 0 ? parseRowActions(rowActions) : [];
  const bulkActionsRaw = ctx.readField(component.fields, 14);
  const bulkActions = bulkActionsRaw == null ? [] : (() => {
    if (!Array.isArray(bulkActionsRaw)) throw new TypeError('Table.bulk_actions: expected Array<Component>');
    for (let i = 0; i < bulkActionsRaw.length; i++) {
      assertComponentRef(bulkActionsRaw[i], BUTTON_TAG, `Table.bulk_actions[${i}]`);
    }
    return bulkActionsRaw;
  })();
  const virtualize = requireBool(ctx.readField(component.fields, 15), 'Table.virtualize');
  const rowExpandable = requireBool(ctx.readField(component.fields, 16), 'Table.row_expandable');
  const expandedRowTemplateRaw = ctx.readField(component.fields, 17);
  const expandedRowTemplateId = expandedRowTemplateRaw == null
    ? null
    : (() => {
      const t = requireString(expandedRowTemplateRaw, 'Table.expanded_row_template_id');
      if (!TEMPLATE_ID_RE.test(t)) throw new TypeError('Table.expanded_row_template_id: invalid grammar');
      return t;
    })();
  if (rowExpandable && expandedRowTemplateId == null) {
    throw new TypeError('Table.row_expandable=true requires expanded_row_template_id');
  }

  // Create <tf-table> web component
  const tfTable = document.createElement('tf-table');
  if (sortable) tfTable.setAttribute('sortable', '');
  // variant/density must reach the real <table> inside tf-table's shadow root,
  // so they go on the component (which mirrors them onto the shadow table),
  // NOT on the light-DOM shell where they could never style the table.
  tfTable.setAttribute('variant', variant);
  tfTable.setAttribute('density', density);
  // selectable carries the mode so tf-table only shows the select-all header
  // affordance in multi mode (single selection has no "all" semantics).
  if (selectMode !== 'none') tfTable.setAttribute('selectable', selectMode);
  // sticky_columns: pin the first N columns (component-side positioning).
  if (stickyColumns > 0) tfTable.stickyColumns = stickyColumns;

  // Wrapper div for additional SDK features (bulk actions, pagination, empty
  // state). It is NOT `.tf-table` — that class belongs to the real <table>
  // emitted by the tf-table component; reusing it here would override the
  // table's `display: table`. The shell stacks toolbar/table/empty/pagination.
  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-table-shell');
  if (stickyHeader) wrapper.classList.add('tf-table-shell--sticky-header');
  if (virtualize) wrapper.classList.add('tf-table-shell--virtualize');

  // Bulk actions toolbar
  let bulkToolbar = null;
  if (bulkActions.length > 0 && selectMode === 'multi') {
    bulkToolbar = document.createElement('div');
    bulkToolbar.classList.add('tf-table__bulk-actions');
    bulkToolbar.setAttribute('role', 'toolbar');
    bulkToolbar.hidden = true;
    for (const ba of bulkActions) {
      bulkToolbar.appendChild(ctx.renderChild(ba));
    }
    wrapper.appendChild(bulkToolbar);
  }

  // Create <tf-column> children for visible columns
  for (let i = 0; i < columns.length; i++) {
    const col = columns[i];
    if (col.hidden_by_default) continue;
    const tfCol = document.createElement('tf-column');
    // tf-table accepts arbitrary column keys and resolves cells via row[key],
    // so the unique TableColumn id is used as the key (field_path's first
    // segment could collide when two columns read the same row field) and
    // transformRows mirrors that by writing formatted values under col.id.
    // The `sort` event then carries the spec-mandated column_id directly.
    tfCol.setAttribute('key', col.id);
    if (sortable && col.sortable) tfCol.setAttribute('sortable', '');
    // TableColumn.sticky_left pins this individual column (in addition to the
    // table-level sticky_columns prefix count).
    if (col.sticky_left) tfCol.setAttribute('sticky', '');
    if (col.render === 'number' || col.render === 'currency' || col.render === 'percent' || col.render === 'bytes') {
      tfCol.setAttribute('renderer', 'num');
      tfCol.setAttribute('align', 'num');
    } else if (col.render === 'chip' || col.render === 'badge' || col.render === 'tag') {
      tfCol.setAttribute('renderer', 'chip');
    } else if (col.render === 'image') {
      tfCol.setAttribute('renderer', 'img');
    } else {
      tfCol.setAttribute('renderer', 'text');
    }
    // Reactive header label
    applyTextBind(tfCol, col.header, ctx);
    tfCol.setAttribute('label', tfCol.textContent || col.id);
    tfTable.appendChild(tfCol);
  }

  wrapper.appendChild(tfTable);

  // Empty state slot
  let emptyStateEl = null;
  if (emptyStateRaw != null) {
    emptyStateEl = ctx.renderChild(emptyStateRaw);
    emptyStateEl.classList.add('tf-table__empty-state');
    emptyStateEl.hidden = true;
    wrapper.appendChild(emptyStateEl);
  }

  // Pagination footer
  let paginationEl = null;
  let pageSizeRef = null;
  if (pagination != null) {
    paginationEl = document.createElement('nav');
    paginationEl.classList.add('tf-table__pagination');
    paginationEl.setAttribute('aria-label', 'Pagination');
    pageSizeRef = pagination.page_size;
    wrapper.appendChild(paginationEl);
  }

  // Per-rebuild cleanups
  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  // Rows reaching consumers are transformRows output, so the raw key lives
  // under ROW_KEY_PROP (row[rowKeyField] may hold a formatted display value).
  const extractRowId = (row) => {
    const id = row != null && typeof row === 'object' ? row[ROW_KEY_PROP] : undefined;
    if (typeof id !== 'string') {
      throw new TypeError(`Table.row missing row_key_field '${rowKeyField}'`);
    }
    return id;
  };

  // Build the per-row kebab menu for row_actions. Each menu item dispatches
  // its Button's backend handler enriched with the row key, reusing the
  // same eventDispatcher merge path as native handlers
  // (params <- {...handler.params, ...dom_event.detail}).
  const buildRowActionsElement = (row) => {
    if (rowActionDescriptors.length === 0) return null;
    const rowId = extractRowId(row);

    const menu = document.createElement('tf-menu');
    menu.setAttribute('placement', 'bottom-end');

    const trigger = document.createElement('tf-button');
    trigger.setAttribute('variant', 'ghost');
    trigger.setAttribute('size', 'sm');
    // The sprite has no "more-horizontal" symbol, so an icon trigger rendered as
    // an empty (invisible) button — the row actions menu looked absent. Use a
    // literal ellipsis glyph so the trigger is always visible without a sprite.
    trigger.textContent = '⋯';
    trigger.setAttribute('aria-label', 'Akcje wiersza');

    const onTriggerClick = (e) => {
      e.stopPropagation();
      menu.toggle();
    };
    trigger.addEventListener('click', onTriggerClick);

    for (const desc of rowActionDescriptors) {
      const item = document.createElement('tf-menu-item');
      item.setAttribute('action', desc.itemId);
      if (desc.iconName) item.setAttribute('icon', desc.iconName);
      if (desc.danger) item.setAttribute('danger', '');
      const label = resolveBindRef(desc.labelBind, ctx.store);
      // Set the label as an attribute (timing-safe in tf-menu-item) and as
      // textContent (fallback). Without the attribute the menu opened blank.
      item.setAttribute('label', label == null ? '' : String(label));
      item.textContent = label == null ? '' : String(label);

      const onSelect = () => {
        // Do NOT stop tf-menu-select here: it must bubble to <tf-menu> so the
        // menu closes via its own _onSelect. Row-click/selection is already
        // suppressed by tf-table's .tf-table__actions-cell guard, not by this
        // listener. Row key is injected via dom_event.detail so the backend
        // handler's params end up carrying both `row_id` and the concrete key
        // field (e.g. `camera_id`).
        const syntheticEvent = {
          detail: { row_id: rowId, [rowKeyField]: rowId },
        };
        ctx.eventDispatcher.emit({
          addon_id: ctx.store.addon_id,
          panel_id: ctx.store.panel_id,
          panel_epoch: ctx.store.panel_epoch,
          source_id: desc.itemId,
          event_kind: 'click',
          handler: desc.handler,
          dom_event: syntheticEvent,
        });
      };
      item.addEventListener('tf-menu-select', onSelect);
      menu.appendChild(item);
    }

    const wrapper = document.createElement('div');
    wrapper.classList.add('tf-table__row-actions');
    wrapper.appendChild(trigger);
    wrapper.appendChild(menu);
    return wrapper;
  };

  if (rowActionDescriptors.length > 0) {
    tfTable.rowActions = (row) => buildRowActionsElement(row);
  }

  // Expandable rows: tf-table renders the toggle + inserted expansion <tr>;
  // the renderer supplies the expansion region as a slot/child area tagged with
  // the SDK template id + the real row id so the template system can fill it.
  if (rowExpandable) {
    tfTable.expandable = true;
    // Key expansion state by the stable row id so it follows the row across
    // sort/page changes instead of sticking to a visible index.
    tfTable.rowKey = ROW_KEY_PROP;
    tfTable.expandRenderer = (row) => {
      const region = document.createElement('div');
      region.classList.add('tf-table__expanded-region');
      region.setAttribute('data-template-id', expandedRowTemplateId);
      try { region.setAttribute('data-row-id', extractRowId(row)); } catch { /* row key absent */ }
      return region;
    };
  }

  const readAllRows = () => {
    let rows;
    try { rows = ctx.store.read(rowsPath); } catch { rows = undefined; }
    return Array.isArray(rows) ? rows : [];
  };

  const getCurrentPage = () => {
    if (pagination == null) return 1;
    try {
      const v = ctx.store.read(pagination.current_page_path);
      const n = typeof v === 'bigint' ? Number(v) : Number(v);
      return Number.isInteger(n) && n >= 1 ? n : 1;
    } catch { return 1; }
  };

  const readVisibleRows = () => {
    const all = readAllRows();
    if (pagination == null) return all;
    const page = getCurrentPage();
    const start = (page - 1) * pageSizeRef;
    return all.slice(start, start + pageSizeRef);
  };

  const getSelectedSet = () => {
    if (selectedIdsBind == null) return new Set();
    const v = resolveBindRef(selectedIdsBind, ctx.store);
    if (selectMode === 'single') {
      return typeof v === 'string' ? new Set([v]) : new Set();
    }
    return Array.isArray(v) ? new Set(v.filter((s) => typeof s === 'string')) : new Set();
  };

  // Transform SDK rows to flat objects for tf-table .rows property.
  // tf-table reads values by column key from row objects.
  const transformRows = (rows) => {
    return rows.map((row) => {
      const flat = { ...row };
      // Preserve the raw row key BEFORE the column loop; the loop can never
      // overwrite it because ROW_KEY_PROP is rejected as a column id.
      flat[ROW_KEY_PROP] = row != null && typeof row === 'object' ? row[rowKeyField] : undefined;
      // Formatted cell values live under the column id (the tf-column key).
      for (const col of columns) {
        if (col.hidden_by_default) continue;
        const rawVal = readRowField(row, col.field_path);
        const formatted = formatCellValue(rawVal, col.render, col.format, ctx.locale);
        if (col.render === 'chip' || col.render === 'badge' || col.render === 'tag') {
          // A chip cell may carry an explicit tone: when the raw cell value is
          // an object with `status`/`tone` (e.g. { label, status, dot }), honor
          // it so status pills/risk badges render their mockup colors instead of
          // a flat 'info'. Plain scalar cells keep the neutral default.
          if (rawVal && typeof rawVal === 'object' && !Array.isArray(rawVal)
              && (rawVal.status != null || rawVal.tone != null)) {
            flat[col.id] = {
              label: rawVal.label != null ? String(rawVal.label) : formatted,
              status: String(rawVal.status != null ? rawVal.status : rawVal.tone),
              dot: rawVal.dot === true,
            };
          } else {
            flat[col.id] = { label: formatted, status: 'info' };
          }
        } else {
          flat[col.id] = formatted;
        }
      }
      return flat;
    });
  };

  const rebuild = () => {
    runRebuildCleanups();
    const rows = readVisibleRows();
    if (rows.length === 0) {
      tfTable.rows = [];
      if (emptyStateEl) emptyStateEl.hidden = false;
      if (paginationEl) renderPagination();
      if (bulkToolbar) bulkToolbar.hidden = true;
      return;
    }
    if (emptyStateEl) emptyStateEl.hidden = true;

    // Set rows on tf-table
    tfTable.rows = transformRows(rows);

    const selectedSet = getSelectedSet();
    if (bulkToolbar) {
      bulkToolbar.hidden = selectedSet.size === 0;
    }
    if (paginationEl) renderPagination();
  };

  const renderPagination = () => {
    if (paginationEl == null || pagination == null) return;
    paginationEl.replaceChildren();
    const total = readAllRows().length;
    const pages = Math.max(1, Math.ceil(total / pageSizeRef));
    const page = getCurrentPage();

    const prev = document.createElement('button');
    prev.setAttribute('type', 'button');
    prev.classList.add('tf-table__page-btn');
    prev.textContent = '‹';
    prev.disabled = page <= 1;
    const onPrev = () => {
      if (page <= 1) return;
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('page_change', {
          bubbles: false,
          detail: { page: page - 1, page_size: pageSizeRef },
        })
      );
    };
    prev.addEventListener('click', onPrev);
    rebuildCleanups.push(() => prev.removeEventListener('click', onPrev));
    paginationEl.appendChild(prev);

    const info = document.createElement('span');
    info.classList.add('tf-table__page-info');
    info.textContent = `${page} / ${pages}`;
    paginationEl.appendChild(info);

    const next = document.createElement('button');
    next.setAttribute('type', 'button');
    next.classList.add('tf-table__page-btn');
    next.textContent = '›';
    next.disabled = page >= pages;
    const onNext = () => {
      if (page >= pages) return;
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('page_change', {
          bubbles: false,
          detail: { page: page + 1, page_size: pageSizeRef },
        })
      );
    };
    next.addEventListener('click', onNext);
    rebuildCleanups.push(() => next.removeEventListener('click', onNext));
    paginationEl.appendChild(next);

    if (pagination.show_size_picker) {
      const sizeSel = document.createElement('select');
      sizeSel.classList.add('tf-table__page-size');
      sizeSel.setAttribute('aria-label', 'Page size');
      for (const sz of [10, 25, 50, 100]) {
        const opt = document.createElement('option');
        opt.value = String(sz);
        opt.textContent = `${sz} / page`;
        if (sz === pageSizeRef) opt.selected = true;
        sizeSel.appendChild(opt);
      }
      const onSize = () => {
        const newSize = Number.parseInt(sizeSel.value, 10);
        if (!Number.isInteger(newSize) || newSize <= 0) return;
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('page_size_change', {
            bubbles: false,
            detail: { page_size: newSize },
          })
        );
      };
      sizeSel.addEventListener('change', onSize);
      rebuildCleanups.push(() => sizeSel.removeEventListener('change', onSize));
      paginationEl.appendChild(sizeSel);
    }
  };

  // Bridge tf-table events to SDK event protocol
  const onSort = (e) => {
    const { key, dir } = e.detail || {};
    // tf-column keys are column ids (see column construction above), so the
    // emitted key IS the spec column_id; unknown keys are dropped defensively.
    if (!key || !colIds.has(key)) return;
    const sortPayload = { column_id: key, direction: dir === 'desc' ? 'desc' : 'asc' };
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('sort_change', {
        bubbles: false,
        detail: { sort: sortPayload },
      })
    );
  };
  tfTable.addEventListener('sort', onSort);
  ctx.registerCleanup(() => tfTable.removeEventListener('sort', onSort));

  const rowKeyOf = (row, index) =>
    typeof row[ROW_KEY_PROP] === 'string' ? row[ROW_KEY_PROP] : `row-${index}`;
  const emitSelection = (selIds, rowId) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('selection_change', {
        bubbles: false,
        detail: { selected_ids: selIds, mode: selectMode, changed_row_id: rowId },
      })
    );
  };

  const onRowClick = (e) => {
    const { row, index } = e.detail || {};
    if (!row) return;
    const rowId = rowKeyOf(row, index);
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('row_click', {
        bubbles: false,
        detail: { row_id: rowId },
      })
    );
    // tf-table draws no checkbox in single mode, so the row itself is the only
    // selection affordance there; multi mode goes through the checkboxes below.
    if (selectMode !== 'single') return;
    const tr = tfTable.shadowRoot?.querySelectorAll('tbody tr')[index];
    const nowSelected = !e.detail.selected;
    if (tr) tr.classList.toggle('selected', nowSelected);
    emitSelection(nowSelected ? rowId : null, rowId);
  };
  tfTable.addEventListener('row-click', onRowClick);
  ctx.registerCleanup(() => tfTable.removeEventListener('row-click', onRowClick));

  const onRowSelect = (e) => {
    const { row, index, selected } = e.detail || {};
    if (!row || selectMode !== 'multi') return;
    const rowId = rowKeyOf(row, index);
    const cur = Array.from(getSelectedSet()).filter((i) => i !== rowId);
    emitSelection(selected ? [...cur, rowId] : cur, rowId);
  };
  tfTable.addEventListener('row-select', onRowSelect);
  ctx.registerCleanup(() => tfTable.removeEventListener('row-select', onRowSelect));

  // row_double_click → SDK event carrying the REAL row key (ROW_KEY_PROP),
  // mirroring row_click so a column shadowing row_key_field can't clobber it.
  const onRowDblClick = (e) => {
    const { row, index } = e.detail || {};
    if (!row) return;
    const rowId = typeof row[ROW_KEY_PROP] === 'string' ? row[ROW_KEY_PROP] : `row-${index}`;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('row_double_click', {
        bubbles: false,
        detail: { row_id: rowId },
      })
    );
  };
  tfTable.addEventListener('row-dblclick', onRowDblClick);
  ctx.registerCleanup(() => tfTable.removeEventListener('row-dblclick', onRowDblClick));

  // Expandable rows: tf-table toggles locally and reports expand/collapse; the
  // renderer re-emits the spec expand/collapse event with row id + template id.
  if (rowExpandable) {
    const onRowExpand = (e) => {
      const { row, index, expanded } = e.detail || {};
      if (!row) return;
      const rowId = typeof row[ROW_KEY_PROP] === 'string' ? row[ROW_KEY_PROP] : `row-${index}`;
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)(expanded ? 'expand' : 'collapse', {
          bubbles: false,
          detail: { row_id: rowId, template_id: expandedRowTemplateId },
        })
      );
    };
    tfTable.addEventListener('row-expand', onRowExpand);
    ctx.registerCleanup(() => tfTable.removeEventListener('row-expand', onRowExpand));
  }

  // Select-all (multi only): toggle every CURRENTLY VISIBLE row's id, emitting
  // selection_change with the full id list or an empty list.
  if (selectMode === 'multi') {
    const onSelectAll = (e) => {
      const checked = !!(e.detail && e.detail.selected);
      // readVisibleRows() returns RAW SDK rows, so the key lives under
      // rowKeyField (transformRows hasn't run); ROW_KEY_PROP only exists on
      // transformed rows. Read the raw key and keep only string ids.
      const ids = checked
        ? readVisibleRows()
          .map((r) => (r != null && typeof r === 'object' ? r[rowKeyField] : undefined))
          .filter((id) => typeof id === 'string')
        : [];
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('selection_change', {
          bubbles: false,
          detail: { selected_ids: ids, mode: selectMode, select_all: checked },
        })
      );
    };
    tfTable.addEventListener('select-all', onSelectAll);
    ctx.registerCleanup(() => tfTable.removeEventListener('select-all', onSelectAll));
  }

  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(rowsPath, rebuild));
  if (selectedIdsBind != null) {
    ctx.registerCleanup(subscribeBindRef(selectedIdsBind, ctx.store, rebuild));
  }
  if (pagination != null) {
    ctx.registerCleanup(ctx.store.subscribe(pagination.current_page_path, rebuild));
  }

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataTableRenderer() {
  if (!lookupComponentRenderer(TABLE_TAG)) registerComponentRenderer(TABLE_TAG, renderTable);
}
