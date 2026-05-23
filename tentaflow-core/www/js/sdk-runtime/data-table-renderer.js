// =============================================================================
// Plik: sdk-runtime/data-table-renderer.js
// Opis: Renderer Table (0x0211) — chunk 3.3d-6. Najbogatszy komponent Data
// Display (18 pól): columns z TableColumn, rows_path, sortable header click,
// selectable single/multi z checkboxes, pagination (slice rows + nav),
// sticky_header, sticky_columns, empty_state, row_actions per-row menu,
// bulk_actions toolbar widoczny gdy zaznaczone, row_expandable +
// expanded_row_template_id, ColumnRender hints (text/number/currency/badge/
// chip/tag/avatar/avatar_group/icon/stat/trend/progress/rating/actions/
// link/custom).
//
// Rows: store value = Array<object>. Każdy wiersz musi mieć `row_key_field`
// (string id). Brak → throw przy renderze tego wiersza. ColumnRender hints
// to wskazówki dla custom rendering host'a; renderer w tej iteracji robi
// fallback text przez field_path lookup + ValueFormat. Pełne rendering
// per-render-hint (np. ColumnRender::Badge → tf-badge) jest deferred do
// 3.3d-7 (gdy host registry templates jest na miejscu).
//
// Eventy per spec: row_click, row_double_click, selection_change.
// Sort header click → emit `sort_change` z column_id (sort_by BindRef
// określa write-back target przez chunk 3.6).
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
  'badge', 'chip', 'tag', 'avatar', 'avatar_group', 'icon',
  'stat', 'trend', 'progress', 'rating', 'actions', 'link', 'custom',
]);
const COLUMN_WIDTH_KINDS = new Set(['auto', 'min_content', 'max_content', 'px', 'fr']);
const VALUE_FORMAT_KINDS = new Set([
  'number', 'currency', 'percent', 'bytes', 'duration',
  'date', 'time', 'datetime', 'relative', 'plain',
]);
const ID_RE = /^[a-z0-9_-]{1,64}$/;
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
// Per-variant ValueFormat allowed keys (per spec value_format.rs).
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
  // Per-variant unknown-key rejection (mirror Rust decoder strictness
  // z value_format.rs).
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
      if (!Number.isInteger(raw.value) || raw.value < 1 || raw.value > max) {
        throw new TypeError(`${ctx}.value must be ${raw.kind === 'px' ? 'u32 >= 1' : 'u8 >= 1'}`);
      }
      return { kind: raw.kind, value: raw.value };
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
        // Per-segment shape validation (mirror StatePath struktura).
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
          if (s.kind === 'index' && !Number.isInteger(s.value)) {
            throw new TypeError(`${ctx}.field_path[${si}].value must be integer for kind=index`);
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

/// Walidacja ComponentRef shape (minimum dla overflow entries).
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

/// Reads nested field z row object przez field_path segments (rooted at row).
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

/// Format value przez ColumnRender hint i optional ValueFormat. Wraca string
/// gotowy do textContent (XSS-safe).
function formatCellValue(value, render, format, locale) {
  if (value == null) return '';
  if (format != null) {
    try { return formatValue(value, format, locale); }
    catch { /* fall through */ }
  }
  // ColumnRender hints fallback do tekstu — pełne komponenty per-render
  // (badge, chip, etc.) deferred do 3.3d-7 host template registry.
  switch (render) {
    case 'number':
    case 'percent':
    case 'bytes':
      try {
        return new Intl.NumberFormat(locale).format(typeof value === 'bigint' ? Number(value) : value);
      } catch { return String(value); }
    case 'currency': return String(value);  // wymaga ValueFormat dla code
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

// =============================================================================
// Table (0x0211)
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
  // Duplicate column id detection.
  const colIds = new Set();
  for (const col of columns) {
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-table');
  wrapper.classList.add(`tf-table--variant-${variant}`);
  wrapper.classList.add(`tf-table--density-${density}`);
  if (stickyHeader) wrapper.classList.add('tf-table--sticky-header');
  if (virtualize) wrapper.classList.add('tf-table--virtualize');

  // Bulk actions toolbar — widoczny gdy zaznaczone rows.
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

  const scroll = document.createElement('div');
  scroll.classList.add('tf-table__scroll');
  wrapper.appendChild(scroll);

  const tableEl = document.createElement('table');
  tableEl.classList.add('tf-table__table');
  scroll.appendChild(tableEl);

  // Header rebuild zostaje stabilny (zmieniaja sie tylko sort indicators);
  // tbody full rebuild przy patch'u.
  const thead = document.createElement('thead');
  if (stickyHeader) thead.classList.add('tf-table__thead--sticky');
  const headerRow = document.createElement('tr');
  // Selection checkbox column header.
  if (selectMode !== 'none') {
    const selTh = document.createElement('th');
    selTh.classList.add('tf-table__th-select');
    if (selectMode === 'multi') {
      const cb = document.createElement('input');
      cb.setAttribute('type', 'checkbox');
      cb.classList.add('tf-table__select-all');
      cb.setAttribute('aria-label', 'Select all');
      selTh.appendChild(cb);
      const onSelectAll = (e) => {
        e.stopPropagation();
        const rows = readVisibleRows();
        const allIds = rows.map((r) => extractRowId(r));
        const next = cb.checked ? allIds : [];
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('selection_change', {
            bubbles: false,
            detail: { selected_ids: next, mode: 'multi', all: cb.checked },
          })
        );
      };
      cb.addEventListener('change', onSelectAll);
      ctx.registerCleanup(() => cb.removeEventListener('change', onSelectAll));
    }
    headerRow.appendChild(selTh);
  }
  for (let i = 0; i < columns.length; i++) {
    const col = columns[i];
    if (col.hidden_by_default) continue;
    const th = document.createElement('th');
    th.classList.add('tf-table__th');
    th.setAttribute('data-column-id', col.id);
    if (col.align) th.classList.add(`tf-table__th--align-${col.align}`);
    if (col.sticky_left || i < stickyColumns) th.classList.add('tf-table__th--sticky-left');
    applyColumnWidth(th, col.width);
    const headerText = document.createElement('span');
    headerText.classList.add('tf-table__th-label');
    applyTextBind(headerText, col.header, ctx);
    th.appendChild(headerText);
    if (sortable && col.sortable) {
      th.classList.add('tf-table__th--sortable');
      th.setAttribute('role', 'button');
      th.setAttribute('tabindex', '0');
      const sortIndicator = document.createElement('span');
      sortIndicator.classList.add('tf-table__th-sort');
      sortIndicator.setAttribute('aria-hidden', 'true');
      th.appendChild(sortIndicator);
      const updateSort = () => {
        let dir = null, current = null;
        if (sortByBind != null) {
          const v = resolveBindRef(sortByBind, ctx.store);
          if (v && typeof v === 'object' && v.column_id === col.id) {
            current = v;
            dir = SORT_DIRECTIONS.has(v.direction) ? v.direction : null;
          }
        }
        sortIndicator.textContent = dir === 'asc' ? '▲' : dir === 'desc' ? '▼' : '↕';
        th.setAttribute('aria-sort', dir === 'asc' ? 'ascending' : dir === 'desc' ? 'descending' : 'none');
      };
      updateSort();
      if (sortByBind != null) {
        ctx.registerCleanup(subscribeBindRef(sortByBind, ctx.store, updateSort));
      }
      const onSortClick = (e) => {
        e.preventDefault();
        // Toggle direction: asc → desc → none.
        let next = { column_id: col.id, direction: 'asc' };
        if (sortByBind != null) {
          const v = resolveBindRef(sortByBind, ctx.store);
          if (v && v.column_id === col.id) {
            if (v.direction === 'asc') next = { column_id: col.id, direction: 'desc' };
            else if (v.direction === 'desc') next = null;
          }
        }
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('sort_change', {
            bubbles: false,
            detail: { sort: next },
          })
        );
      };
      const onSortKey = (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); onSortClick(e); }
      };
      th.addEventListener('click', onSortClick);
      th.addEventListener('keydown', onSortKey);
      ctx.registerCleanup(() => {
        th.removeEventListener('click', onSortClick);
        th.removeEventListener('keydown', onSortKey);
      });
    }
    headerRow.appendChild(th);
  }
  // Row actions column header (jeśli row_actions != []).
  if (rowActions.length > 0) {
    const actTh = document.createElement('th');
    actTh.classList.add('tf-table__th-actions');
    actTh.setAttribute('aria-label', 'Actions');
    headerRow.appendChild(actTh);
  }
  thead.appendChild(headerRow);
  tableEl.appendChild(thead);

  const tbody = document.createElement('tbody');
  tbody.classList.add('tf-table__tbody');
  tableEl.appendChild(tbody);

  // Empty state slot.
  let emptyStateEl = null;
  if (emptyStateRaw != null) {
    emptyStateEl = ctx.renderChild(emptyStateRaw);
    emptyStateEl.classList.add('tf-table__empty-state');
    emptyStateEl.hidden = true;
    wrapper.appendChild(emptyStateEl);
  }

  // Pagination footer.
  let paginationEl = null;
  let pageSizeRef = null;
  if (pagination != null) {
    paginationEl = document.createElement('nav');
    paginationEl.classList.add('tf-table__pagination');
    paginationEl.setAttribute('aria-label', 'Pagination');
    pageSizeRef = pagination.page_size;
    wrapper.appendChild(paginationEl);
  }

  // Per-rebuild cleanups (row click listeners).
  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const extractRowId = (row) => {
    if (row == null || typeof row !== 'object') {
      throw new TypeError(`Table.row missing row_key_field '${rowKeyField}'`);
    }
    const id = row[rowKeyField];
    if (typeof id !== 'string') {
      throw new TypeError(`Table.row.${rowKeyField} must be string, got ${typeof id}`);
    }
    return id;
  };

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

  const rebuild = () => {
    runRebuildCleanups();
    tbody.replaceChildren();
    const rows = readVisibleRows();
    if (rows.length === 0) {
      tbody.hidden = true;
      if (emptyStateEl) emptyStateEl.hidden = false;
      if (paginationEl) renderPagination();
      if (bulkToolbar) bulkToolbar.hidden = true;
      return;
    }
    tbody.hidden = false;
    if (emptyStateEl) emptyStateEl.hidden = true;

    const selectedSet = getSelectedSet();
    for (let rIdx = 0; rIdx < rows.length; rIdx++) {
      const row = rows[rIdx];
      const rowId = extractRowId(row);
      const tr = document.createElement('tr');
      tr.classList.add('tf-table__tr');
      tr.setAttribute('data-row-id', rowId);
      if (selectedSet.has(rowId)) tr.classList.add('tf-table__tr--selected');

      // Selection checkbox cell.
      if (selectMode !== 'none') {
        const td = document.createElement('td');
        td.classList.add('tf-table__td-select');
        const cb = document.createElement('input');
        cb.setAttribute('type', selectMode === 'single' ? 'radio' : 'checkbox');
        if (selectMode === 'single') cb.setAttribute('name', `tf-table-sel-${component.id}`);
        cb.setAttribute('aria-label', 'Select row');
        cb.checked = selectedSet.has(rowId);
        const onSelect = (e) => {
          e.stopPropagation();
          let next;
          if (selectMode === 'single') {
            next = cb.checked ? rowId : null;
          } else {
            const cur = Array.from(selectedSet);
            next = cb.checked ? [...cur.filter((i) => i !== rowId), rowId] : cur.filter((i) => i !== rowId);
          }
          wrapper.dispatchEvent(
            new (globalThis.CustomEvent || globalThis.Event)('selection_change', {
              bubbles: false,
              detail: { selected_ids: next, mode: selectMode, changed_row_id: rowId },
            })
          );
        };
        cb.addEventListener('change', onSelect);
        rebuildCleanups.push(() => cb.removeEventListener('change', onSelect));
        td.appendChild(cb);
        tr.appendChild(td);
      }

      // Column cells.
      for (let cIdx = 0; cIdx < columns.length; cIdx++) {
        const col = columns[cIdx];
        if (col.hidden_by_default) continue;
        const td = document.createElement('td');
        td.classList.add('tf-table__td');
        if (col.align) td.classList.add(`tf-table__td--align-${col.align}`);
        if (col.sticky_left || cIdx < stickyColumns) td.classList.add('tf-table__td--sticky-left');
        td.setAttribute('data-column-id', col.id);
        const raw = readRowField(row, col.field_path);
        td.textContent = formatCellValue(raw, col.render, col.format, ctx.locale);
        tr.appendChild(td);
      }

      // Row actions cell.
      if (rowActions.length > 0) {
        const td = document.createElement('td');
        td.classList.add('tf-table__td-actions');
        for (const action of rowActions) {
          // Każda akcja jest re-render'owana per row — host'owy event
          // dispatcher dostaje original Component handlers + addona moze
          // odróżnić target row przez data-row-id na <tr>.
          const btn = ctx.renderChild(action);
          btn.classList.add('tf-table__row-action');
          btn.setAttribute('data-row-id', rowId);
          td.appendChild(btn);
        }
        tr.appendChild(td);
      }

      // Row click/dblclick events.
      const onRowClick = (e) => {
        if (e.target.closest('.tf-table__td-select') || e.target.closest('.tf-table__td-actions')) return;
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('row_click', {
            bubbles: false,
            detail: { row_id: rowId },
          })
        );
      };
      const onRowDblClick = (e) => {
        if (e.target.closest('.tf-table__td-select') || e.target.closest('.tf-table__td-actions')) return;
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('row_double_click', {
            bubbles: false,
            detail: { row_id: rowId },
          })
        );
      };
      tr.addEventListener('click', onRowClick);
      tr.addEventListener('dblclick', onRowDblClick);
      rebuildCleanups.push(() => {
        tr.removeEventListener('click', onRowClick);
        tr.removeEventListener('dblclick', onRowDblClick);
      });

      tbody.appendChild(tr);
    }

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

  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(rowsPath, rebuild));
  if (selectedIdsBind != null) {
    ctx.registerCleanup(subscribeBindRef(selectedIdsBind, ctx.store, rebuild));
  }
  if (pagination != null) {
    ctx.registerCleanup(ctx.store.subscribe(pagination.current_page_path, rebuild));
  }

  // Update select-all checkbox state based na selectedSet. Niezależnie
  // od bulkToolbar — checkbox jest renderowany dla każdego selectMode=multi
  // (header), więc musi sync ze stanem zawsze.
  if (selectMode === 'multi') {
    const updateSelectAll = () => {
      const cb = headerRow.querySelector('.tf-table__select-all');
      if (!cb) return;
      const rows = readVisibleRows();
      if (rows.length === 0) {
        cb.checked = false;
        cb.indeterminate = false;
        return;
      }
      const selectedSet = getSelectedSet();
      const ids = rows.map((r) => extractRowId(r));
      const selCount = ids.filter((i) => selectedSet.has(i)).length;
      cb.checked = selCount === ids.length;
      cb.indeterminate = selCount > 0 && selCount < ids.length;
    };
    updateSelectAll();
    if (selectedIdsBind != null) {
      ctx.registerCleanup(subscribeBindRef(selectedIdsBind, ctx.store, updateSelectAll));
    }
    ctx.registerCleanup(ctx.store.subscribe(rowsPath, updateSelectAll));
  }

  return wrapper;
}

function applyColumnWidth(el, width) {
  switch (width.kind) {
    case 'auto': el.style.width = 'auto'; break;
    case 'min_content': el.style.width = 'min-content'; break;
    case 'max_content': el.style.width = 'max-content'; break;
    case 'px': el.style.width = `${width.value}px`; break;
    case 'fr': el.style.width = `${width.value}fr`; break;
  }
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataTableRenderer() {
  if (!lookupComponentRenderer(TABLE_TAG)) registerComponentRenderer(TABLE_TAG, renderTable);
}
