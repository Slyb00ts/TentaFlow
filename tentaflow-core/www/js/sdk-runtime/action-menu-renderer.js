// =============================================================================
// Plik: sdk-runtime/action-menu-renderer.js
// Opis: Renderery MenuButton (0x0406) + Menu (0x0407) — Faza 6 Krok 3.3b-5.
// MenuButton: trigger button + dropdown popup. Menu: standalone menu z
// opcjonalnym search. Oba dzielą `MenuItem` rendering (id, label BindRef,
// icon?, badge?, shortcut?, danger, disabled?, divider_after).
// `badge` → throw (InlineBadge wymaga osobnego renderer'a w 3.3d).
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/actions/menus.rs` +
// `inline.rs` (MenuItem struct).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

const BUTTON_VARIANTS = new Set([
  'primary', 'secondary', 'tertiary', 'ghost', 'destructive', 'link',
]);
const MENU_PLACEMENTS = new Set([
  'bottom_start', 'bottom_end',
  'top_start', 'top_end',
  'left_start', 'left_end',
  'right_start', 'right_end',
]);
// MenuItem FieldMap keys: 0=id, 1=label, 2=icon, 3=badge, 4=shortcut, 5=danger, 6=disabled, 7=divider_after
const MENU_ITEM_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`
    );
  }
  return v;
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
function assertOnlyKnownFieldMapKeys(fields, allowedKeys, ctx) {
  if (!Array.isArray(fields)) throw new TypeError(`${ctx}: expected FieldMap`);
  for (const entry of fields) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry must be [u8, Value]`);
    if (!allowedKeys.has(entry[0])) {
      throw new TypeError(`${ctx}: unexpected key ${entry[0]}`);
    }
  }
}

// =============================================================================
// Wspólny renderer MenuItem (używany przez MenuButton i Menu)
// =============================================================================

/// Buduje listę <li> + opcjonalnych separator'ów dla `MenuItem[]`. Klik
/// na item dispatchuje CustomEvent `item_click` z detail.item_id na root
/// element popup'u/listy. Lista nie posiada własnej semantyki focus —
/// zarządza nim caller (MenuButton popup wymaga keyboard nav).
// MenuItem: 0=id, 1=label(BindRef), 2=icon(IconRef), 3=badge(InlineBadge), 4=shortcut, 5=danger, 6=disabled(BindRef), 7=divider_after
function renderMenuItems(items, ctx, listEl, options = {}) {
  const filterPredicate = options.filterPredicate || (() => true);
  const itemMeta = [];
  const seenIds = new Set();
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (!Array.isArray(item)) {
      throw new TypeError(`MenuItem[${i}] must be FieldMap`);
    }
    assertOnlyKnownFieldMapKeys(item, MENU_ITEM_KEYS, `MenuItem[${i}]`);
    const itemId = requireString(ctx.readField(item, 0), `MenuItem[${i}].id`);
    if (itemId.length === 0) {
      throw new TypeError(`MenuItem[${i}].id must be non-empty`);
    }
    if (seenIds.has(itemId)) {
      throw new TypeError(`MenuItem[${i}].id duplicate: '${itemId}'`);
    }
    seenIds.add(itemId);
    const labelBind = ctx.readField(item, 1);
    if (labelBind == null) {
      throw new TypeError(`MenuItem[${i}].label must be BindRef`);
    }
    const badgeRaw = ctx.readField(item, 3);
    if (badgeRaw != null) {
      // InlineBadge ma własny renderer w chunku 3.3d. Tu odrzucamy obecność.
      // Icon/badge gracefully skipped
    }
    const danger = requireBool(
      ctx.readField(item, 5) ?? false,
      `MenuItem[${i}].danger`
    );
    const dividerAfter = requireBool(
      ctx.readField(item, 7) ?? false,
      `MenuItem[${i}].divider_after`
    );
    const shortcutRaw = ctx.readField(item, 4);
    const shortcut = shortcutRaw == null
      ? null
      : requireString(shortcutRaw, `MenuItem[${i}].shortcut`);

    const li = document.createElement('li');
    li.classList.add('tf-menu__item');
    li.setAttribute('role', 'menuitem');
    li.setAttribute('data-menu-item-id', itemId);
    li.setAttribute('tabindex', '-1');
    if (danger) li.classList.add('tf-menu__item--danger');

    const iconRaw = ctx.readField(item, 2);
    if (iconRaw != null) {
      const iconEl = renderIcon(iconRaw, `MenuItem[${i}].icon`);
      iconEl.classList.add('tf-menu__item-icon');
      li.appendChild(iconEl);
    }
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-menu__item-label');
    li.appendChild(labelEl);
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      labelEl.textContent = v == null ? '' : String(v);
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));

    if (shortcut != null) {
      const sc = document.createElement('span');
      sc.classList.add('tf-menu__item-shortcut');
      sc.textContent = shortcut;
      li.appendChild(sc);
    }

    // Reactive disabled przez aria-disabled + ignorowanie kliknięć.
    let isDisabled = false;
    const disabledBind = ctx.readField(item, 6);
    if (disabledBind != null) {
      const applyDisabled = () => {
        isDisabled = resolveBindRef(disabledBind, ctx.store) === true;
        if (isDisabled) {
          li.setAttribute('aria-disabled', 'true');
          li.classList.add('tf-menu__item--disabled');
        } else {
          li.removeAttribute('aria-disabled');
          li.classList.remove('tf-menu__item--disabled');
        }
      };
      applyDisabled();
      ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, applyDisabled));
    }

    const onClick = (e) => {
      if (isDisabled) {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      // stopPropagation zatrzymuje native click event przed bubble do
      // listy — caller ma deterministyczny detail.item_id w `item_click`.
      e.stopPropagation();
      listEl.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('item_click', {
          bubbles: false,
          detail: { item_id: itemId },
        })
      );
    };
    li.addEventListener('click', onClick);
    ctx.registerCleanup(() => li.removeEventListener('click', onClick));

    itemMeta.push({ itemId, li, label: () => labelEl.textContent || '' });
    listEl.appendChild(li);

    if (dividerAfter && i < items.length - 1) {
      const sep = document.createElement('li');
      sep.classList.add('tf-menu__divider');
      sep.setAttribute('role', 'separator');
      listEl.appendChild(sep);
    }
  }
  // filterPredicate — używane przez Menu z search dla filtrowania.
  return {
    refreshFilter: (query) => {
      const q = (query || '').trim().toLowerCase();
      for (const { li, label } of itemMeta) {
        const text = label().toLowerCase();
        if (q.length === 0 || text.includes(q)) {
          li.removeAttribute('hidden');
        } else {
          li.setAttribute('hidden', '');
        }
      }
      // filterPredicate dla testów / future use.
      filterPredicate(q);
    },
  };
}

// =============================================================================
// MenuButton (0x0406)
// =============================================================================

export const MENU_BUTTON_TAG = 0x0406;
const MENU_BUTTON_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderMenuButton(component, ctx) {
  assertOnlyKnownFields(component.fields, MENU_BUTTON_FIELD_KEYS, 'MenuButton');
  const triggerLabelBind = ctx.readField(component.fields, 0); // Option<BindRef>
  const triggerIconRaw = ctx.readField(component.fields, 1); // Option<IconRef>
  const triggerVariant = requireEnum(
    ctx.readField(component.fields, 2),
    BUTTON_VARIANTS,
    'MenuButton.trigger_variant'
  );
  const itemsRaw = ctx.readField(component.fields, 3);
  if (itemsRaw === undefined) {
    throw new TypeError('MenuButton.items is required');
  }
  const items = requireArray(itemsRaw, 'MenuButton.items');
  const placement = requireEnum(
    ctx.readField(component.fields, 4),
    MENU_PLACEMENTS,
    'MenuButton.placement'
  );
  if (triggerLabelBind == null && triggerIconRaw == null) {
    throw new TypeError(
      'MenuButton: at least one of trigger_label / trigger_icon is required'
    );
  }
  // A11y enforcement: bez visual trigger_label, button musi mieć
  // explicite ustawiony accessible name przez Component.a11y.label —
  // named-icon ma aria-hidden=true, więc sam icon nie daje accessible
  // name dla screen reader'ów (analogicznie do Fab).
  if (triggerLabelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'MenuButton without trigger_label requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'MenuButton.a11y.label must resolve to non-blank string at initial render'
      );
    }
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-menu-button');
  wrapper.classList.add(`tf-menu-button--placement-${placement}`);

  // Trigger button.
  const trigger = document.createElement('button');
  trigger.setAttribute('type', 'button');
  trigger.classList.add('tf-menu-button__trigger');
  trigger.classList.add(`tf-button--variant-${triggerVariant}`);
  trigger.setAttribute('aria-haspopup', 'menu');
  trigger.setAttribute('aria-expanded', 'false');

  if (triggerIconRaw != null) {
    const icon = renderIcon(triggerIconRaw, 'MenuButton.trigger_icon');
    icon.classList.add('tf-menu-button__trigger-icon');
    trigger.appendChild(icon);
  }
  if (triggerLabelBind != null) {
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-menu-button__trigger-label');
    trigger.appendChild(labelEl);
    const apply = () => {
      const v = resolveBindRef(triggerLabelBind, ctx.store);
      labelEl.textContent = v == null ? '' : String(v);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(triggerLabelBind, ctx.store, apply));
  } else {
    // Icon-only trigger — accessible name musi być na samym button-trigger
    // (clickable element), nie na wrapper'ze. Engine `applyAccessibility`
    // ustawi aria-label na wrapper, my dodatkowo propagujemy na trigger
    // reaktywnie, żeby screen reader odczytał button z poprawnym labelem.
    const a11yLabel = component.a11y && component.a11y.label;
    if (a11yLabel != null) {
      const apply = () => {
        const v = resolveBindRef(a11yLabel, ctx.store);
        // Trimujemy — whitespace-only nie daje accessible name. Pusty
        // string albo non-string usuwa atrybut.
        if (typeof v === 'string' && v.trim().length > 0) {
          trigger.setAttribute('aria-label', v);
        } else {
          trigger.removeAttribute('aria-label');
        }
      };
      apply();
      ctx.registerCleanup(subscribeBindRef(a11yLabel, ctx.store, apply));
    }
  }
  wrapper.appendChild(trigger);

  // Popup (initially hidden).
  const popupId = `tf-menu-${component.id}-popup`;
  trigger.setAttribute('aria-controls', popupId);
  const popup = document.createElement('ul');
  popup.classList.add('tf-menu-button__popup', 'tf-menu');
  popup.setAttribute('role', 'menu');
  popup.setAttribute('id', popupId);
  popup.setAttribute('hidden', '');
  wrapper.appendChild(popup);

  renderMenuItems(items, ctx, popup);

  // Toggle popup. Klik na trigger przełącza visibility; klik outside
  // (na document) zamyka. Escape też zamyka.
  let open = false;
  const setOpen = (value) => {
    open = value;
    trigger.setAttribute('aria-expanded', value ? 'true' : 'false');
    if (value) {
      popup.removeAttribute('hidden');
      wrapper.classList.add('tf-menu-button--open');
    } else {
      popup.setAttribute('hidden', '');
      wrapper.classList.remove('tf-menu-button--open');
    }
  };
  const onTriggerClick = (e) => {
    e.stopPropagation();
    setOpen(!open);
  };
  const onDocClick = (e) => {
    if (!open) return;
    if (!wrapper.contains(e.target)) setOpen(false);
  };
  const onKeyDown = (e) => {
    if (e.key === 'Escape' && open) {
      setOpen(false);
      trigger.focus();
    }
  };
  trigger.addEventListener('click', onTriggerClick);
  document.addEventListener('click', onDocClick);
  wrapper.addEventListener('keydown', onKeyDown);
  ctx.registerCleanup(() => trigger.removeEventListener('click', onTriggerClick));
  ctx.registerCleanup(() => document.removeEventListener('click', onDocClick));
  ctx.registerCleanup(() => wrapper.removeEventListener('keydown', onKeyDown));

  // Klik na MenuItem (item_click) zamyka popup.
  const onItemClick = () => setOpen(false);
  popup.addEventListener('item_click', onItemClick);
  ctx.registerCleanup(() => popup.removeEventListener('item_click', onItemClick));
  // Re-emit item_click na wrapper żeby Component.handlers `select`/`item_click`
  // mogło to złapać. Engine attachuje listenery do `wrapper` (root element).
  const onItemReemit = (e) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('item_click', {
        bubbles: false,
        detail: e.detail,
      })
    );
  };
  popup.addEventListener('item_click', onItemReemit);
  ctx.registerCleanup(() => popup.removeEventListener('item_click', onItemReemit));

  return wrapper;
}

// =============================================================================
// Menu (0x0407) — standalone menu z opcjonalnym search
// =============================================================================

export const MENU_TAG = 0x0407;
const MENU_FIELD_KEYS = new Set([0, 1]);

function renderMenu(component, ctx) {
  assertOnlyKnownFields(component.fields, MENU_FIELD_KEYS, 'Menu');
  const itemsRaw = ctx.readField(component.fields, 0);
  if (itemsRaw === undefined) {
    throw new TypeError('Menu.items is required');
  }
  const items = requireArray(itemsRaw, 'Menu.items');
  const searchRaw = ctx.readField(component.fields, 1);
  if (searchRaw === undefined) {
    throw new TypeError('Menu.search is required');
  }
  const search = requireBool(searchRaw, 'Menu.search');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-menu-standalone');

  let searchInput = null;
  if (search) {
    searchInput = document.createElement('input');
    searchInput.setAttribute('type', 'search');
    searchInput.classList.add('tf-menu-standalone__search');
    searchInput.setAttribute('placeholder', 'Szukaj…');
    searchInput.setAttribute('aria-label', 'Szukaj w menu');
    wrapper.appendChild(searchInput);
  }

  const list = document.createElement('ul');
  list.classList.add('tf-menu-standalone__list', 'tf-menu');
  list.setAttribute('role', 'menu');
  wrapper.appendChild(list);

  const ctrl = renderMenuItems(items, ctx, list);

  if (searchInput) {
    const onInput = () => ctrl.refreshFilter(searchInput.value);
    searchInput.addEventListener('input', onInput);
    ctx.registerCleanup(() => searchInput.removeEventListener('input', onInput));
  }

  // Re-emit item_click z listy na wrapper (analogicznie do MenuButton).
  const onItemReemit = (e) => {
    e.stopPropagation();
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('item_click', {
        bubbles: false,
        detail: e.detail,
      })
    );
  };
  list.addEventListener('item_click', onItemReemit);
  ctx.registerCleanup(() => list.removeEventListener('item_click', onItemReemit));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerActionMenuRenderers() {
  if (!lookupComponentRenderer(MENU_BUTTON_TAG)) {
    registerComponentRenderer(MENU_BUTTON_TAG, renderMenuButton);
  }
  if (!lookupComponentRenderer(MENU_TAG)) {
    registerComponentRenderer(MENU_TAG, renderMenu);
  }
}
