// =============================================================================
// File: sdk-runtime/molecule-page-renderer.js
// Description: Renderers for page-level molecule components: Header (0x0001),
// PageHeader (0x0002), SectionHeader (0x0004), Toolbar (0x0005),
// StatGroup (0x000A), Inspector (0x000C) — chunk 3.3f.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/molecules/page.rs,
//           tentaflow-sdk-spec/src/protocol/ui/molecules/sections.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireU8, requireString,
  assertOnlyKnownFields,
} from './data-chart-shared.js';
import { renderIcon } from './icon-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';
import { STAT_CARD_TAG } from './data-stat-labels-renderer.js';
import { SEGMENTED_CONTROL_TAG } from './action-bars-renderer.js';
import { SELECT_TAG } from './form-select-renderer.js';

const DENSITIES = new Set(['default', 'compact', 'comfortable']);
const BADGE_VARIANTS = new Set(['dot', 'count', 'text', 'status', 'icon']);
const CHIP_VARIANTS = new Set(['solid', 'soft', 'outline', 'removable', 'selectable', 'toggle']);
const SPACINGS = new Set(['zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl']);

// SearchBox tag — 0x0307 per spec, no JS renderer export exists yet.
const SEARCHBOX_TAG = 0x0307;

function assertComponentTag(c, expectedTag, parent, field) {
  if (!c || typeof c !== 'object' || Array.isArray(c)) {
    throw new TypeError(`${parent}.${field}: expected Component`);
  }
  if (c.tag !== expectedTag) {
    throw new TypeError(
      `${parent}.${field}: expected tag 0x${expectedTag.toString(16)}, got 0x${(c.tag || 0).toString(16)}`
    );
  }
}

