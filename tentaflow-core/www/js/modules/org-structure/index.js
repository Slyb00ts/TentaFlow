// =============================================================================
// File: modules/org-structure/index.js — organization structure screen
//
// One route (`#/org-structure`, deep-linkable as `?tab=list|visibility|history`)
// over MessageBody::OrgStructureBody (binary protocol only, codec org* helpers).
// Five tabs, in the order of the mockups: Drzewo, Lista i import, Widoczność,
// Historia and Katalog ról — the last one is its own screen and the tab only
// navigates there, as the role catalog's own strip navigates back here.
//
// This is the foundation the tree, list/import, visibility and history work
// builds on: every tab already reads the real structure through the wire.
// Drzewo and Lista show what the structure holds (counts, units, positions);
// Historia reads it as of a chosen day; Widoczność states the read rule.
// Everyone in the organization may read; the server refuses writes without
// `org.admin`, and `my_permissions` from the answer is what a later editing
// surface will consult — nothing mutating is drawn from a guess.
// tf-* components only; every visible string comes from i18n org_structure.*.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { formatDay } from '/js/lib/date-format.js';
import { Router } from '/js/router.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-detail-header.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import {
  summarize, warningText,
} from '/js/modules/org-structure/model.js';
import { canLeaveTree, mountTreeTab, refreshTreeTab, unmountTreeTab } from '/js/modules/org-structure/tree-tab.js';
import '/js/modules/org-structure/edit-mode.js';
import { mountListTab, refreshListTab, unmountListTab } from '/js/modules/org-structure/list-tab.js';
import { mountHistoryTab, refreshHistoryTab, unmountHistoryTab } from '/js/modules/org-structure/history-tab.js';
import { mountVisibilityTab, refreshVisibilityTab, unmountVisibilityTab } from '/js/modules/org-structure/visibility-tab.js';
import { mountHandoverTab, unmountHandoverTab } from '/js/modules/org-structure/handover-tab.js';
import { handoverTarget, openHandover } from '/js/modules/org-structure/handover-nav.js';
import { openActionMenu } from '/js/lib/actions/index.js';

const TABS = [
  { id: 'tree', icon: 'sitemap' },
  { id: 'list', icon: 'list' },
  { id: 'visibility', icon: 'eye' },
  { id: 'history', icon: 'history' },
  { id: 'roles', icon: 'tag' },
];

// `roles` is another screen, so it never is the tab this screen shows.
const OWN_TABS = TABS.filter((tab) => tab.id !== 'roles').map((tab) => tab.id);

const state = {
  tab: 'tree',
  view: null,
  unitTypes: [],
  myPermissions: [],
  historyMounted: false,
  historyShown: false,
  treeMounted: false,
  treeShown: false,
  listMounted: false,
  listShown: false,
  visibilityMounted: false,
  // "Do przekazania" is a mode of the list tab (route parameters `handover`, `reason`, `project`).
  handover: null,
  handoverMounted: false,
  // People whose assignment ended and who still hold work; only an administrator asks.
  pending: [],
};

function t(key, params) {
  return I18n.t(`org_structure.${key}`, params);
}

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

// =============================================================================
// Loading
// =============================================================================

async function fetchStructure(at) {
  const body = await ApiBinary.one('orgStructureRequest', at ? { at } : {});
  return {
    view: body.view,
    unitTypes: body.unit_types ?? [],
    myPermissions: body.my_permissions ?? [],
  };
}

// =============================================================================
// Rendering
// =============================================================================

function warningsBlock(view) {
  if (!view.warnings?.length) return '';
  const items = view.warnings
    .map((w) => `<li>${escapeHtml(warningText(w, view, t))}</li>`)
    .join('');
  return `
    <div class="org-section-title">${escapeHtml(t('section_warnings'))}</div>
    <ul class="org-warnings">${items}</ul>`;
}

function emptyState() {
  return `<tf-empty-state icon="sitemap" title="${escapeAttr(t('empty_title'))}"
    message="${escapeAttr(t('empty_message'))}"></tf-empty-state>`;
}

