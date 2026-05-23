// =============================================================================
// Plik: sdk-runtime/data-list-renderer.js
// Opis: Renderer §4 Data Display List (0x0212) — chunk 3.3d-5.
//
// Items pochodzą z `items_path` (StatePath → Array). Każdy item powinien
// mieć przynajmniej `id` (string). `item_template_id` jest host-side
// templating identifier — renderer eksponuje go przez `data-template-id`
// na każdym <li>, ale samej renderowanej zawartości nie wytwarza
// (host/slot manager z chunka 3.5 podpina pełny template). Bez template'u
// renderer pokazuje fallback (item.label || item.title || item.id) żeby
// lista była widoczna nawet bez host'a.
//
// Empty state: gdy items.length === 0, renderuje opcjonalny `empty_state`
// ComponentRef<EmptyState> (tag 0x0003) przez ctx.renderChild.
// max_visible: truncate widoczne items + "show more" indicator z licznikiem.
//
// Handler per spec: `item_click` z `{ item_id, item_index, action_id? }`.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/tables.rs List.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

const DENSITIES = new Set(['compact', 'default', 'comfortable']);
const EMPTY_STATE_TAG = 0x0003;
const EMPTY_STATE_VARIANTS = new Set(['default', 'compact', 'illustrated']);
const BUTTON_TAG = 0x0401;
// item_template_id grammar: [a-z0-9_-]+ length 1..=64 (mirror inne id grammars).
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

/// Walidacja Component shape dla empty_state (ComponentRef<EmptyState>).
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

/// Tworzy fallback text z item shape — używane gdy host nie zarejestrował
/// template dla `item_template_id`. Preferencja: item.label > item.title >
/// item.id > JSON.stringify(item).
function fallbackItemText(item) {
  if (item == null) return '';
  if (typeof item === 'string') return item;
  if (typeof item !== 'object') return String(item);
  if (typeof item.label === 'string') return item.label;
  if (typeof item.title === 'string') return item.title;
  if (typeof item.id === 'string') return item.id;
  try { return JSON.stringify(item); } catch { return ''; }
}

function extractItemId(item, index) {
  if (item != null && typeof item === 'object' && typeof item.id === 'string') return item.id;
  return `item-${index}`;
}

// =============================================================================
// EmptyState (0x0003) — molecules §2
// =============================================================================
// EmptyState jest §2 molecules group (osobny chunk w przyszlosci), ale List
// trzyma go jako empty_state ref. Rejestrujemy minimalny renderer tutaj —
// gdy molecules chunk powstanie, ta implementacja zostanie zaktualizowana
// w miejscu lub odznaczona/przeniesiona zgodnie z deduplikacja.

