// =============================================================================
// Plik: sdk-runtime/layout-nav-renderers.js
// Opis: NavTabs renderer (tag 0x010C) from Layout nav.
// Uses <tf-tabs> + <tf-tab> web components for tab rendering with FLIP
// indicator, horizontal overflow, and chevron scroll.
//
// NavTabs: page-level routing tabs. Items: NavTab { id, label, icon?,
// badge?, panel_id?, locked }. Active item via `active_id` BindRef.
// Handler `select` emitted on tab click.
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

// Map SDK variant names to tf-tabs variant names
const VARIANT_MAP = {
  'default': 'solid',
  'underlined': 'underline',
  'pills': 'soft',
};

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

// =============================================================================
// NavTabs (0x010C) — uses <tf-tabs> + <tf-tab> web components
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
  requireBool(scrollOverflowRaw, 'NavTabs.scroll_overflow');

  // Create <tf-tabs> web component
  const tfTabs = document.createElement('tf-tabs');
  tfTabs.setAttribute('variant', VARIANT_MAP[variant] || 'solid');

  // Parse and validate items, then create <tf-tab> children
  const tabIds = [];
  const seenIds = new Set();
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    const itemId = requireString(ctx.readField(item, 0), `NavTabs.items[${i}].id`);
    if (itemId.length === 0) {
      throw new TypeError(`NavTabs.items[${i}].id must be non-empty`);
    }
    if (seenIds.has(itemId)) {
      throw new TypeError(`NavTabs.items: duplicate id '${itemId}'`);
    }
    seenIds.add(itemId);
    const labelBind = ctx.readField(item, 1);
    if (labelBind == null) {
      throw new TypeError(`NavTabs.items[${i}].label must be BindRef`);
    }
    const locked = requireBool(
      ctx.readField(item, 5) ?? false,
      `NavTabs.items[${i}].locked`
    );
    const itemIcon = ctx.readField(item, 2) ?? null;
    const itemBadge = ctx.readField(item, 3) ?? null;
    const panelId = ctx.readField(item, 4) ?? null;

    // Create <tf-tab> web component
    const tfTab = document.createElement('tf-tab');
    tfTab.id = itemId;

    // Icon attribute
    if (itemIcon) {
      const iconName = typeof itemIcon === 'string' ? itemIcon
        : ctx.readField(itemIcon, 1) || '';
      if (iconName) {
        tfTab.setAttribute('icon', iconName);
      }
    }

    // Badge/count attribute
    if (itemBadge != null) {
      tfTab.setAttribute('count', String(itemBadge));
    }

    // Locked = disabled
    if (locked) {
      tfTab.setAttribute('disabled', '');
    }

    if (panelId != null) {
      tfTab.setAttribute('data-nav-panel-id', panelId);
    }

    // Reactive label — tf-tab uses textContent (innerHTML) as label source
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      const text = v == null ? '' : String(v);
      // tf-tab stores original label in _btn._label and uses it in _update()
      if (tfTab._btn) {
        tfTab._btn._label = text;
        tfTab._update();
      } else {
        tfTab.textContent = text;
      }
    };
    applyLabel();
    const offLabel = subscribeBindRef(labelBind, ctx.store, applyLabel);
    ctx.registerCleanup(offLabel);

    tfTabs.appendChild(tfTab);
    tabIds.push(itemId);
  }

  // Reactive active tab — set tf-tabs `value` property
  const applyActive = () => {
    const activeId = resolveBindRef(activeIdBind, ctx.store);
    if (activeId != null) {
      tfTabs.value = String(activeId);
    }
  };
  applyActive();
  const off = subscribeBindRef(activeIdBind, ctx.store, applyActive);
  ctx.registerCleanup(off);

  // tf-tabs emits `change` event with detail.value — translate to SDK `select`
  const onChange = (e) => {
    const selectedId = e.detail?.value;
    if (!selectedId) return;
    tfTabs.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('select', {
        bubbles: false,
        detail: { item_id: selectedId },
      })
    );
  };
  tfTabs.addEventListener('change', onChange);
  ctx.registerCleanup(() => tfTabs.removeEventListener('change', onChange));

  return tfTabs;
}

// =============================================================================
// Registration
// =============================================================================

export function registerLayoutNavRenderers() {
  if (!lookupComponentRenderer(NAV_TABS_TAG)) {
    registerComponentRenderer(NAV_TABS_TAG, renderNavTabs);
  }
}