function isEmpty(view) {
  return (view.units ?? []).length === 0 && (view.positions ?? []).length === 0;
}

function renderHeader() {
  const header = byId('org-header');
  if (!header || !state.view) return;
  const s = summarize(state.view);
  const roots = state.view.units.filter((u) => !u.parent_unit_id);
  const parts = [
    roots.length === 1 ? roots[0].name : '',
    t('people_count', { count: s.people }),
    t('summary_units', { count: s.units }),
    t('header_today'),
  ].filter(Boolean);
  header.setAttribute('subtitle', parts.join(' · '));
  byId('org-header-badges').innerHTML = `${pendingButton()}
    <tf-chip variant="outline" icon="users">${escapeHtml(t('people_count', { count: s.people }))}</tf-chip>
    <tf-chip variant="outline" icon="sitemap">${escapeHtml(t('summary_units', { count: s.units }))}</tf-chip>
    ${s.vacancies > 0 ? `<tf-chip variant="outline" status="warn" icon="user">${escapeHtml(t('vacancies_count', { count: s.vacancies }))}</tf-chip>` : ''}
    <tf-chip variant="outline" status="info" icon="calendar">${escapeHtml(t('as_of_today', { date: formatDay(state.view.at) }))}</tf-chip>`;
}

// The counter of people who still hold work after their assignment ended (docs §2.6): drawn only for an
// administrator and only when there is somebody, because the server answers it for administrators alone.
function pendingButton() {
  if (!state.pending.length) return '';
  return `<tf-button size="sm" variant="secondary" icon="send" data-act="handover-pending">${escapeHtml(t('handover.pending_button', { count: state.pending.length }))}</tf-button>`;
}

async function refreshPending() {
  if (!state.myPermissions.includes('org.admin')) {
    state.pending = [];
    return;
  }
  try {
    state.pending = (await ApiBinary.one('orgHandoverPendingRequest', {})).people ?? [];
  } catch {
    // The counter is a convenience; the screen does not depend on it.
    state.pending = [];
  }
  renderHeader();
}

function openPendingMenu(anchor) {
  if (state.pending.length === 1) {
    openHandover({ userId: state.pending[0].user_id, reason: 'departure' });
    return;
  }
  openActionMenu(anchor, state.pending.map((person) => ({
    label: t('handover.pending_item', { name: person.display_name || '—', count: person.count }),
    icon: 'user',
    run: () => openHandover({ userId: person.user_id, reason: 'departure' }),
  })), t('handover.pending_title'));
}

function treeInput() {
  return { view: state.view, unitTypes: state.unitTypes, myPermissions: state.myPermissions };
}

function renderTree() {
  const host = byId('org-panel-tree');
  if (!host || !state.view) return;
  // An administrator of an empty structure gets the chart anyway: its empty state is where a template is loaded.
  if (isEmpty(state.view) && !state.myPermissions.includes('org.admin')) {
    if (state.treeMounted) unmountTreeTab();
    state.treeMounted = false;
    host.innerHTML = emptyState();
    return;
  }
  if (state.treeMounted) {
    // The chart keeps its zoom, position and selection while another tab is shown; it only takes the new data.
    refreshTreeTab(treeInput());
    return;
  }
  state.treeMounted = true;
  host.innerHTML = `<div id="org-tree-host"></div>${warningsBlock(state.view)}`;
  mountTreeTab(byId('org-tree-host'), treeInput(), { changed: reloadStructure }).catch((err) => {
    const target = byId('org-tree-host');
    if (target) target.textContent = t('load_failed', { message: err.message || '' });
  });
}

// Every time the tab is shown the structure is read again: another admin (or the edit mode) may have
// changed it, and the server publishes no dashboard push for it.
async function activateTree() {
  if (state.treeShown) {
    try {
      Object.assign(state, await fetchStructure(null));
      renderHeader();
    } catch {
      // Keep what is on screen; the next activation tries again.
    }
  }
  state.treeShown = true;
  renderTree();
}

