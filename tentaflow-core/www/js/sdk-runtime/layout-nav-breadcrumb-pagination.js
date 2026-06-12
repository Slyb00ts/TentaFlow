// =============================================================================
// Plik: sdk-runtime/layout-nav-breadcrumb-pagination.js
// Opis: Renderers for Breadcrumb (0x0110) + Pagination (0x0111).
// Breadcrumb renders through the dashboard <tf-breadcrumb> /
// <tf-breadcrumb-item> web components; Pagination stays hand-rolled (no
// tf-pagination component exists) using shared dashboard classes only.
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/nav.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

const BREADCRUMB_SEPARATORS = new Set(['chevron', 'slash', 'dot']);
const PAGINATION_VARIANTS = new Set(['compact', 'full', 'input']);

// BreadcrumbItem field keys per spec (inline structs decode to [key, value]
// pair arrays): 0 label, 1 icon, 2 action_id, 3 local_action, 4 is_current.
const BREADCRUMB_ITEM_KEYS = new Set([0, 1, 2, 3, 4]);

function readInlineField(pairs, key) {
  if (!Array.isArray(pairs)) return undefined;
  for (const entry of pairs) {
    if (Array.isArray(entry) && entry.length === 2 && entry[0] === key) return entry[1];
  }
  return undefined;
}

function assertOnlyKnownInlineKeys(pairs, allowedKeys, ctx) {
  if (!Array.isArray(pairs)) {
    throw new TypeError(`${ctx}: expected inline struct ([key, value] pairs)`);
  }
  for (const entry of pairs) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError(`${ctx}: malformed inline struct entry`);
    }
    if (!allowedKeys.has(entry[0])) {
      throw new TypeError(`${ctx}: unexpected key '${entry[0]}'`);
    }
  }
}

function requireEnum(value, set, ctx) {
  if (typeof value !== 'string' || !set.has(value)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(value)}`
    );
  }
  return value;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') {
    throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  }
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') {
    throw new TypeError(`${ctx}: expected string, got ${typeof v}`);
  }
  return v;
}
function requireU8(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) {
      throw new TypeError(`${ctx}: expected u8, got ${v}`);
    }
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) {
    throw new TypeError(`${ctx}: expected u8, got ${v}`);
  }
  return v;
}
function requireArray(v, ctx) {
  if (!Array.isArray(v)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof v}`);
  }
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
      );
    }
  }
}
// =============================================================================
// Breadcrumb (0x0110)
// =============================================================================

export const BREADCRUMB_TAG = 0x0110;
const BREADCRUMB_FIELD_KEYS = new Set([0, 1, 2]);

