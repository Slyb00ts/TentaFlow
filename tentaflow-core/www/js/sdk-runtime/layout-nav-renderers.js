// =============================================================================
// Plik: sdk-runtime/layout-nav-renderers.js
// Opis: Renderer NavTabs (tag 0x010C) z Layout nav (Faza 6 Krok 3.3a-4).
// Pozostałe nav primitives (Sidebar 0x010A, Tabs 0x010B, Breadcrumb 0x0110,
// Pagination 0x0111) lecą w kolejnych sub-chunkach — Sidebar/Tabs wymagają
// slot manager z chunka 3.5, Breadcrumb/Pagination dochodzą w 3.3a-5/6.
//
// NavTabs: page-level routing tabs. Items: NavTab { id, label, icon?,
// badge?, panel_id?, locked }. Active item podświetlony przez `active_id`
// BindRef. `scroll_overflow=true` włącza horizontal scroll dla overflow.
// Handler `select` emitowany przy kliknięciu na tab.
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/layout/nav.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import {
  resolveBindRef,
  subscribeBindRef,
} from './bind-resolver.js';

const NAV_TABS_VARIANTS = new Set(['default', 'underlined', 'pills']);

const NAV_TAB_KEYS = new Set([
  'id', 'label', 'icon', 'badge', 'panel_id', 'locked',
]);

function requireEnum(value, set, ctx) {
  if (typeof value !== 'string' || !set.has(value)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(value)}`
    );
  }
  return value;
}

function requireBool(value, ctx) {
  if (typeof value !== 'boolean') {
    throw new TypeError(`${ctx}: expected boolean, got ${typeof value}`);
  }
  return value;
}

function requireString(value, ctx) {
  if (typeof value !== 'string') {
    throw new TypeError(`${ctx}: expected string, got ${typeof value}`);
  }
  return value;
}

function requireArray(value, ctx) {
  if (!Array.isArray(value)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof value}`);
  }
  return value;
}

function assertOnlyKnownFields(fields, allowedKeys, componentName) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${componentName}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
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
// NavTabs (0x010C)
// =============================================================================

