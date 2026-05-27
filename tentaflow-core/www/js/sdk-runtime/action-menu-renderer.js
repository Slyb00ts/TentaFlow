// =============================================================================
// File: sdk-runtime/action-menu-renderer.js
// Description: Renderers for MenuButton (0x0406) and Menu (0x0407) using
//              <tf-menu>, <tf-menu-item>, <tf-menu-divider> and <tf-button>
//              web components. Menu standalone uses tf-menu directly.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/actions/menus.rs +
//           inline.rs (MenuItem struct).
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
const MENU_ITEM_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

// SDK variant -> tf-button variant
const VARIANT_MAP = {
  primary: 'primary',
  secondary: 'secondary',
  tertiary: 'ghost',
  ghost: 'ghost',
  destructive: 'danger',
  link: 'ghost',
};

// SDK placement -> tf-menu placement (underscores to hyphens)
function mapPlacement(p) {
  return p.replace(/_/g, '-');
}

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
// Shared: build <tf-menu-item> and <tf-menu-divider> elements
// =============================================================================

function buildMenuItems(items, ctx, menuEl) {
  const seenIds = new Set();
  const itemMeta = [];

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
      // Badge gracefully skipped
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

    const iconRaw = ctx.readField(item, 2);
    const iconName = (iconRaw && iconRaw.kind === 'named') ? iconRaw.name : null;

    const menuItem = document.createElement('tf-menu-item');
    menuItem.setAttribute('action', itemId);
    if (iconName) menuItem.setAttribute('icon', iconName);
    if (shortcut) menuItem.setAttribute('shortcut', shortcut);
    if (danger) menuItem.setAttribute('danger', '');

    // Reactive label
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      menuItem.textContent = v == null ? '' : String(v);
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));

    // Reactive disabled
    let isDisabled = false;
    const disabledBind = ctx.readField(item, 6);
    if (disabledBind != null) {
      const applyDisabled = () => {
        isDisabled = resolveBindRef(disabledBind, ctx.store) === true;
        if (isDisabled) {
          menuItem.setAttribute('disabled', '');
        } else {
          menuItem.removeAttribute('disabled');
        }
      };
      applyDisabled();
      ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, applyDisabled));
    }

    menuEl.appendChild(menuItem);
    itemMeta.push({ itemId, menuItem, label: () => menuItem.textContent || '' });

    if (dividerAfter && i < items.length - 1) {
      const divider = document.createElement('tf-menu-divider');
      menuEl.appendChild(divider);
    }
  }

  return {
    refreshFilter: (query) => {
      const q = (query || '').trim().toLowerCase();
      for (const { menuItem, label } of itemMeta) {
        const text = label().toLowerCase();
        if (q.length === 0 || text.includes(q)) {
          menuItem.removeAttribute('hidden');
        } else {
          menuItem.setAttribute('hidden', '');
        }
      }
    },
  };
}

// =============================================================================
// MenuButton (0x0406) — <tf-button> trigger + <tf-menu> dropdown
// =============================================================================

export const MENU_BUTTON_TAG = 0x0406;
const MENU_BUTTON_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderMenuButton(component, ctx) {
  assertOnlyKnownFields(component.fields, MENU_BUTTON_FIELD_KEYS, 'MenuButton');
  const triggerLabelBind = ctx.readField(component.fields, 0);
  const triggerIconRaw = ctx.readField(component.fields, 1);
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

  // Trigger: <tf-button>
  const trigger = document.createElement('tf-button');
  trigger.setAttribute('variant', VARIANT_MAP[triggerVariant] || 'primary');

  const triggerIconName = (triggerIconRaw && triggerIconRaw.kind === 'named')
    ? triggerIconRaw.name : null;
  if (triggerIconName) trigger.setAttribute('icon', triggerIconName);

  if (triggerLabelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(triggerLabelBind, ctx.store);
      trigger.setAttribute('label', v == null ? '' : String(v));
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(triggerLabelBind, ctx.store, applyLabel));
  } else {
    // Icon-only trigger: propagate a11y label
    const a11yLabel = component.a11y && component.a11y.label;
    if (a11yLabel != null) {
      const apply = () => {
        const v = resolveBindRef(a11yLabel, ctx.store);
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

  // Menu: <tf-menu>
  const menu = document.createElement('tf-menu');
  menu.setAttribute('placement', mapPlacement(placement));
  wrapper.appendChild(menu);

  buildMenuItems(items, ctx, menu);

  // Toggle menu on trigger click
  const onTriggerClick = (e) => {
    e.stopPropagation();
    if (menu.hasAttribute('open')) {
      menu.close();
    } else {
      menu.open();
    }
  };
  trigger.addEventListener('click', onTriggerClick);
  ctx.registerCleanup(() => trigger.removeEventListener('click', onTriggerClick));

  // Forward tf-menu-select to wrapper as item_click
  const onSelect = (e) => {
    menu.close();
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('item_click', {
        bubbles: false,
        detail: { item_id: e.detail && e.detail.action },
      })
    );
  };
  menu.addEventListener('tf-menu-select', onSelect);
  ctx.registerCleanup(() => menu.removeEventListener('tf-menu-select', onSelect));

  return wrapper;
}

// =============================================================================
// Menu (0x0407) — standalone <tf-menu> with optional search
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
    searchInput = document.createElement('tf-input');
    searchInput.setAttribute('type', 'search');
    searchInput.setAttribute('placeholder', 'Search...');
    searchInput.setAttribute('aria-label', 'Search menu');
    searchInput.classList.add('tf-menu-standalone__search');
    wrapper.appendChild(searchInput);
  }

  const menu = document.createElement('tf-menu');
  menu.setAttribute('open', '');
  wrapper.appendChild(menu);

  const ctrl = buildMenuItems(items, ctx, menu);

  if (searchInput) {
    const onInput = () => ctrl.refreshFilter(searchInput.value);
    searchInput.addEventListener('input', onInput);
    ctx.registerCleanup(() => searchInput.removeEventListener('input', onInput));
  }

  // Forward tf-menu-select to wrapper as item_click
  const onSelect = (e) => {
    e.stopPropagation();
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('item_click', {
        bubbles: false,
        detail: { item_id: e.detail && e.detail.action },
      })
    );
  };
  menu.addEventListener('tf-menu-select', onSelect);
  ctx.registerCleanup(() => menu.removeEventListener('tf-menu-select', onSelect));

  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerActionMenuRenderers() {
  if (!lookupComponentRenderer(MENU_BUTTON_TAG)) {
    registerComponentRenderer(MENU_BUTTON_TAG, renderMenuButton);
  }
  if (!lookupComponentRenderer(MENU_TAG)) {
    registerComponentRenderer(MENU_TAG, renderMenu);
  }
}
