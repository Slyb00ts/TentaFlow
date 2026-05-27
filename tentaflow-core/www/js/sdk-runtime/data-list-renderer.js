// =============================================================================
// File: sdk-runtime/data-list-renderer.js
// Description: Renderer for Data Display List (0x0212) — uses <tf-list> web
// component. Items come from `items_path` (StatePath -> Array). Each item
// should have at least `id` (string). Click handler emits `item_click` with
// `{ item_id, item_index, action_id? }`.
//
// Empty state: when items.length === 0, renders optional `empty_state`
// ComponentRef<EmptyState> (tag 0x0003) via ctx.renderChild.
// max_visible: truncates visible items + "show more" indicator with count.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/tables.rs List.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';

const DENSITIES = new Set(['compact', 'default', 'comfortable']);
const EMPTY_STATE_TAG = 0x0003;
const TEMPLATE_ID_RE = /^[a-z0-9_-]{1,64}$/;

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
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

function assertEmptyStateRef(c, ctx) {
  if (!c || typeof c !== 'object' || Array.isArray(c)) {
    throw new TypeError(`${ctx}: Component must be object`);
  }
  if (c.tag !== EMPTY_STATE_TAG) {
    throw new TypeError(`${ctx}: expected EmptyState (0x0003), got tag 0x${(c.tag || 0).toString(16)}`);
  }
  if (typeof c.id !== 'string' || c.id.length === 0) {
    throw new TypeError(`${ctx}.id must be non-empty string`);
  }
  if (!Array.isArray(c.fields)) {
    throw new TypeError(`${ctx}.fields must be Array<[u8, Value]>`);
  }
}

function extractItemId(item, index) {
  if (item != null && typeof item === 'object' && typeof item.id === 'string') return item.id;
  return `item-${index}`;
}

/// Transform SDK item data to tf-list item format
function itemToTfListItem(item, index) {
  if (item == null) return { id: `item-${index}`, title: '' };
  if (typeof item === 'string') return { id: `item-${index}`, title: item };
  if (typeof item !== 'object') return { id: `item-${index}`, title: String(item) };
  return {
    id: typeof item.id === 'string' ? item.id : `item-${index}`,
    title: item.label || item.title || item.id || '',
    sub: item.sub || item.subtitle || item.description || '',
    icon: item.icon || undefined,
    severity: item.severity || item.tone || undefined,
    chip: item.chip || undefined,
    chipTone: item.chipTone || item.chip_tone || undefined,
  };
}

// =============================================================================
// List (0x0212) — uses <tf-list> web component
// =============================================================================

export const LIST_TAG = 0x0212;
const LIST_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderList(component, ctx) {
  assertOnlyKnownFields(component.fields, LIST_FIELD_KEYS, 'List');

  const itemsPath = requirePath(ctx.readField(component.fields, 0), 'List.items_path');
  const itemTemplateId = requireString(ctx.readField(component.fields, 1), 'List.item_template_id');
  if (!TEMPLATE_ID_RE.test(itemTemplateId)) {
    throw new TypeError('List.item_template_id: invalid grammar (must match [a-z0-9_-]+ length 1..=64)');
  }
  const divider = requireBool(ctx.readField(component.fields, 2), 'List.divider');
  const density = requireEnum(ctx.readField(component.fields, 3), DENSITIES, 'List.density');
  const virtualize = requireBool(ctx.readField(component.fields, 4), 'List.virtualize');
  const emptyStateRaw = ctx.readField(component.fields, 5);
  if (emptyStateRaw != null) assertEmptyStateRef(emptyStateRaw, 'List.empty_state');
  const maxVisibleRaw = ctx.readField(component.fields, 6);
  const maxVisible = maxVisibleRaw == null ? null : requireU32(maxVisibleRaw, 'List.max_visible');
  if (maxVisible != null && maxVisible === 0) {
    throw new TypeError('List.max_visible must be > 0 if set');
  }

  // Wrapper div for additional SDK features (empty state, attributes)
  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-list-wrapper');
  wrapper.setAttribute('data-template-id', itemTemplateId);

  // Create <tf-list> web component
  const tfList = document.createElement('tf-list');
  if (density === 'compact') tfList.setAttribute('compact', '');
  wrapper.appendChild(tfList);

  // Empty state slot
  let emptyStateEl = null;
  if (emptyStateRaw != null) {
    emptyStateEl = ctx.renderChild(emptyStateRaw);
    emptyStateEl.classList.add('tf-list__empty-state');
    emptyStateEl.hidden = true;
    wrapper.appendChild(emptyStateEl);
  }

  const rebuild = () => {
    let items;
    try { items = ctx.store.read(itemsPath); } catch { items = undefined; }
    if (!Array.isArray(items) || items.length === 0) {
      tfList.items = [];
      if (emptyStateEl) emptyStateEl.hidden = false;
      return;
    }
    if (emptyStateEl) emptyStateEl.hidden = true;

    const total = items.length;
    const visibleCount = maxVisible == null ? total : Math.min(total, maxVisible);
    const visibleItems = items.slice(0, visibleCount);

    // Transform items to tf-list format
    tfList.items = visibleItems.map((item, i) => itemToTfListItem(item, i));
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(itemsPath, rebuild));

  // Bridge tf-list 'item-click' event to SDK 'item_click' event
  const onItemClick = (e) => {
    const { item, index } = e.detail || {};
    if (!item) return;
    const itemId = item.id || `item-${index}`;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('item_click', {
        bubbles: false,
        detail: { item_id: itemId, item_index: index, template_id: itemTemplateId },
      })
    );
  };
  tfList.addEventListener('item-click', onItemClick);
  ctx.registerCleanup(() => tfList.removeEventListener('item-click', onItemClick));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataListRenderer() {
  if (!lookupComponentRenderer(LIST_TAG)) registerComponentRenderer(LIST_TAG, renderList);
}