// The list draws even an empty structure: importing a file into it is how a structure begins.
function renderList() {
  const host = byId('org-panel-list');
  if (!host || !state.view) return;
  if (state.listMounted) {
    refreshListTab(treeInput());
    return;
  }
  state.listMounted = true;
  mountListTab(host, treeInput(), { reload: reloadStructure });
}

// Like the tree, the list reads the structure again each time it is shown: another admin may have changed it.
async function activateList() {
  if (state.listShown) {
    try {
      Object.assign(state, await fetchStructure(null));
      renderHeader();
    } catch {
      // Keep what is on screen; the next activation tries again.
    }
  }
  state.listShown = true;
  renderList();
}

// After a write or an import: the header, the list and (when it is drawn) the chart show the new structure.
async function reloadStructure() {
  Object.assign(state, await fetchStructure(null));
  renderHeader();
  refreshPending();
  renderList();
  if (state.treeMounted) refreshTreeTab(treeInput());
}

// Widoczność is drawn from the server's own answers, so every time the tab is shown they are read again.
function renderVisibility() {
  const host = byId('org-panel-visibility');
  if (!host || !state.view) return;
  const input = { view: state.view, myPermissions: state.myPermissions };
  if (state.visibilityMounted) {
    refreshVisibilityTab(input);
    return;
  }
  state.visibilityMounted = true;
  mountVisibilityTab(host, input).catch((err) => {
    host.textContent = t('load_failed', { message: err.message || '' });
  });
}

function renderHistory() {
  const host = byId('org-panel-history');
  if (!host || !state.view) return;
  if (state.historyMounted) {
    refreshHistoryTab(treeInput());
    return;
  }
  state.historyMounted = true;
  mountHistoryTab(host, treeInput(), { reload: reloadStructure }).catch((err) => {
    host.textContent = t('load_failed', { message: err.message || '' });
  });
}

// Like the tree and the list, the tab reads today's structure again each time it is shown: another admin may have approved a reorganization.
async function activateHistory() {
  if (state.historyShown) {
    try {
      Object.assign(state, await fetchStructure(null));
      renderHeader();
    } catch {
      // Keep what is on screen; the next activation tries again.
    }
  }
  state.historyShown = true;
  renderHistory();
}

const RENDERERS = {
  tree: activateTree,
  list: activateList,
  visibility: renderVisibility,
  history: activateHistory,
};

function renderHandover() {
  const host = byId('org-panel-handover');
  if (!host || !state.handover) return;
  state.handoverMounted = true;
  mountHandoverTab(host, {
    target: state.handover,
    isAdmin: state.myPermissions.includes('org.admin'),
    onBack: () => Router.navigate('org-structure', { tab: 'list' }),
    onChanged: () => reloadStructure().catch(() => {}),
  }).catch((err) => {
    host.textContent = t('load_failed', { message: err.message || '' });
  });
}

function showTab(tab) {
  state.tab = tab;
  // The handover screen takes the place of the list while it is asked for.
  const inHandover = tab === 'list' && Boolean(state.handover);
  for (const id of OWN_TABS) {
    const panel = byId(`org-panel-${id}`);
    if (panel) panel.hidden = id !== tab || (id === 'list' && inHandover);
  }
  const handoverPanel = byId('org-panel-handover');
  if (handoverPanel) handoverPanel.hidden = !inHandover;
  if (inHandover) {
    renderHandover();
  } else if (state.handoverMounted) {
    unmountHandoverTab();
    state.handoverMounted = false;
  }
  const actions = byId('org-header-actions');
  if (actions) actions.hidden = tab !== 'tree';
  const listActions = byId('org-list-actions');
  if (listActions) listActions.hidden = tab !== 'list' || inHandover;
  // On a narrow screen the strip scrolls; the tab of this screen must be in view.
  requestAnimationFrame(() => {
    byId('org-tabs')?.querySelector(`tf-tab[id="${tab}"]`)?.scrollIntoView({ inline: 'nearest', block: 'nearest' });
  });
  RENDERERS[tab]?.();
}