export const NAV_TABS_TAG = 0x010C;
const NAV_TABS_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderNavTabs(component, ctx) {
  assertOnlyKnownFields(component.fields, NAV_TABS_FIELD_KEYS, 'NavTabs');
  const itemsRaw = ctx.readField(component.fields, 0);
  const items =
    itemsRaw === undefined ? [] : requireArray(itemsRaw, 'NavTabs.items');
  const activeIdBind = ctx.readField(component.fields, 1);
  if (activeIdBind == null) {
    throw new TypeError('NavTabs.active_id must be BindRef');
  }
  const variant = requireEnum(
    ctx.readField(component.fields, 2),
    NAV_TABS_VARIANTS,
    'NavTabs.variant'
  );
  const scrollOverflowRaw = ctx.readField(component.fields, 3);
  if (scrollOverflowRaw === undefined) {
    throw new TypeError('NavTabs.scroll_overflow is required');
  }
  const scrollOverflow = requireBool(scrollOverflowRaw, 'NavTabs.scroll_overflow');

  const wrapper = document.createElement('nav');
  wrapper.classList.add('tf-nav-tabs');
  wrapper.classList.add(`tf-nav-tabs--variant-${variant}`);
  if (scrollOverflow) wrapper.classList.add('tf-nav-tabs--scroll');
  wrapper.setAttribute('role', 'tablist');

  // Mapowanie itemów. Każdy tab to <button role="tab"> z id, aria-selected
  // sterowanym reaktywnie przez `active_id` BindRef.
  const tabEls = [];
  const seenIds = new Set();
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (!item || typeof item !== 'object') {
      throw new TypeError(`NavTabs.items[${i}] must be object`);
    }
    assertOnlyKnownObjectKeys(item, NAV_TAB_KEYS, `NavTabs.items[${i}]`);
    const itemId = requireString(item.id, `NavTabs.items[${i}].id`);
    if (itemId.length === 0) {
      throw new TypeError(`NavTabs.items[${i}].id must be non-empty`);
    }
    if (seenIds.has(itemId)) {
      throw new TypeError(`NavTabs.items: duplicate id '${itemId}'`);
    }
    seenIds.add(itemId);
    const labelBind = item.label;
    if (labelBind == null) {
      throw new TypeError(`NavTabs.items[${i}].label must be BindRef`);
    }
    const locked = requireBool(
      item.locked === undefined ? false : item.locked,
      `NavTabs.items[${i}].locked`
    );
    // `icon` i `badge` wymagają icon registry / InlineBadge renderer'ów
    // (chunki 3.3d/e). Renderer NIE renderuje ich potajemnie — odrzuca
    // explicitnie, żeby addon dostał deterministyczny error zamiast
    // niezamierzonego silent-drop.
    if (item.icon != null) {
      throw new Error(
        `NavTabs.items[${i}].icon: IconRef rendering deferred to chunk 3.3d`
      );
    }
    if (item.badge != null) {
      throw new Error(
        `NavTabs.items[${i}].badge: InlineBadge rendering deferred to chunk 3.3d`
      );
    }
    if (item.panel_id != null) {
      // `panel_id` jest opcjonalnym mostem do Router'a (cross-panel nav).
      // Walidujemy że to string, ale faktyczne routing pociągamy w chunku
      // 3.7 (cutover). Tu zostaje atrybut data-panel-id do dyspozycji
      // routera shell'a.
      requireString(item.panel_id, `NavTabs.items[${i}].panel_id`);
    }

    const btn = document.createElement('button');
    btn.classList.add('tf-nav-tabs__tab');
    btn.setAttribute('role', 'tab');
    btn.setAttribute('type', 'button');
    btn.setAttribute('data-nav-tab-id', itemId);
    if (item.panel_id != null) {
      btn.setAttribute('data-nav-panel-id', item.panel_id);
    }
    if (locked) {
      btn.setAttribute('disabled', '');
      btn.classList.add('tf-nav-tabs__tab--locked');
    }

    // Label binding — reactive text. Reuse pattern z innych renderer'ów.
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-nav-tabs__label');
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      labelEl.textContent = v == null ? '' : String(v);
    };
    applyLabel();
    const offLabel = subscribeBindRef(labelBind, ctx.store, applyLabel);
    ctx.registerCleanup(offLabel);
    btn.appendChild(labelEl);

    // Klik → emit CustomEvent('select') na <nav> wrapper'ze z
    // detail.item_id. Engine attachuje listenery `select` handlers
    // addona na wrapper'ze, więc dostają item id.
    const onClick = (e) => {
      if (locked) {
        e.preventDefault();
        return;
      }
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('select', {
          bubbles: false,
          detail: { item_id: itemId },
        })
      );
    };
    btn.addEventListener('click', onClick);
    ctx.registerCleanup(() => btn.removeEventListener('click', onClick));

    wrapper.appendChild(btn);
    tabEls.push({ itemId, btn });
  }

  // Reactive aktywny tab — single source of truth = `active_id` BindRef.
  const applyActive = () => {
    const activeId = resolveBindRef(activeIdBind, ctx.store);
    for (const { itemId, btn } of tabEls) {
      const isActive = activeId === itemId;
      btn.setAttribute('aria-selected', isActive ? 'true' : 'false');
      btn.setAttribute('tabindex', isActive ? '0' : '-1');
      if (isActive) {
        btn.classList.add('tf-nav-tabs__tab--active');
      } else {
        btn.classList.remove('tf-nav-tabs__tab--active');
      }
    }
  };
  applyActive();
  const off = subscribeBindRef(activeIdBind, ctx.store, applyActive);
  ctx.registerCleanup(off);

  // Keyboard nav: Arrow Left/Right przesuwa fokus po tab-ach (WAI-ARIA tab pattern).
  const onKeyNav = (e) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    const focusable = tabEls.filter(
      ({ btn }) => !btn.hasAttribute('disabled')
    );
    if (focusable.length === 0) return;
    const idx = focusable.findIndex(({ btn }) => btn === document.activeElement);
    if (idx < 0) return;
    e.preventDefault();
    const delta = e.key === 'ArrowLeft' ? -1 : 1;
    const next = (idx + delta + focusable.length) % focusable.length;
    focusable[next].btn.focus();
  };
  wrapper.addEventListener('keydown', onKeyNav);
  ctx.registerCleanup(() => wrapper.removeEventListener('keydown', onKeyNav));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerLayoutNavRenderers() {
  if (!lookupComponentRenderer(NAV_TABS_TAG)) {
    registerComponentRenderer(NAV_TABS_TAG, renderNavTabs);
  }
}