function renderBreadcrumb(component, ctx) {
  assertOnlyKnownFields(component.fields, BREADCRUMB_FIELD_KEYS, 'Breadcrumb');
  const itemsRaw = ctx.readField(component.fields, 0);
  const items = itemsRaw === undefined ? [] : requireArray(itemsRaw, 'Breadcrumb.items');
  const separator = requireEnum(
    ctx.readField(component.fields, 1),
    BREADCRUMB_SEPARATORS,
    'Breadcrumb.separator'
  );
  // §3 0x0110: default max_items = 5.
  const maxItemsRaw = ctx.readField(component.fields, 2);
  const maxItems = maxItemsRaw === undefined ? 5 : requireU8(maxItemsRaw, 'Breadcrumb.max_items');

  // <tf-breadcrumb> renders the canonical dashboard breadcrumb (single ">"
  // separator). The SDK separator enum is still validated and exposed as a
  // data attribute for tooling, but the visual separator is the component's.
  const root = document.createElement('tf-breadcrumb');
  root.setAttribute('data-separator', separator);

  // Collapse strategy: when items.length > max_items show the first item +
  // "…" + the last (max_items - 2). Keeps the path start and the current
  // location context while hiding the middle.
  const displayItems = collapseBreadcrumbItems(items, maxItems);

  // Clickable entries in DOM order — maps the n-th rendered <a> back to its
  // original item index and action id.
  const clickableEntries = [];

  for (const entry of displayItems) {
    const itemEl = document.createElement('tf-breadcrumb-item');
    if (entry.ellipsis) {
      itemEl.textContent = '…';
      root.appendChild(itemEl);
      continue;
    }
    const item = entry.item;
    const itemIdx = entry.originalIndex;
    assertOnlyKnownInlineKeys(item, BREADCRUMB_ITEM_KEYS, `Breadcrumb.items[${itemIdx}]`);
    const labelBind = readInlineField(item, 0);
    if (labelBind == null) {
      throw new TypeError(`Breadcrumb.items[${itemIdx}].label is required`);
    }
    // icon and local_action: gracefully ignored (optional decorations)
    const isCurrentRaw = readInlineField(item, 4);
    const isCurrent = requireBool(
      isCurrentRaw === undefined ? false : isCurrentRaw,
      `Breadcrumb.items[${itemIdx}].is_current`
    );
    const actionIdRaw = readInlineField(item, 2);
    const actionId = actionIdRaw == null
      ? null
      : requireString(actionIdRaw, `Breadcrumb.items[${itemIdx}].action_id`);

    if (isCurrent) {
      itemEl.setAttribute('current', '');
    }
    if (!isCurrent && actionId != null) {
      // href makes the component render an <a>; navigation is suppressed in
      // the delegated click handler below and replaced by the SDK action.
      itemEl.setAttribute('href', '#');
      clickableEntries.push({ actionId, itemIdx });
    }

    // Label binding. The component's MutationObserver does not watch item
    // text (no subtree), so force a re-render after post-connect updates.
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      itemEl.textContent = v == null ? '' : String(v);
      if (root._nav) root._render();
    };
    applyLabel();
    const off = subscribeBindRef(labelBind, ctx.store, applyLabel);
    ctx.registerCleanup(off);
    root.appendChild(itemEl);
  }

  // Delegated capture-phase click: the component rebuilds its anchors on
  // every internal re-render, so per-anchor listeners would go stale.
  // stopPropagation keeps the raw MouseEvent from reaching consumer
  // listeners on the root — they only ever see our CustomEvent('click')
  // with detail.{action_id, item_index}, same as before the migration.
  const onClick = (e) => {
    const anchor = e.target && e.target.closest
      ? e.target.closest('a.tf-breadcrumb-item')
      : null;
    if (!anchor || !root.contains(anchor)) return;
    e.preventDefault();
    e.stopPropagation();
    const anchors = Array.from(root.querySelectorAll('a.tf-breadcrumb-item'));
    const clickEntry = clickableEntries[anchors.indexOf(anchor)];
    if (!clickEntry) return;
    root.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('click', {
        bubbles: false,
        detail: { action_id: clickEntry.actionId, item_index: clickEntry.itemIdx },
      })
    );
  };
  root.addEventListener('click', onClick, true);
  ctx.registerCleanup(() => root.removeEventListener('click', onClick, true));

  return root;
}

function collapseBreadcrumbItems(items, maxItems) {
  if (items.length <= maxItems || maxItems < 3) {
    return items.map((it, i) => ({ item: it, originalIndex: i }));
  }
  const tail = maxItems - 2;
  const result = [
    { item: items[0], originalIndex: 0 },
    { ellipsis: true },
  ];
  for (let i = items.length - tail; i < items.length; i++) {
    result.push({ item: items[i], originalIndex: i });
  }
  return result;
}

// =============================================================================
// Pagination (0x0111)
// =============================================================================