// =============================================================================
// Events
// =============================================================================

function onTabChange(tab) {
  if (tab === 'roles') {
    Router.navigate('roles-catalog');
    return;
  }
  // Choosing "Lista i import" while the handover screen is open leaves it for the list.
  if (!OWN_TABS.includes(tab) || (tab === state.tab && !state.handover)) return;
  state.handover = null;
  Router.replaceParams(tab === 'tree' ? null : { tab });
  showTab(tab);
}

function onVisible() {
  if (document.visibilityState === 'visible' && state.tab === 'tree' && state.treeShown) activateTree();
}

function wire(root) {
  document.addEventListener('visibilitychange', onVisible);
  root.addEventListener('change', (e) => {
    if (e.target.id === 'org-tabs') onTabChange(String(e.detail?.value ?? ''));
  });
  root.addEventListener('click', (e) => {
    const button = e.target.closest?.('[data-act="handover-pending"]');
    if (button) openPendingMenu(button);
  });
}

// =============================================================================
// Screen
// =============================================================================

const OrgStructureScreen = {
  get title() { return t('title'); },

  render() {
    const tabs = TABS
      .map((tab) => `<tf-tab id="${tab.id}" icon="${tab.icon}">${escapeHtml(t(`tab_${tab.id}`))}</tf-tab>`)
      .join('');
    const panels = [...OWN_TABS, 'handover']
      .map((id) => `<div id="org-panel-${id}" class="org-panel" hidden></div>`)
      .join('');
    return `
      <div id="org-root">
        <tf-detail-header id="org-header" compact title="${escapeAttr(t('title'))}" subtitle="${escapeAttr(t('subtitle'))}" icon="sitemap">
          <span slot="badges" id="org-header-badges"></span>
          <span slot="actions" class="org-header-actions-host">
            <span id="org-header-actions"></span>
            <span id="org-list-actions" hidden></span>
          </span>
        </tf-detail-header>
        <div class="org-toolbar">
          <tf-tabs variant="solid" id="org-tabs" value="tree" aria-label="${escapeAttr(t('tabs_label'))}">${tabs}</tf-tabs>
        </div>
        <div id="org-loading" class="org-loading">${escapeHtml(t('loading'))}</div>
        <div id="org-panels" hidden>${panels}</div>
      </div>
    `;
  },

  async mount(params = {}) {
    const root = byId('org-root');
    if (!root) return;
    wire(root);
    state.handover = handoverTarget(params);
    const wanted = state.handover ? 'list' : String(params?.tab ?? '');
    const tab = OWN_TABS.includes(wanted) ? wanted : 'tree';
    byId('org-tabs')?.setAttribute('value', tab);

    try {
      Object.assign(state, await fetchStructure(null));
    } catch (err) {
      byId('org-loading').textContent = t('load_failed', { message: err.message || '' });
      return;
    }
    renderHeader();
    byId('org-loading').hidden = true;
    byId('org-panels').hidden = false;
    showTab(tab);
    refreshPending();
  },

  // Unsaved changes of the edit mode are asked about before the router leaves the screen.
  canUnmount() {
    return canLeaveTree();
  },

  unmount() {
    document.removeEventListener('visibilitychange', onVisible);
    unmountTreeTab();
    unmountListTab();
    unmountVisibilityTab();
    unmountHandoverTab();
    state.handover = null;
    state.handoverMounted = false;
    state.pending = [];
    state.visibilityMounted = false;
    state.treeMounted = false;
    state.treeShown = false;
    state.listMounted = false;
    state.listShown = false;
    state.tab = 'tree';
    state.view = null;
    state.unitTypes = [];
    state.myPermissions = [];
    unmountHistoryTab();
    state.historyMounted = false;
    state.historyShown = false;
  },
};

export default OrgStructureScreen;
