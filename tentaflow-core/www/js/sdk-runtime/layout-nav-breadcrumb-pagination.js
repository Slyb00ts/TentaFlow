// =============================================================================
// Plik: sdk-runtime/layout-nav-breadcrumb-pagination.js
// Opis: Rendererzy Breadcrumb (0x0110) + Pagination (0x0111) — Faza 6
// Krok 3.3a-5. Bez zależności od slot manager (chunk 3.5) ani icon
// registry (chunk 3.3d) — icon/local_action present jest odrzucany.
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/nav.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

const BREADCRUMB_SEPARATORS = new Set(['chevron', 'slash', 'dot']);
const PAGINATION_VARIANTS = new Set(['compact', 'full', 'input']);

const BREADCRUMB_ITEM_KEYS = new Set([
  'label', 'icon', 'action_id', 'local_action', 'is_current',
]);

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
function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}'`);
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

  const nav = document.createElement('nav');
  nav.classList.add('tf-breadcrumb');
  nav.classList.add(`tf-breadcrumb--separator-${separator}`);
  nav.setAttribute('aria-label', 'Breadcrumb');
  const list = document.createElement('ol');
  list.classList.add('tf-breadcrumb__list');
  nav.appendChild(list);

  // Strategia collapse'u: jeśli items.length > max_items, pokazujemy
  // pierwszy + "…" + ostatnie (max_items - 2). To zachowuje początek
  // ścieżki + kontekst bieżącej lokacji, ukrywając środek.
  const displayItems = collapseBreadcrumbItems(items, maxItems);

  for (let i = 0; i < displayItems.length; i++) {
    const entry = displayItems[i];
    if (entry.ellipsis) {
      const li = document.createElement('li');
      li.classList.add('tf-breadcrumb__item');
      li.classList.add('tf-breadcrumb__item--ellipsis');
      li.setAttribute('aria-hidden', 'true');
      li.textContent = '…';
      list.appendChild(li);
      if (i < displayItems.length - 1) {
        list.appendChild(makeSeparator(separator));
      }
      continue;
    }
    const item = entry.item;
    const itemIdx = entry.originalIndex;
    assertOnlyKnownObjectKeys(item, BREADCRUMB_ITEM_KEYS, `Breadcrumb.items[${itemIdx}]`);
    if (item.label == null) {
      throw new TypeError(`Breadcrumb.items[${itemIdx}].label is required`);
    }
    // icon and local_action: gracefully ignored (optional decorations)
    const isCurrent = requireBool(
      item.is_current === undefined ? false : item.is_current,
      `Breadcrumb.items[${itemIdx}].is_current`
    );
    const actionId = item.action_id == null
      ? null
      : requireString(item.action_id, `Breadcrumb.items[${itemIdx}].action_id`);

    const li = document.createElement('li');
    li.classList.add('tf-breadcrumb__item');
    if (isCurrent) {
      li.classList.add('tf-breadcrumb__item--current');
    }

    // Element interaktywny: <a> dla klikalnych, <span> dla bieżącej
    // pozycji. Klik na breadcrumb item z action_id dispatchuje
    // CustomEvent('click') z detail.{action_id, item_index} —
    // dispatcher (chunk 3.6) zmapuje action_id na backend handler.
    const inner = isCurrent || actionId == null
      ? document.createElement('span')
      : document.createElement('a');
    inner.classList.add('tf-breadcrumb__link');
    if (inner.tagName === 'A') {
      inner.setAttribute('href', '#');
      inner.setAttribute('role', 'link');
    }
    if (isCurrent) {
      // ARIA: aria-current="page" idzie na ELEMENT WEWNĘTRZNY (link/span),
      // nie na li — zgodnie z W3C wcag practices breadcrumb pattern.
      inner.setAttribute('aria-current', 'page');
    }
    if (!isCurrent && actionId != null) {
      const onClick = (e) => {
        e.preventDefault();
        // Native MouseEvent musi NIE bubble do nav — inaczej nadpisałby
        // nasz CustomEvent('click', { detail: ... }) gołym MouseEvent'em
        // z `detail=clickCount` w listenerze nav-level.
        e.stopPropagation();
        nav.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('click', {
            bubbles: false,
            detail: { action_id: actionId, item_index: itemIdx },
          })
        );
      };
      inner.addEventListener('click', onClick);
      ctx.registerCleanup(() => inner.removeEventListener('click', onClick));
    }
    // Label binding.
    const applyLabel = () => {
      const v = resolveBindRef(item.label, ctx.store);
      inner.textContent = v == null ? '' : String(v);
    };
    applyLabel();
    const off = subscribeBindRef(item.label, ctx.store, applyLabel);
    ctx.registerCleanup(off);
    li.appendChild(inner);
    list.appendChild(li);
    if (i < displayItems.length - 1) {
      list.appendChild(makeSeparator(separator));
    }
  }
  return nav;
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

function makeSeparator(kind) {
  const sep = document.createElement('li');
  sep.classList.add('tf-breadcrumb__separator');
  sep.classList.add(`tf-breadcrumb__separator--${kind}`);
  sep.setAttribute('aria-hidden', 'true');
  switch (kind) {
    case 'chevron': sep.textContent = '›'; break;
    case 'slash': sep.textContent = '/'; break;
    case 'dot': sep.textContent = '·'; break;
  }
  return sep;
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

  // Prev / Next przyciski.
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

  // Środkowy element zależy od wariantu: compact → "N / total",
  // full → numerki stron (max 7), input → <input type=number>.
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

  // Summary tekst "page X of Y".
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
      // Native <input> change event bubbles — odetnijmy żeby listener na
      // wrapper'ze nie dostał najpierw native eventu (bez detail), a
      // potem naszego CustomEvent (z detail.page).
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
  // Po destroy: zwolnij listenery dla aktualnych numeric buttons.
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

/// Per-rerender pool listenerów `full` variant'u. Każde przerenderowanie
/// odpala wszystkie poprzednie listenery, czyści middle, i buduje od nowa.
/// Bez tego repeated page changes akumulowałyby cleanup closures dla
/// odłączonych elementów.
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
  // Pokazujemy: 1, …, current-1, current, current+1, …, total. Max 7 spots.
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
// Rejestracja
// =============================================================================

export function registerLayoutBreadcrumbPaginationRenderers() {
  if (!lookupComponentRenderer(BREADCRUMB_TAG)) {
    registerComponentRenderer(BREADCRUMB_TAG, renderBreadcrumb);
  }
  if (!lookupComponentRenderer(PAGINATION_TAG)) {
    registerComponentRenderer(PAGINATION_TAG, renderPagination);
  }
}