export const PAGINATION_TAG = 0x0111;
const PAGINATION_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderPagination(component, ctx) {
  assertOnlyKnownFields(component.fields, PAGINATION_FIELD_KEYS, 'Pagination');
  const currentPageBind = ctx.readField(component.fields, 0);
  if (currentPageBind == null) {
    throw new TypeError('Pagination.current_page must be BindRef');
  }
  const totalPagesBind = ctx.readField(component.fields, 1);
  if (totalPagesBind == null) {
    throw new TypeError('Pagination.total_pages must be BindRef');
  }
  const variant = requireEnum(
    ctx.readField(component.fields, 2),
    PAGINATION_VARIANTS,
    'Pagination.variant'
  );
  const showSummaryRaw = ctx.readField(component.fields, 3);
  if (showSummaryRaw === undefined) {
    throw new TypeError('Pagination.show_summary is required');
  }
  const showSummary = requireBool(showSummaryRaw, 'Pagination.show_summary');

  const wrapper = document.createElement('nav');
  wrapper.classList.add('tf-pagination');
  wrapper.classList.add(`tf-pagination--variant-${variant}`);
  wrapper.setAttribute('aria-label', 'Pagination');

  // Prev / Next buttons.
  const prevBtn = document.createElement('button');
  prevBtn.classList.add('tf-pagination__btn', 'tf-pagination__prev');
  prevBtn.setAttribute('type', 'button');
  prevBtn.setAttribute('aria-label', 'Previous page');
  prevBtn.textContent = '‹';
  const nextBtn = document.createElement('button');
  nextBtn.classList.add('tf-pagination__btn', 'tf-pagination__next');
  nextBtn.setAttribute('type', 'button');
  nextBtn.setAttribute('aria-label', 'Next page');
  nextBtn.textContent = '›';

  // Middle element depends on the variant: compact → "N / total",
  // full → page numbers (max 7), input → <input type=number>.
  const middle = document.createElement('div');
  middle.classList.add('tf-pagination__middle');

  let currentValue = null;
  let totalValue = null;

  const emitChange = (target) => {
    if (typeof target !== 'number' || !Number.isInteger(target)) return;
    if (totalValue != null && (target < 1 || target > totalValue)) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { page: target },
      })
    );
  };

  const onPrevClick = () => {
    if (currentValue == null) return;
    emitChange(currentValue - 1);
  };
  const onNextClick = () => {
    if (currentValue == null) return;
    emitChange(currentValue + 1);
  };
  prevBtn.addEventListener('click', onPrevClick);
  nextBtn.addEventListener('click', onNextClick);
  ctx.registerCleanup(() => prevBtn.removeEventListener('click', onPrevClick));
  ctx.registerCleanup(() => nextBtn.removeEventListener('click', onNextClick));

  // Summary text "page X of Y".
  const summary = document.createElement('div');
  summary.classList.add('tf-pagination__summary');
  if (!showSummary) summary.setAttribute('hidden', '');

  // Numeric input (variant='input').
  let pageInput = null;
  if (variant === 'input') {
    pageInput = document.createElement('input');
    pageInput.type = 'number';
    pageInput.min = '1';
    pageInput.classList.add('tf-pagination__input');
    pageInput.setAttribute('aria-label', 'Go to page');
    const onChange = (e) => {
      // Native <input> change bubbles — cut it off so the wrapper listener
      // does not receive the raw native event (no detail) before our
      // CustomEvent (with detail.page).
      e.stopPropagation();
      const parsed = parseInt(pageInput.value, 10);
      if (Number.isInteger(parsed)) emitChange(parsed);
    };
    pageInput.addEventListener('change', onChange);
    ctx.registerCleanup(() => pageInput.removeEventListener('change', onChange));
    middle.appendChild(pageInput);
  }

  const fullVariantPool = [];
  const applyState = () => {
    const c = resolveBindRef(currentPageBind, ctx.store);
    const t = resolveBindRef(totalPagesBind, ctx.store);
    currentValue = typeof c === 'number' && Number.isInteger(c) ? c : null;
    totalValue = typeof t === 'number' && Number.isInteger(t) ? t : null;

    if (currentValue == null || totalValue == null || totalValue < 1) {
      prevBtn.setAttribute('disabled', '');
      nextBtn.setAttribute('disabled', '');
      summary.textContent = '';
      if (pageInput) {
        pageInput.value = '';
        pageInput.removeAttribute('max');
      }
      if (variant === 'compact') middle.textContent = '';
      if (variant === 'full') {
        renderFullVariant(middle, null, null, emitChange, fullVariantPool);
      }
      return;
    }

    prevBtn.toggleAttribute('disabled', currentValue <= 1);
    nextBtn.toggleAttribute('disabled', currentValue >= totalValue);
    summary.textContent = `Strona ${currentValue} z ${totalValue}`;

    if (pageInput) {
      pageInput.value = String(currentValue);
      pageInput.setAttribute('max', String(totalValue));
    }
    if (variant === 'compact') {
      middle.textContent = `${currentValue} / ${totalValue}`;
    }
    if (variant === 'full') {
      renderFullVariant(middle, currentValue, totalValue, emitChange, fullVariantPool);
    }
  };
  // On destroy: release listeners attached to the current numeric buttons.
  if (variant === 'full') {
    ctx.registerCleanup(() => {
      for (const off of fullVariantPool) { try { off(); } catch {} }
      fullVariantPool.length = 0;
    });
  }
  applyState();
  const offC = subscribeBindRef(currentPageBind, ctx.store, applyState);
  const offT = subscribeBindRef(totalPagesBind, ctx.store, applyState);
  ctx.registerCleanup(offC);
  ctx.registerCleanup(offT);

  wrapper.appendChild(prevBtn);
  wrapper.appendChild(middle);
  wrapper.appendChild(nextBtn);
  if (showSummary) wrapper.appendChild(summary);
  return wrapper;
}