export const EMPTY_STATE_COMPONENT_TAG = 0x0003;
const EMPTY_STATE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function renderEmptyState(component, ctx) {
  assertOnlyKnownFields(component.fields, EMPTY_STATE_FIELD_KEYS, 'EmptyState');

  const iconRaw = ctx.readField(component.fields, 0);
  if (iconRaw == null) throw new TypeError('EmptyState.icon is required (IconRef)');
  const heading = ctx.readField(component.fields, 1);
  if (heading == null) throw new TypeError('EmptyState.heading is required (BindRef)');
  const messageBind = ctx.readField(component.fields, 2);
  const primaryActionRaw = ctx.readField(component.fields, 3);
  const secondaryActionRaw = ctx.readField(component.fields, 4);
  const variant = requireEnum(ctx.readField(component.fields, 5), EMPTY_STATE_VARIANTS, 'EmptyState.variant');

  if (primaryActionRaw != null) {
    if (!primaryActionRaw || primaryActionRaw.tag !== BUTTON_TAG) {
      throw new TypeError('EmptyState.primary_action: expected ComponentRef<Button> (0x0401)');
    }
  }
  if (secondaryActionRaw != null) {
    if (!secondaryActionRaw || secondaryActionRaw.tag !== BUTTON_TAG) {
      throw new TypeError('EmptyState.secondary_action: expected ComponentRef<Button> (0x0401)');
    }
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-empty-state');
  wrapper.classList.add(`tf-empty-state--variant-${variant}`);
  wrapper.setAttribute('role', 'status');

  const iconEl = renderIcon(iconRaw, 'EmptyState.icon');
  iconEl.classList.add('tf-empty-state__icon');
  wrapper.appendChild(iconEl);

  const headingEl = document.createElement('h3');
  headingEl.classList.add('tf-empty-state__heading');
  applyTextBind(headingEl, heading, ctx);
  wrapper.appendChild(headingEl);

  if (messageBind != null) {
    const msg = document.createElement('p');
    msg.classList.add('tf-empty-state__message');
    applyTextBind(msg, messageBind, ctx);
    wrapper.appendChild(msg);
  }

  if (primaryActionRaw != null || secondaryActionRaw != null) {
    const actions = document.createElement('div');
    actions.classList.add('tf-empty-state__actions');
    if (primaryActionRaw != null) {
      const btn = ctx.renderChild(primaryActionRaw);
      btn.classList.add('tf-empty-state__action');
      btn.classList.add('tf-empty-state__action--primary');
      actions.appendChild(btn);
    }
    if (secondaryActionRaw != null) {
      const btn = ctx.renderChild(secondaryActionRaw);
      btn.classList.add('tf-empty-state__action');
      btn.classList.add('tf-empty-state__action--secondary');
      actions.appendChild(btn);
    }
    wrapper.appendChild(actions);
  }

  return wrapper;
}

// =============================================================================
// List (0x0212)
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-list');
  wrapper.classList.add(`tf-list--density-${density}`);
  if (divider) wrapper.classList.add('tf-list--divider');
  if (virtualize) wrapper.classList.add('tf-list--virtualize');
  wrapper.setAttribute('data-template-id', itemTemplateId);

  const listEl = document.createElement('ul');
  listEl.classList.add('tf-list__items');
  listEl.setAttribute('role', 'list');
  wrapper.appendChild(listEl);

  // Empty state slot — renderowany conditionally (hidden gdy są items).
  let emptyStateEl = null;
  if (emptyStateRaw != null) {
    emptyStateEl = ctx.renderChild(emptyStateRaw);
    emptyStateEl.classList.add('tf-list__empty-state');
    emptyStateEl.hidden = true;
    wrapper.appendChild(emptyStateEl);
  }

  // Per-rebuild cleanup (per-item click listeners). Pełny rebuild przy
  // każdym store update items_path; stare listenery muszą zniknąć przed
  // nowym renderem żeby uniknąć DOM/listener leak.
  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const rebuild = () => {
    runRebuildCleanups();
    listEl.replaceChildren();
    let items;
    try { items = ctx.store.read(itemsPath); } catch { items = undefined; }
    if (!Array.isArray(items) || items.length === 0) {
      if (emptyStateEl) emptyStateEl.hidden = false;
      listEl.hidden = true;
      return;
    }
    if (emptyStateEl) emptyStateEl.hidden = true;
    listEl.hidden = false;

    const total = items.length;
    const visibleCount = maxVisible == null ? total : Math.min(total, maxVisible);
    for (let i = 0; i < visibleCount; i++) {
      const item = items[i];
      const itemId = extractItemId(item, i);
      const li = document.createElement('li');
      li.classList.add('tf-list__item');
      li.setAttribute('data-item-id', itemId);
      li.setAttribute('data-item-index', String(i));
      li.setAttribute('role', 'listitem');
      li.setAttribute('tabindex', '0');
      // Fallback content — host (chunk 3.5 slot manager) podmieni przez
      // template `item_template_id`. textContent jest XSS-safe.
      const content = document.createElement('span');
      content.classList.add('tf-list__item-content');
      content.textContent = fallbackItemText(item);
      li.appendChild(content);

      const onClick = (e) => {
        e.preventDefault();
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('item_click', {
            bubbles: false,
            detail: { item_id: itemId, item_index: i, template_id: itemTemplateId },
          })
        );
      };
      const onKey = (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onClick(e);
        }
      };
      li.addEventListener('click', onClick);
      li.addEventListener('keydown', onKey);
      rebuildCleanups.push(() => {
        li.removeEventListener('click', onClick);
        li.removeEventListener('keydown', onKey);
      });
      listEl.appendChild(li);
    }
    // "Show more" indicator gdy max_visible truncated.
    if (maxVisible != null && total > maxVisible) {
      const more = document.createElement('li');
      more.classList.add('tf-list__more');
      more.setAttribute('role', 'presentation');
      more.textContent = `+${total - maxVisible} more`;
      listEl.appendChild(more);
    }
  };
  rebuild();
  ctx.registerCleanup(ctx.store.subscribe(itemsPath, rebuild));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerDataListRenderer() {
  if (!lookupComponentRenderer(EMPTY_STATE_COMPONENT_TAG)) {
    registerComponentRenderer(EMPTY_STATE_COMPONENT_TAG, renderEmptyState);
  }
  if (!lookupComponentRenderer(LIST_TAG)) registerComponentRenderer(LIST_TAG, renderList);
}