function assertComponentArrayTag(arr, expectedTag, parent, field) {
  if (!Array.isArray(arr)) return;
  for (const c of arr) assertComponentTag(c, expectedTag, parent, field);
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function renderInlineBadge(raw, ctx) {
  if (raw == null || typeof raw !== 'object') return null;
  const el = document.createElement('span');
  el.classList.add('tf-inline-badge');
  const variant = typeof raw[0] === 'string' ? raw[0] : 'dot';
  const tone = typeof raw[1] === 'string' ? raw[1] : 'neutral';
  el.classList.add(`tf-inline-badge--variant-${variant}`);
  el.classList.add(`tf-inline-badge--tone-${tone}`);
  const label = raw[2];
  if (label != null) {
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-inline-badge__label');
    applyTextBind(labelEl, label, ctx);
    el.appendChild(labelEl);
  }
  const count = raw[3];
  if (count != null) {
    const countEl = document.createElement('span');
    countEl.classList.add('tf-inline-badge__count');
    applyTextBind(countEl, count, ctx);
    el.appendChild(countEl);
  }
  const icon = raw[4];
  if (icon != null) {
    const iconEl = renderIcon(icon, 'InlineBadge.icon');
    iconEl.classList.add('tf-inline-badge__icon');
    el.appendChild(iconEl);
  }
  if (raw[5] === true) el.classList.add('tf-inline-badge--pulse');
  return el;
}

function renderInlineChip(raw, ctx) {
  if (raw == null || typeof raw !== 'object') return null;
  const el = document.createElement('span');
  el.classList.add('tf-inline-chip');
  const variant = typeof raw[0] === 'string' ? raw[0] : 'solid';
  const tone = typeof raw[1] === 'string' ? raw[1] : 'neutral';
  el.classList.add(`tf-inline-chip--variant-${variant}`);
  el.classList.add(`tf-inline-chip--tone-${tone}`);
  const label = raw[2];
  if (label != null) {
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-inline-chip__label');
    applyTextBind(labelEl, label, ctx);
    el.appendChild(labelEl);
  }
  const icon = raw[3];
  if (icon != null) {
    const iconEl = renderIcon(icon, 'InlineChip.icon');
    iconEl.classList.add('tf-inline-chip__icon');
    el.appendChild(iconEl);
  }
  if (raw[6] === true) el.classList.add('tf-inline-chip--removable');
  return el;
}

function renderNavTabs(tabs, ctx, parent) {
  if (!Array.isArray(tabs) || tabs.length === 0) return null;
  const nav = document.createElement('nav');
  nav.classList.add('tf-molecule-tabs');
  nav.setAttribute('role', 'tablist');
  for (const tab of tabs) {
    if (tab == null || typeof tab !== 'object') continue;
    const btn = document.createElement('button');
    btn.classList.add('tf-molecule-tabs__tab');
    btn.setAttribute('role', 'tab');
    btn.type = 'button';
    const id = typeof tab[0] === 'string' ? tab[0] : '';
    btn.dataset.tabId = id;
    const label = tab[1];
    if (label != null) {
      const labelEl = document.createElement('span');
      labelEl.classList.add('tf-molecule-tabs__label');
      applyTextBind(labelEl, label, ctx);
      btn.appendChild(labelEl);
    }
    const icon = tab[2];
    if (icon != null) {
      const iconEl = renderIcon(icon, `${parent}.tabs.icon`);
      iconEl.classList.add('tf-molecule-tabs__icon');
      btn.prepend(iconEl);
    }
    const badge = tab[3];
    if (badge != null) {
      const badgeEl = renderInlineBadge(badge, ctx);
      if (badgeEl) btn.appendChild(badgeEl);
    }
    if (tab[5] === true) {
      btn.classList.add('tf-molecule-tabs__tab--locked');
      btn.disabled = true;
    }
    nav.appendChild(btn);
  }
  return nav;
}

function renderBreadcrumbs(crumbs, ctx) {
  if (!Array.isArray(crumbs) || crumbs.length === 0) return null;
  const nav = document.createElement('nav');
  nav.classList.add('tf-molecule-breadcrumbs');
  nav.setAttribute('aria-label', 'Breadcrumb');
  const ol = document.createElement('ol');
  ol.classList.add('tf-molecule-breadcrumbs__list');
  for (let i = 0; i < crumbs.length; i++) {
    const item = crumbs[i];
    if (item == null || typeof item !== 'object') continue;
    if (i > 0) {
      const sep = document.createElement('li');
      sep.classList.add('tf-molecule-breadcrumbs__separator');
      sep.setAttribute('aria-hidden', 'true');
      sep.textContent = '/';
      ol.appendChild(sep);
    }
    const li = document.createElement('li');
    li.classList.add('tf-molecule-breadcrumbs__item');
    const isCurrent = item[4] === true;
    const label = item[0];
    const icon = item[1];
    const el = document.createElement('span');
    el.classList.add('tf-molecule-breadcrumbs__link');
    if (!isCurrent) {
      el.setAttribute('role', 'link');
      el.tabIndex = 0;
    } else {
      li.classList.add('tf-molecule-breadcrumbs__item--current');
      el.setAttribute('aria-current', 'page');
    }
    if (icon != null) {
      const iconEl = renderIcon(icon, 'BreadcrumbItem.icon');
      iconEl.classList.add('tf-molecule-breadcrumbs__icon');
      el.appendChild(iconEl);
    }
    if (label != null) {
      const labelEl = document.createElement('span');
      applyTextBind(labelEl, label, ctx);
      el.appendChild(labelEl);
    }
    li.appendChild(el);
    ol.appendChild(li);
  }
  nav.appendChild(ol);
  return nav;
}

function renderFilterChips(filters, ctx) {
  if (!Array.isArray(filters) || filters.length === 0) return null;
  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-molecule-filters');
  for (const f of filters) {
    if (f == null || typeof f !== 'object') continue;
    const chip = document.createElement('button');
    chip.classList.add('tf-molecule-filters__chip');
    chip.type = 'button';
    const id = typeof f[0] === 'string' ? f[0] : '';
    chip.dataset.filterId = id;
    const label = f[1];
    if (label != null) {
      const labelEl = document.createElement('span');
      applyTextBind(labelEl, label, ctx);
      chip.appendChild(labelEl);
    }
    const icon = f[2];
    if (icon != null) {
      const iconEl = renderIcon(icon, 'FilterChipDef.icon');
      chip.prepend(iconEl);
    }
    wrapper.appendChild(chip);
  }
  return wrapper;
}

// =============================================================================
// Header (0x0001) — 7 fields
// =============================================================================

export const HEADER_TAG = 0x0001;
const HEADER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderHeader(component, ctx) {
  assertOnlyKnownFields(component.fields, HEADER_FIELD_KEYS, 'Header');

  const iconRaw = ctx.readField(component.fields, 0);
  if (iconRaw == null) throw new TypeError('Header.icon is required (IconRef)');
  const titleBind = ctx.readField(component.fields, 1);
  if (titleBind == null) throw new TypeError('Header.title is required (BindRef)');
  assertBindRef(titleBind, 'Header.title');
  const statusBadgeRaw = ctx.readField(component.fields, 2);
  const subtitleBind = ctx.readField(component.fields, 3);
  if (subtitleBind != null) assertBindRef(subtitleBind, 'Header.subtitle');
  const metaChipsRaw = ctx.readField(component.fields, 4) || [];
  const actionsRaw = ctx.readField(component.fields, 5) || [];
  const densityRaw = ctx.readField(component.fields, 6);
  const density = densityRaw == null ? 'default' : requireEnum(densityRaw, DENSITIES, 'Header.density');

  assertComponentArrayTag(actionsRaw, BUTTON_TAG, 'Header', 'actions');

  const wrapper = document.createElement('header');
  wrapper.classList.add('tf-header', `tf-header--density-${density}`);

  const iconEl = renderIcon(iconRaw, 'Header.icon');
  iconEl.classList.add('tf-header__icon');
  wrapper.appendChild(iconEl);

  const content = document.createElement('div');
  content.classList.add('tf-header__content');

  const titleRow = document.createElement('div');
  titleRow.classList.add('tf-header__title-row');

  const titleEl = document.createElement('h1');
  titleEl.classList.add('tf-header__title');
  applyTextBind(titleEl, titleBind, ctx);
  titleRow.appendChild(titleEl);

  if (statusBadgeRaw != null) {
    const badgeEl = renderInlineBadge(statusBadgeRaw, ctx);
    if (badgeEl) {
      badgeEl.classList.add('tf-header__badge');
      titleRow.appendChild(badgeEl);
    }
  }
  content.appendChild(titleRow);

  if (subtitleBind != null) {
    const subtitleEl = document.createElement('p');
    subtitleEl.classList.add('tf-header__subtitle');
    applyTextBind(subtitleEl, subtitleBind, ctx);
    content.appendChild(subtitleEl);
  }

  if (Array.isArray(metaChipsRaw) && metaChipsRaw.length > 0) {
    const chipsRow = document.createElement('div');
    chipsRow.classList.add('tf-header__chips');
    for (const chipRaw of metaChipsRaw) {
      const chipEl = renderInlineChip(chipRaw, ctx);
      if (chipEl) chipsRow.appendChild(chipEl);
    }
    content.appendChild(chipsRow);
  }

  wrapper.appendChild(content);

  if (Array.isArray(actionsRaw) && actionsRaw.length > 0) {
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-header__actions');
    for (const actionComp of actionsRaw) {
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    wrapper.appendChild(actionsEl);
  }

  return wrapper;
}

// =============================================================================
// PageHeader (0x0002) — 5 fields
// =============================================================================

export const PAGE_HEADER_TAG = 0x0002;
const PAGE_HEADER_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderPageHeader(component, ctx) {
  assertOnlyKnownFields(component.fields, PAGE_HEADER_FIELD_KEYS, 'PageHeader');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) throw new TypeError('PageHeader.title is required (BindRef)');
  assertBindRef(titleBind, 'PageHeader.title');
  const subtitleBind = ctx.readField(component.fields, 1);
  if (subtitleBind != null) assertBindRef(subtitleBind, 'PageHeader.subtitle');
  const breadcrumbsRaw = ctx.readField(component.fields, 2);
  const actionsRaw = ctx.readField(component.fields, 3) || [];
  const tabsRaw = ctx.readField(component.fields, 4);

  assertComponentArrayTag(actionsRaw, BUTTON_TAG, 'PageHeader', 'actions');

  const wrapper = document.createElement('header');
  wrapper.classList.add('tf-page-header');

  if (breadcrumbsRaw != null) {
    const breadcrumbsEl = renderBreadcrumbs(breadcrumbsRaw, ctx);
    if (breadcrumbsEl) wrapper.appendChild(breadcrumbsEl);
  }

  const titleRow = document.createElement('div');
  titleRow.classList.add('tf-page-header__title-row');

  const titles = document.createElement('div');
  titles.classList.add('tf-page-header__titles');

  const titleEl = document.createElement('h1');
  titleEl.classList.add('tf-page-header__title');
  applyTextBind(titleEl, titleBind, ctx);
  titles.appendChild(titleEl);

  if (subtitleBind != null) {
    const subtitleEl = document.createElement('p');
    subtitleEl.classList.add('tf-page-header__subtitle');
    applyTextBind(subtitleEl, subtitleBind, ctx);
    titles.appendChild(subtitleEl);
  }
  titleRow.appendChild(titles);

  if (Array.isArray(actionsRaw) && actionsRaw.length > 0) {
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-page-header__actions');
    for (const actionComp of actionsRaw) {
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    titleRow.appendChild(actionsEl);
  }

  wrapper.appendChild(titleRow);

  if (tabsRaw != null) {
    const tabsEl = renderNavTabs(tabsRaw, ctx, 'PageHeader');
    if (tabsEl) wrapper.appendChild(tabsEl);
  }

  return wrapper;
}

// =============================================================================
// SectionHeader (0x0004) — 4 fields
// =============================================================================

export const SECTION_HEADER_TAG = 0x0004;
const SECTION_HEADER_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderSectionHeader(component, ctx) {
  assertOnlyKnownFields(component.fields, SECTION_HEADER_FIELD_KEYS, 'SectionHeader');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) throw new TypeError('SectionHeader.title is required (BindRef)');
  assertBindRef(titleBind, 'SectionHeader.title');
  const subtitleBind = ctx.readField(component.fields, 1);
  if (subtitleBind != null) assertBindRef(subtitleBind, 'SectionHeader.subtitle');
  const actionsRaw = ctx.readField(component.fields, 2) || [];
  const divider = requireBool(ctx.readField(component.fields, 3), 'SectionHeader.divider');

  assertComponentArrayTag(actionsRaw, BUTTON_TAG, 'SectionHeader', 'actions');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-section-header');

  const row = document.createElement('div');
  row.classList.add('tf-section-header__row');

  const titles = document.createElement('div');
  titles.classList.add('tf-section-header__titles');

  const titleEl = document.createElement('h2');
  titleEl.classList.add('tf-section-header__title');
  applyTextBind(titleEl, titleBind, ctx);
  titles.appendChild(titleEl);

  if (subtitleBind != null) {
    const subtitleEl = document.createElement('p');
    subtitleEl.classList.add('tf-section-header__subtitle');
    applyTextBind(subtitleEl, subtitleBind, ctx);
    titles.appendChild(subtitleEl);
  }
  row.appendChild(titles);

  if (Array.isArray(actionsRaw) && actionsRaw.length > 0) {
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-section-header__actions');
    for (const actionComp of actionsRaw) {
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    row.appendChild(actionsEl);
  }

  wrapper.appendChild(row);

  if (divider) {
    const div = document.createElement('hr');
    div.classList.add('tf-section-header__divider');
    wrapper.appendChild(div);
  }

  return wrapper;
}

// =============================================================================
// Toolbar (0x0005) — 6 fields
// =============================================================================

export const TOOLBAR_TAG = 0x0005;
const TOOLBAR_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderToolbar(component, ctx) {
  assertOnlyKnownFields(component.fields, TOOLBAR_FIELD_KEYS, 'Toolbar');

  const searchRaw = ctx.readField(component.fields, 0);
  const filtersRaw = ctx.readField(component.fields, 1) || [];
  const viewModeRaw = ctx.readField(component.fields, 2);
  const sortControlRaw = ctx.readField(component.fields, 3);
  const trailingActionsRaw = ctx.readField(component.fields, 4) || [];
  const density = requireEnum(ctx.readField(component.fields, 5), DENSITIES, 'Toolbar.density');

  if (searchRaw != null) {
    assertComponentTag(searchRaw, SEARCHBOX_TAG, 'Toolbar', 'search');
  }
  if (viewModeRaw != null) {
    assertComponentTag(viewModeRaw, SEGMENTED_CONTROL_TAG, 'Toolbar', 'view_mode');
  }
  if (sortControlRaw != null) {
    assertComponentTag(sortControlRaw, SELECT_TAG, 'Toolbar', 'sort_control');
  }
  assertComponentArrayTag(trailingActionsRaw, BUTTON_TAG, 'Toolbar', 'trailing_actions');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-toolbar', `tf-toolbar--density-${density}`);
  wrapper.setAttribute('role', 'toolbar');

  if (searchRaw != null) {
    const searchEl = ctx.renderChild(searchRaw);
    searchEl.classList.add('tf-toolbar__search');
    wrapper.appendChild(searchEl);
  }

  const filtersEl = renderFilterChips(filtersRaw, ctx);
  if (filtersEl) {
    filtersEl.classList.add('tf-toolbar__filters');
    wrapper.appendChild(filtersEl);
  }

  if (viewModeRaw != null) {
    const vmEl = ctx.renderChild(viewModeRaw);
    vmEl.classList.add('tf-toolbar__view-mode');
    wrapper.appendChild(vmEl);
  }

  if (sortControlRaw != null) {
    const sortEl = ctx.renderChild(sortControlRaw);
    sortEl.classList.add('tf-toolbar__sort');
    wrapper.appendChild(sortEl);
  }

  if (Array.isArray(trailingActionsRaw) && trailingActionsRaw.length > 0) {
    const trailing = document.createElement('div');
    trailing.classList.add('tf-toolbar__trailing');
    for (const actionComp of trailingActionsRaw) {
      trailing.appendChild(ctx.renderChild(actionComp));
    }
    wrapper.appendChild(trailing);
  }

  return wrapper;
}

// =============================================================================
// StatGroup (0x000A) — 3 fields
// =============================================================================

export const STAT_GROUP_TAG = 0x000A;
const STAT_GROUP_FIELD_KEYS = new Set([0, 1, 2]);

function renderStatGroup(component, ctx) {
  assertOnlyKnownFields(component.fields, STAT_GROUP_FIELD_KEYS, 'StatGroup');

  const statsRaw = ctx.readField(component.fields, 0) || [];
  const columnsRaw = ctx.readField(component.fields, 1);
  const density = requireEnum(ctx.readField(component.fields, 2), DENSITIES, 'StatGroup.density');

  assertComponentArrayTag(statsRaw, STAT_CARD_TAG, 'StatGroup', 'stats');

  const columns = columnsRaw == null ? statsRaw.length : requireU8(columnsRaw, 'StatGroup.columns');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-stat-group', `tf-stat-group--density-${density}`);
  wrapper.style.gridTemplateColumns = `repeat(${columns}, 1fr)`;

  for (const statComp of statsRaw) {
    const el = ctx.renderChild(statComp);
    el.classList.add('tf-stat-group__item');
    wrapper.appendChild(el);
  }

  return wrapper;
}

// =============================================================================
// Inspector (0x000C) — 5 fields
// =============================================================================

export const INSPECTOR_TAG = 0x000C;
const INSPECTOR_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderInspector(component, ctx) {
  assertOnlyKnownFields(component.fields, INSPECTOR_FIELD_KEYS, 'Inspector');

  const titleBind = ctx.readField(component.fields, 0);
  if (titleBind == null) throw new TypeError('Inspector.title is required (BindRef)');
  assertBindRef(titleBind, 'Inspector.title');
  const contentSlot = requireString(ctx.readField(component.fields, 1), 'Inspector.content_slot');
  const actionsRaw = ctx.readField(component.fields, 2) || [];
  const tabsRaw = ctx.readField(component.fields, 3);
  const collapsible = requireBool(ctx.readField(component.fields, 4), 'Inspector.collapsible');

  assertComponentArrayTag(actionsRaw, BUTTON_TAG, 'Inspector', 'actions');

  const wrapper = document.createElement('aside');
  wrapper.classList.add('tf-inspector');
  if (collapsible) wrapper.classList.add('tf-inspector--collapsible');

  const header = document.createElement('div');
  header.classList.add('tf-inspector__header');

  const titleEl = document.createElement('h2');
  titleEl.classList.add('tf-inspector__title');
  applyTextBind(titleEl, titleBind, ctx);
  header.appendChild(titleEl);

  if (Array.isArray(actionsRaw) && actionsRaw.length > 0) {
    const actionsEl = document.createElement('div');
    actionsEl.classList.add('tf-inspector__actions');
    for (const actionComp of actionsRaw) {
      actionsEl.appendChild(ctx.renderChild(actionComp));
    }
    header.appendChild(actionsEl);
  }

  if (collapsible) {
    const toggle = document.createElement('button');
    toggle.classList.add('tf-inspector__toggle');
    toggle.type = 'button';
    toggle.setAttribute('aria-label', 'Toggle inspector');
    toggle.textContent = '▸';
    toggle.addEventListener('click', () => {
      const collapsed = wrapper.classList.toggle('tf-inspector--collapsed');
      toggle.setAttribute('aria-expanded', String(!collapsed));
    });
    toggle.setAttribute('aria-expanded', 'true');
    header.appendChild(toggle);
  }

  wrapper.appendChild(header);

  if (tabsRaw != null) {
    const tabsEl = renderNavTabs(tabsRaw, ctx, 'Inspector');
    if (tabsEl) wrapper.appendChild(tabsEl);
  }

  const body = document.createElement('div');
  body.classList.add('tf-inspector__body');
  body.setAttribute('data-slot-id', contentSlot);
  wrapper.appendChild(body);

  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================

export function registerMoleculePageRenderers() {
  if (!lookupComponentRenderer(HEADER_TAG)) {
    registerComponentRenderer(HEADER_TAG, renderHeader);
  }
  if (!lookupComponentRenderer(PAGE_HEADER_TAG)) {
    registerComponentRenderer(PAGE_HEADER_TAG, renderPageHeader);
  }
  if (!lookupComponentRenderer(SECTION_HEADER_TAG)) {
    registerComponentRenderer(SECTION_HEADER_TAG, renderSectionHeader);
  }
  if (!lookupComponentRenderer(TOOLBAR_TAG)) {
    registerComponentRenderer(TOOLBAR_TAG, renderToolbar);
  }
  if (!lookupComponentRenderer(STAT_GROUP_TAG)) {
    registerComponentRenderer(STAT_GROUP_TAG, renderStatGroup);
  }
  if (!lookupComponentRenderer(INSPECTOR_TAG)) {
    registerComponentRenderer(INSPECTOR_TAG, renderInspector);
  }
}
