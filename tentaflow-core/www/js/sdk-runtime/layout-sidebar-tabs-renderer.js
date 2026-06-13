// =============================================================================
// Plik: sdk-runtime/layout-sidebar-tabs-renderer.js
// Opis: Sidebar (0x010A) and Tabs (0x010B) renderers from Layout nav.
//
// Sidebar: vertical nav container. header_slot/footer_slot are data-slot-id
// regions the SlotManager fills; items are SidebarItem nav rows (id, icon?,
// label, badge?, active_path?, action_id|local_action, children? 1-level).
// Active state follows each item's `active_path` (StatePath → bool). Selecting
// a row dispatches the SDK `select` event with { item_id, action_id }.
//
// Tabs: content-tab container. The tab strip is a <tf-tabs>/<tf-tab> web
// component; below it a single `content_slot` data-slot-id region holds the
// active tab's panel content (the SlotManager swaps it). Active tab follows the
// `active_id` BindRef. Selecting a tab dispatches the SDK `select` event with
// { item_id }. Distinct from NavTabs (0x010C): NavTabs is page-level routing,
// Tabs swaps in-panel content via the content slot.
//
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

// =============================================================================
// Shared validators (mirror Rust unknown_field / typed decoders)
// =============================================================================

const TABS_VARIANTS = new Set(['default', 'pills', 'underlined', 'boxed']);
const DENSITIES = new Set(['compact', 'default', 'comfortable']);