/// Per-rerender listener pool for the `full` variant. Every re-render
/// releases all previous listeners, clears the middle, and rebuilds.
/// Without it repeated page changes would accumulate cleanup closures
/// for detached elements.
function renderFullVariant(middle, current, total, emitChange, fullVariantPool) {
  for (const off of fullVariantPool) {
    try { off(); } catch {}
  }
  fullVariantPool.length = 0;
  middle.innerHTML = '';
  if (current == null || total == null) return;
  const pages = computePaginationWindow(current, total);
  for (const p of pages) {
    if (p === '…') {
      const sp = document.createElement('span');
      sp.classList.add('tf-pagination__ellipsis');
      sp.textContent = '…';
      sp.setAttribute('aria-hidden', 'true');
      middle.appendChild(sp);
      continue;
    }
    const btn = document.createElement('button');
    btn.classList.add('tf-pagination__page');
    btn.setAttribute('type', 'button');
    btn.textContent = String(p);
    if (p === current) {
      btn.classList.add('tf-pagination__page--current');
      btn.setAttribute('aria-current', 'page');
    }
    const onClick = () => emitChange(p);
    btn.addEventListener('click', onClick);
    fullVariantPool.push(() => btn.removeEventListener('click', onClick));
    middle.appendChild(btn);
  }
}

function computePaginationWindow(current, total) {
  // Show: 1, …, current-1, current, current+1, …, total. Max 7 spots.
  if (total <= 7) return Array.from({ length: total }, (_, i) => i + 1);
  const pages = [];
  pages.push(1);
  if (current <= 4) {
    pages.push(2, 3, 4, 5, '…', total);
  } else if (current >= total - 3) {
    pages.push(
      '…',
      total - 4, total - 3, total - 2, total - 1, total
    );
  } else {
    pages.push('…', current - 1, current, current + 1, '…', total);
  }
  return pages;
}

// =============================================================================
// Registration
// =============================================================================

export function registerLayoutBreadcrumbPaginationRenderers() {
  if (!lookupComponentRenderer(BREADCRUMB_TAG)) {
    registerComponentRenderer(BREADCRUMB_TAG, renderBreadcrumb);
  }
  if (!lookupComponentRenderer(PAGINATION_TAG)) {
    registerComponentRenderer(PAGINATION_TAG, renderPagination);
  }
}