// SDK TabsVariant → tf-tabs variant names.
const TABS_VARIANT_MAP = {
  'default': 'solid',
  'pills': 'soft',
  'underlined': 'underline',
  'boxed': 'solid',
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

function requireNonEmptyString(value, ctx) {
  if (typeof value !== 'string' || value.length === 0) {
    throw new TypeError(`${ctx}: expected non-empty string, got ${JSON.stringify(value)}`);
  }
  return value;
}

function requireArray(value, ctx) {
  if (!Array.isArray(value)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof value}`);
  }
  return value;
}

/// Reject any FieldMap key not in `allowedKeys`. Mirror of Rust
/// `unknown_field(...)` — an addon sending an unknown field MUST error rather
/// than have it silently ignored.
function assertOnlyKnownFields(fields, allowedKeys, componentName) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${componentName}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
      );
    }
  }
}

/// IconRef → sprite name string (only `named` icons map to a sprite name;
/// `asset` icons have no sprite name so the row renders text-only).
function iconRefName(iconRaw) {
  if (iconRaw == null) return null;
  if (typeof iconRaw === 'string') return iconRaw;
  if (typeof iconRaw === 'object' && iconRaw.kind === 'named') {
    return typeof iconRaw.name === 'string' ? iconRaw.name : null;
  }
  return null;
}

/// InlineBadge → bound BindRef source (count preferred, else label) or a scalar
/// legacy value. Returns `{ bind }` when the value is reactive, `{ value }` for a
/// plain scalar, or null when there is no badge.
function badgeSource(badgeRaw) {
  if (badgeRaw == null) return null;
  if (typeof badgeRaw !== 'object') return { value: String(badgeRaw) };
  const src = badgeRaw.count != null ? badgeRaw.count
    : (badgeRaw.label != null ? badgeRaw.label : null);
  if (src == null) return null;
  return { bind: src };
}

/// Resolve a badgeSource() result to its current display string (or null).
function badgeDisplay(src, store) {
  if (src == null) return null;
  if (src.value != null) return src.value;
  const v = resolveBindRef(src.bind, store);
  return v == null ? null : String(v);
}

function iconSvg(name) {
  // Sprite reference identical to tf-tab's icon markup (#i-<name>).
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('width', '16');
  svg.setAttribute('height', '16');
  svg.setAttribute('aria-hidden', 'true');
  svg.classList.add('tf-sidebar__icon');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  svg.appendChild(use);
  return svg;
}

// =============================================================================
// Sidebar (0x010A)
// =============================================================================

export const SIDEBAR_TAG = 0x010A;
const SIDEBAR_FIELD_KEYS = new Set([0, 1, 2, 3]);
// SidebarItem: 0=id, 1=icon, 2=label, 3=badge, 4=active_path, 5=action_id,
//              6=local_action, 7=children.
const SIDEBAR_ITEM_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7]);

function renderSidebar(component, ctx) {
  assertOnlyKnownFields(component.fields, SIDEBAR_FIELD_KEYS, 'Sidebar');

  const headerSlotRaw = ctx.readField(component.fields, 0);
  const headerSlot = headerSlotRaw == null
    ? null : requireNonEmptyString(headerSlotRaw, 'Sidebar.header_slot');
  const itemsRaw = ctx.readField(component.fields, 1);
  const items = itemsRaw === undefined ? [] : requireArray(itemsRaw, 'Sidebar.items');
  const footerSlotRaw = ctx.readField(component.fields, 2);
  const footerSlot = footerSlotRaw == null
    ? null : requireNonEmptyString(footerSlotRaw, 'Sidebar.footer_slot');
  const collapsedBind = ctx.readField(component.fields, 3) ?? null;

  const root = document.createElement('nav');
  root.classList.add('tf-sidebar');

  if (headerSlot != null) {
    const header = document.createElement('div');
    header.classList.add('tf-sidebar__header');
    header.setAttribute('data-slot-id', headerSlot);
    root.appendChild(header);
  }

  const list = document.createElement('ul');
  list.classList.add('tf-sidebar__nav');
  const seenIds = new Set();
  for (let i = 0; i < items.length; i++) {
    list.appendChild(
      renderSidebarItem(items[i], `Sidebar.items[${i}]`, ctx, seenIds, false, root)
    );
  }
  root.appendChild(list);

  if (footerSlot != null) {
    const footer = document.createElement('div');
    footer.classList.add('tf-sidebar__footer');
    footer.setAttribute('data-slot-id', footerSlot);
    root.appendChild(footer);
  }

  // collapsed BindRef → bool toggles the --collapsed modifier reactively.
  if (collapsedBind != null) {
    const applyCollapsed = () => {
      const v = resolveBindRef(collapsedBind, ctx.store);
      root.classList.toggle('tf-sidebar--collapsed', v === true);
    };
    applyCollapsed();
    ctx.registerCleanup(subscribeBindRef(collapsedBind, ctx.store, applyCollapsed));
  }

  return root;
}

function renderSidebarItem(item, path, ctx, seenIds, isNested, root) {
  if (!Array.isArray(item)) {
    throw new TypeError(`${path}: SidebarItem must be a FieldMap`);
  }
  assertOnlyKnownFields(item, SIDEBAR_ITEM_KEYS, `${path} (SidebarItem)`);

  const id = requireNonEmptyString(ctx.readField(item, 0), `${path}.id`);
  if (seenIds.has(id)) {
    throw new TypeError(`Sidebar.items: duplicate id '${id}'`);
  }
  seenIds.add(id);

  const labelBind = ctx.readField(item, 2);
  if (labelBind == null) {
    throw new TypeError(`${path}.label must be BindRef`);
  }
  const iconRaw = ctx.readField(item, 1) ?? null;
  const badgeRaw = ctx.readField(item, 3) ?? null;
  const activePath = ctx.readField(item, 4) ?? null;
  if (activePath != null && !Array.isArray(activePath)) {
    throw new TypeError(`${path}.active_path: expected StatePath (Array<PathSegment>)`);
  }
  const actionId = ctx.readField(item, 5) ?? null;
  if (actionId != null) requireString(actionId, `${path}.action_id`);
  const localAction = ctx.readField(item, 6) ?? null;
  // §1.5: action_id and local_action are mutually exclusive.
  if (actionId != null && localAction != null) {
    throw new TypeError(`${path}: action_id and local_action are mutually exclusive`);
  }
  const childrenRaw = ctx.readField(item, 7) ?? null;
  if (childrenRaw != null && !Array.isArray(childrenRaw)) {
    throw new TypeError(`${path}.children: expected Array<SidebarItem>`);
  }
  // Renderer enforces a 1-level nesting limit (Krok 4 host validator mirror).
  if (isNested && childrenRaw != null && childrenRaw.length > 0) {
    throw new TypeError(`${path}.children: nested items may not have their own children`);
  }

  const li = document.createElement('li');
  li.classList.add('tf-sidebar__item');

  const link = document.createElement('button');
  link.type = 'button';
  link.classList.add('tf-sidebar__link');
  link.dataset.itemId = id;

  const iconName = iconRefName(iconRaw);
  if (iconName) link.appendChild(iconSvg(iconName));

  const labelEl = document.createElement('span');
  labelEl.classList.add('tf-sidebar__label');
  const applyLabel = () => {
    const v = resolveBindRef(labelBind, ctx.store);
    labelEl.textContent = v == null ? '' : String(v);
  };
  applyLabel();
  ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
  link.appendChild(labelEl);

  // InlineBadge count/label is a BindRef — subscribe so a store update to the
  // bound value updates the visible badge (mirrors the reactive label binding).
  const badgeSrc = badgeSource(badgeRaw);
  if (badgeSrc != null) {
    const badgeEl = document.createElement('span');
    badgeEl.classList.add('tf-sidebar__badge');
    const applyBadge = () => {
      const text = badgeDisplay(badgeSrc, ctx.store);
      badgeEl.textContent = text == null ? '' : text;
      badgeEl.hidden = text == null;
    };
    applyBadge();
    link.appendChild(badgeEl);
    if (badgeSrc.bind != null) {
      ctx.registerCleanup(subscribeBindRef(badgeSrc.bind, ctx.store, applyBadge));
    }
  }

  // active_path → bool drives the active state (and aria-current) reactively.
  if (activePath != null) {
    const applyActive = () => {
      const active = ctx.store.read(activePath) === true;
      link.classList.toggle('is-active', active);
      if (active) link.setAttribute('aria-current', 'page');
      else link.removeAttribute('aria-current');
    };
    applyActive();
    ctx.registerCleanup(ctx.store.subscribe(activePath, applyActive));
  }

  // Selecting a row re-dispatches the SDK `select` event on the renderer root —
  // ComponentRenderer installs the addon's `select` handler on the root element,
  // so a non-bubbling event must originate there. __tfReemit keeps it idempotent.
  const onClick = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    const ce = new (globalThis.CustomEvent || globalThis.Event)('select', {
      bubbles: false,
      detail: { item_id: id, action_id: actionId },
    });
    ce.__tfReemit = true;
    root.dispatchEvent(ce);
  };
  link.addEventListener('click', onClick);
  ctx.registerCleanup(() => link.removeEventListener('click', onClick));

  li.appendChild(link);

  if (childrenRaw != null && childrenRaw.length > 0) {
    const sub = document.createElement('ul');
    sub.classList.add('tf-sidebar__sub');
    for (let i = 0; i < childrenRaw.length; i++) {
      sub.appendChild(
        renderSidebarItem(childrenRaw[i], `${path}.children[${i}]`, ctx, seenIds, true, root)
      );
    }
    li.appendChild(sub);
  }

  return li;
}

// =============================================================================
// Tabs (0x010B) — tab strip via <tf-tabs> + content_slot region
// =============================================================================

export const TABS_TAG = 0x010B;
const TABS_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
// TabItem: 0=id, 1=label, 2=icon, 3=badge, 4=locked, 5=content_template_id.
const TAB_ITEM_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderTabs(component, ctx) {
  assertOnlyKnownFields(component.fields, TABS_FIELD_KEYS, 'Tabs');

  const variant = requireEnum(
    ctx.readField(component.fields, 0), TABS_VARIANTS, 'Tabs.variant'
  );
  const itemsRaw = ctx.readField(component.fields, 1);
  const items = itemsRaw === undefined ? [] : requireArray(itemsRaw, 'Tabs.items');
  const activeIdBind = ctx.readField(component.fields, 2);
  if (activeIdBind == null) {
    throw new TypeError('Tabs.active_id must be BindRef');
  }
  const contentSlot = requireNonEmptyString(
    ctx.readField(component.fields, 3), 'Tabs.content_slot'
  );
  const density = requireEnum(
    ctx.readField(component.fields, 4), DENSITIES, 'Tabs.density'
  );

  const root = document.createElement('div');
  root.classList.add('tf-tabs-container');
  root.classList.add(`tf-tabs-container--density-${density}`);

  const tfTabs = document.createElement('tf-tabs');
  tfTabs.setAttribute('variant', TABS_VARIANT_MAP[variant] || 'solid');

  const seenIds = new Set();
  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    if (!Array.isArray(item)) {
      throw new TypeError(`Tabs.items[${i}]: TabItem must be a FieldMap`);
    }
    assertOnlyKnownFields(item, TAB_ITEM_KEYS, `Tabs.items[${i}] (TabItem)`);
    const itemId = requireNonEmptyString(ctx.readField(item, 0), `Tabs.items[${i}].id`);
    if (seenIds.has(itemId)) {
      throw new TypeError(`Tabs.items: duplicate id '${itemId}'`);
    }
    seenIds.add(itemId);
    const labelBind = ctx.readField(item, 1);
    if (labelBind == null) {
      throw new TypeError(`Tabs.items[${i}].label must be BindRef`);
    }
    const locked = requireBool(ctx.readField(item, 4) ?? false, `Tabs.items[${i}].locked`);
    const iconName = iconRefName(ctx.readField(item, 2) ?? null);
    const badgeSrc = badgeSource(ctx.readField(item, 3) ?? null);
    const templateId = ctx.readField(item, 5) ?? null;
    if (templateId != null) requireString(templateId, `Tabs.items[${i}].content_template_id`);

    const tfTab = document.createElement('tf-tab');
    tfTab.id = itemId;
    if (iconName) tfTab.setAttribute('icon', iconName);
    if (locked) tfTab.setAttribute('disabled', '');
    if (templateId != null) tfTab.setAttribute('data-content-template-id', templateId);

    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      const text = v == null ? '' : String(v);
      if (tfTab._btn) {
        tfTab._btn._label = text;
        tfTab._update();
      } else {
        tfTab.textContent = text;
      }
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));

    // InlineBadge count/label is a BindRef — subscribe so a store update to the
    // bound value updates the tab's count attribute reactively.
    if (badgeSrc != null) {
      const applyBadge = () => {
        const text = badgeDisplay(badgeSrc, ctx.store);
        if (text == null) tfTab.removeAttribute('count');
        else tfTab.setAttribute('count', text);
      };
      applyBadge();
      if (badgeSrc.bind != null) {
        ctx.registerCleanup(subscribeBindRef(badgeSrc.bind, ctx.store, applyBadge));
      }
    }

    tfTabs.appendChild(tfTab);
  }

  // active tab follows active_id reactively.
  const applyActive = () => {
    const activeId = resolveBindRef(activeIdBind, ctx.store);
    if (activeId != null) tfTabs.value = String(activeId);
  };
  applyActive();
  ctx.registerCleanup(subscribeBindRef(activeIdBind, ctx.store, applyActive));

  // tf-tabs emits `change` { value } — convert to the SDK `select` { item_id }.
  // ComponentRenderer installs the addon's `select` handler on the renderer root
  // (the returned container), so the non-bubbling event must be dispatched there,
  // not on the nested tf-tabs. __tfReemit keeps the conversion idempotent.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    const selectedId = e.detail && e.detail.value;
    if (!selectedId) return;
    e.stopImmediatePropagation();
    const ce = new (globalThis.CustomEvent || globalThis.Event)('select', {
      bubbles: false,
      detail: { item_id: selectedId },
    });
    ce.__tfReemit = true;
    root.dispatchEvent(ce);
  };
  tfTabs.addEventListener('change', onChange);
  ctx.registerCleanup(() => tfTabs.removeEventListener('change', onChange));

  root.appendChild(tfTabs);

  // The active tab's panel content lives in a single content slot the
  // SlotManager fills/swaps as the addon pushes SlotContent for it.
  const content = document.createElement('div');
  content.classList.add('tf-tabs-content');
  content.setAttribute('data-slot-id', contentSlot);
  root.appendChild(content);

  return root;
}

// =============================================================================
// Registration
// =============================================================================

export function registerLayoutSidebarTabsRenderers() {
  if (!lookupComponentRenderer(SIDEBAR_TAG)) {
    registerComponentRenderer(SIDEBAR_TAG, renderSidebar);
  }
  if (!lookupComponentRenderer(TABS_TAG)) {
    registerComponentRenderer(TABS_TAG, renderTabs);
  }
}
