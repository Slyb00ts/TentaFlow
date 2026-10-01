// =============================================================================
// File: modules/org-structure/tree-tab.js
// Description: The Drzewo tab of the organization structure screen: the chart
//   (tf-org-tree) with its toolbar — search with the path from the root,
//   "Moja pozycja", persons / units, functional lines, export, presentation —
//   the path strip, the read-only detail panel and the legend. Presentation
//   mode is a stage (chart + panel, nothing else) taken fullscreen, or pinned
//   over the page when the browser refuses the Fullscreen API.
//   Nothing here writes. Editing is the edit mode (edit-mode.js): it plugs in through
//   `registerEditEntry`, which makes "Edytuj strukturę" appear for `org.admin`, and is handed
//   the chart, the inspector slot and the model through a small `api` — the chart, the model
//   and the selection stay this tab's. While it is open the structure it shows is its own
//   (the effective day), so a refresh from the screen is ignored until it closes.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeAttr, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { formatDay } from '/js/lib/date-format.js';
import '/js/components/tf-org-tree.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-button.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-avatar.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-key-value.js';
import { exportPng, exportSvg, printPages } from '/js/modules/org-structure/export.js';
import {
  buildTreeModel, initialsOf, legendUnits, pathTo, searchNodes, userIdHex,
} from '/js/modules/org-structure/tree.js';
import { treeBadges } from '/js/modules/org-structure/cover-model.js';
import { mountAsOf } from '/js/modules/org-structure/history-asof.js';

const t = (key, params) => I18n.t(`org_structure.${key}`, params);

let state = null;
let editEntry = null;

/** The edit mode announces itself here: `open()` is what "Edytuj strukturę" runs. */
export function registerEditEntry(entry) {
  editEntry = entry;
}

export function treeLabels() {
  return {
    tree: t('tree_label'),
    hint: t('tree_hint'),
    vacancy: t('vacancy'),
    badge: (kind) => t(`badge_${kind}`),
    people: (count) => t('people_count', { count }),
    vacancies: (count) => t('vacancies_count', { count }),
    span: (value) => t('span_value', { value }),
    expand: (count) => t('expand_count', { count }),
    menu: (name) => t('edit.card_menu', { name }),
    tools: {
      zoomIn: t('tool_zoom_in'),
      zoomOut: t('tool_zoom_out'),
      zoomReset: t('tool_zoom_reset'),
      fit: t('tool_fit'),
      present: t('tool_present'),
      export: t('tool_export'),
    },
  };
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

function shell(view) {
  return `
    <div id="org-edit-top" class="org-edit-top" hidden></div>
    <div class="tf-toolbar org-tree-toolbar">
      <div class="org-tree-search-wrap">
        <tf-searchbox id="org-tree-search" class="org-tree-search" placeholder="${escapeAttr(t('search_placeholder'))}"
          aria-label="${escapeAttr(t('search_label'))}"></tf-searchbox>
        <span class="org-kbd" aria-hidden="true">/</span>
      </div>
      <span id="org-tree-asof" class="org-tree-asof"></span>
      <span class="tf-toolbar-spacer"></span>
      <tf-button id="org-tree-me" variant="secondary" icon="crosshair" data-act="me">${escapeHtml(t('my_position'))}</tf-button>
      <tf-segmented id="org-tree-mode" size="sm" value="persons" aria-label="${escapeAttr(t('view_label'))}">
        <option value="persons">${escapeHtml(t('view_persons'))}</option>
        <option value="units">${escapeHtml(t('view_units'))}</option>
      </tf-segmented>
      <tf-button id="org-tree-options" variant="secondary" icon="filter" data-act="options">${escapeHtml(t('view_options'))}</tf-button>
    </div>
    <div id="org-tree-asof-note" class="org-tree-asof-note" hidden></div>
    <tf-menu id="org-tree-view-menu" placement="bottom-end"></tf-menu>
    <div id="org-tree-path" class="org-tree-path" hidden></div>
    <div id="org-tree-stage" class="org-stage">
      <tf-org-tree id="org-tree" class="org-stage-tree"></tf-org-tree>
      <aside id="org-tree-detail" class="org-detail" aria-label="${escapeAttr(t('detail_label'))}" tabindex="-1" hidden></aside>
      <div class="org-stage-overlay">
        <tf-chip variant="outline" status="info" icon="calendar">${escapeHtml(t('as_of_today', { date: formatDay(view.at) }))}</tf-chip>
        <tf-button variant="secondary" icon="x" data-act="present-exit">${escapeHtml(t('present_exit'))}</tf-button>
      </div>
      <tf-menu id="org-tree-export-menu" placement="bottom-end"></tf-menu>
      <div id="org-tree-empty" class="org-tree-empty" hidden></div>
    </div>
    <div id="org-edit-below" class="org-below org-edit-below" hidden></div>
    <div id="org-tree-legend" class="org-legend org-below"></div>`;
}

// The screen's header card owns these slots; the actions belong to this tab only.
function renderActions() {
  const host = byId('org-header-actions');
  if (!host) return;
  // The header card is outside this tab's root, so the actions carry their own (idempotent) listener.
  host.addEventListener('click', onClick);
  if (state.edit) {
    host.innerHTML = state.edit.actionsHtml();
    return;
  }
  host.innerHTML = `
    ${canEdit() ? `<tf-button variant="secondary" icon="edit" data-act="edit">${escapeHtml(t('edit_structure'))}</tf-button>` : ''}
    <tf-button id="org-tree-export" variant="secondary" icon="download" data-act="export">${escapeHtml(t('export'))}</tf-button>
    <tf-button id="org-tree-present" variant="primary" icon="present" data-act="present">${escapeHtml(t('present'))}</tf-button>`;
}

function canEdit() {
  return Boolean(editEntry) && state.myPermissions.includes('org.admin');
}

// The stage takes what is left of the window below the header, tabs, toolbar and path strip, minus
// the legend under it, so the whole screen fits without scrolling. Offsets are layout values: the
// screen scales in when it opens, and an on-screen rect would be measured during that animation.
function fitStageHeight() {
  const stage = byId('org-tree-stage');
  if (!stage || !state || state.presenting) return;
  let top = 0;
  for (let el = stage; el; el = el.offsetParent) top += el.offsetTop;
  // Everything under the stage (the legend, and in edit mode the people strip and the session panel).
  let below = 0;
  for (const el of state.host.querySelectorAll('.org-below')) if (!el.hidden) below += el.offsetHeight + 12;
  stage.style.setProperty('--org-stage-h', `${Math.max(state.edit ? 300 : 360, Math.floor(window.innerHeight - top - below - 24))}px`);
}

function renderLegend() {
  const items = legendUnits(state.model)
    .map((u) => `<span class="org-legend-item"><i style="--c:${escapeAttr(u.color)}"></i>${escapeHtml(u.name)} <b>${u.total}</b></span>`)
    .join('');
  byId('org-tree-legend').innerHTML = `${items}<span class="org-legend-sep"></span>`
    + `<span class="org-legend-item"><span class="org-legend-sym org-legend-sym--vacancy"></span>${escapeHtml(t('vacancy'))}</span>`
    + `<span class="org-legend-item"><span class="org-legend-sym org-legend-sym--staff"></span>${escapeHtml(t('legend_staff'))}</span>`
    + `<span class="org-legend-item">${escapeHtml(t('legend_deputy'))}</span>`;
  fitStageHeight();
}

function chipLabel(node) {
  return node.vacant ? `${node.role} — ${t('vacancy')}` : node.name;
}

function renderPath() {
  const strip = byId('org-tree-path');
  const searching = state.query !== '';
  // The inspector already says where a card sits; the strip would only cost the chart a row of height.
  if (state.edit && !searching) {
    strip.hidden = true;
    strip.innerHTML = '';
    fitStageHeight();
    return;
  }
  if (searching && state.matches.length === 0) {
    strip.hidden = false;
    strip.innerHTML = `<span class="org-path-note">${escapeHtml(t('match_none'))}</span>`;
    fitStageHeight();
    return;
  }
  if (!state.pathIds.length) {
    strip.hidden = true;
    strip.innerHTML = '';
    fitStageHeight();
    return;
  }
  const last = state.pathIds.length - 1;
  const chips = state.pathIds.map((id, i) => {
    const node = state.nodeById.get(id);
    const avatar = node.vacant ? '' : `<tf-avatar slot="lead" size="sm" initials="${escapeAttr(initialsOf(node.name))}"></tf-avatar>`;
    return `<tf-chip variant="outline" class="org-path-chip" data-node="${escapeAttr(id)}" role="button" tabindex="0"${i === last ? ' status="accent"' : ''}>${avatar}${escapeHtml(chipLabel(node))}</tf-chip>`;
  }).join('<svg class="icon org-path-sep" aria-hidden="true"><use href="#i-chevron-right"/></svg>');
  const stepper = searching && state.matches.length > 1
    ? `<span class="org-path-matches"><tf-button variant="ghost" size="sm" icon="chevron-left" data-act="match-prev" aria-label="${escapeAttr(t('match_prev'))}"></tf-button>`
      + `<span>${escapeHtml(t('match_counter', { current: state.matchIndex + 1, total: state.matches.length }))}</span>`
      + `<tf-button variant="ghost" size="sm" icon="chevron-right" data-act="match-next" aria-label="${escapeAttr(t('match_next'))}"></tf-button></span>`
    : '';
  strip.hidden = false;
  strip.setAttribute('aria-label', t('path_current'));
  strip.innerHTML = `<span class="org-path-label">${escapeHtml(t('path_label'))}</span>${chips}${stepper}`;
  fitStageHeight();
}

function holderEntries(node) {
  return node.people.map((p) => {
    const percent = Math.round(p.share * 100);
    const parts = [t('detail_share', { percent })];
    if (p.type === 'acting') parts.push(t('detail_type_acting'));
    if (p.type === 'contractor') parts.push(t('detail_type_contractor'));
    return { key: t('detail_holder'), value: `${p.name} · ${parts.join(' · ')}` };
  });
}

function positionDetail(node) {
  const manager = node.parentId ? state.nodeById.get(node.parentId) : null;
  const managerText = manager ? chipLabel(manager) : t('detail_no_manager');
  const entries = [
    { key: t('col_position'), value: node.role },
    { key: t('col_unit'), value: node.unitName },
    { key: t('col_reports_to'), value: managerText },
    { key: t('col_since'), value: formatDay(node.since) },
    ...(node.vacant ? [{ key: t('detail_holder'), value: t('detail_vacant') }] : holderEntries(node)),
  ];
  return {
    avatar: node.vacant ? '' : `<tf-avatar size="lg" initials="${escapeAttr(initialsOf(node.name))}"></tf-avatar>`,
    title: node.vacant ? `— ${t('vacancy')} —` : node.name,
    subtitle: node.role,
    entries,
  };
}

function unitDetail(unit) {
  const head = unit.headId ? state.nodeById.get(unit.headId) : null;
  const entries = [
    { key: t('col_unit'), value: unit.name },
    ...(unit.code ? [{ key: t('col_code'), value: unit.code }] : []),
    ...(unit.typeName ? [{ key: t('col_type'), value: unit.typeName }] : []),
    { key: t('col_head'), value: head ? chipLabel(head) : t('no_head') },
    { key: t('stat_people'), value: String(unit.people) },
    { key: t('stat_vacancies'), value: String(unit.vacancies) },
    { key: t('col_positions'), value: String(unit.memberIds.length) },
    ...(unit.span != null ? [{ key: t('detail_span'), value: String(unit.span) }] : []),
  ];
  return { avatar: '', title: unit.name, subtitle: unit.typeName || unit.code, entries };
}

function renderDetail() {
  if (state.edit) {
    state.edit.renderInspector();
    return;
  }
  const panel = byId('org-tree-detail');
  const selection = state.selection;
  if (!selection) {
    panel.hidden = true;
    panel.innerHTML = '';
    return;
  }
  const detail = selection.kind === 'unit'
    ? unitDetail(state.unitById.get(selection.id))
    : positionDetail(state.nodeById.get(selection.id));
  panel.hidden = false;
  panel.innerHTML = `
    <div class="org-detail-head">
      ${detail.avatar}
      <div class="org-detail-titles">
        <div class="org-detail-title">${escapeHtml(detail.title)}</div>
        <div class="org-detail-sub">${escapeHtml(detail.subtitle ?? '')}</div>
      </div>
      <tf-button variant="ghost" size="sm" icon="x" data-act="detail-close" aria-label="${escapeAttr(t('detail_close'))}"></tf-button>
    </div>
    <tf-key-value id="org-tree-detail-kv"></tf-key-value>`;
  byId('org-tree-detail-kv').entries = detail.entries;
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

function tree() {
  return byId('org-tree');
}

function applyPath(ids) {
  state.pathIds = ids;
  tree().pathIds = ids;
  renderPath();
}

function selectNode(id, kind) {
  if (kind === 'unit' ? !state.unitById.has(id) : !state.nodeById.has(id)) return;
  state.selection = { id, kind };
  const anchor = kind === 'unit' ? state.unitById.get(id).headId : id;
  applyPath(anchor ? pathTo(state.model, anchor) : []);
  renderDetail();
}

function clearSearch() {
  state.query = '';
  state.matches = [];
  state.matchIndex = 0;
  tree().matchIds = [];
  applyPath([]);
}

function gotoMatch(index) {
  const total = state.matches.length;
  state.matchIndex = ((index % total) + total) % total;
  const id = state.matches[state.matchIndex];
  // A hit is selected like a clicked card: the path bar and the detail panel follow it.
  selectNode(id, 'position');
  tree().focusNode(id, { select: true });
}

function onSearch(value) {
  const query = String(value ?? '').trim();
  if (!query) {
    clearSearch();
    return;
  }
  state.query = query;
  state.matches = searchNodes(state.model, query);
  tree().matchIds = state.matches;
  if (state.matches.length === 0) applyPath([]);
  else gotoMatch(0);
}

function goToMe() {
  const id = state.model.meIds[0];
  if (!id) {
    toast(t('my_position_none'), 'info');
    return;
  }
  selectNode(id, 'position');
  tree().focusNode(id, { select: true });
}

// The empty organization has no chart to draw: an administrator is offered the two ways to begin.
function renderEmpty() {
  const overlay = byId('org-tree-empty');
  if (!overlay) return;
  const empty = state.model.nodes.length === 0 && state.model.units.length === 0;
  overlay.hidden = !empty;
  if (!empty) {
    overlay.innerHTML = '';
    return;
  }
  const actions = canEdit()
    ? `<tf-button variant="primary" icon="sparkle" data-act="empty-template">${escapeHtml(t('edit.load_template'))}</tf-button>`
      + `<tf-button variant="secondary" icon="layers" data-act="empty-unit">${escapeHtml(t('edit.add_unit'))}</tf-button>`
    : '';
  const title = canEdit() ? t('edit.empty_title') : t('empty_title');
  const message = canEdit() ? t('edit.empty_message') : t('empty_message');
  overlay.innerHTML = `<tf-empty-state icon="sitemap" title="${escapeAttr(title)}" message="${escapeAttr(message)}">${actions}</tf-empty-state>`;
}

function editApi() {
  return {
    chart: tree(),
    detail: byId('org-tree-detail'),
    top: byId('org-edit-top'),
    below: byId('org-edit-below'),
    me: { name: state.meName },
    today: state.view.at,
    data: () => ({ view: state.view, model: state.model, unitTypes: state.unitTypes }),
    selection: () => state.selection,
    select(id, kind) {
      selectNode(id, kind);
      if (kind === 'position') tree().focusNode(id, { select: true });
      else if (tree().mode === 'units') tree().focusNode(`unit:${id}`, { select: true });
    },
    clearSelection() {
      state.selection = null;
      tree().selectedId = null;
      applyPath([]);
      renderDetail();
    },
    apply: applyStructure,
    renderActions,
    // The header's numbers and the list are the screen's, not this tab's: the edit mode says when a save changed them.
    notifyChanged: () => state.changed?.(),
    fitStage: fitStageHeight,
    // Leaving: the tab takes back today's structure and its own read-only panel.
    exit(answer) {
      state.edit = null;
      state.root.classList.remove('org-editing');
      byId('org-tree-empty')?.replaceChildren();
      applyStructure(answer);
      renderActions();
      fitStageHeight();
      state.changed?.();
    },
  };
}

function asOfApi() {
  return {
    apply: applyStructure,
    setDecorate(decorate) {
      state.decorate = decorate;
    },
  };
}

function startEdit(preset = null) {
  if (!state.edit) {
    // The edit mode draws its own effective day on the chart; another day on show gives way to it.
    state.asOf?.reset();
    state.edit = editEntry.open(editApi(), undefined, preset);
    state.root.classList.add('org-editing');
    renderActions();
  }
  return state.edit;
}

/**
 * Opens the edit mode on a stored reorganization (`{ id, name, effectiveDate, ops }`), waiting for the tab to be
 * drawn when the screen has only just been asked to show it. Resolves whether the edit mode took it.
 */
export async function openChangeSetInTree(preset) {
  for (let waited = 0; !(state && state.model) && waited < 4000; waited += 50) await new Promise((resolve) => setTimeout(resolve, 50));
  if (!state || !canEdit()) return false;
  if (state.edit) return state.edit.replaceWith(preset);
  startEdit(preset);
  return true;
}

/** Whether the screen may be left: the edit mode asks when it holds unsaved changes. */
export function canLeaveTree() {
  return state?.edit ? state.edit.canLeave() : true;
}

function isFullscreen() {
  return Boolean(document.fullscreenElement);
}

async function setPresenting(on) {
  if (on === state.presenting || (on && state.edit)) return;
  state.presenting = on;
  if (on) {
    // The stage opens clean; a click on a card brings the (read-only) details back.
    state.selection = null;
    tree().selectedId = null;
    renderDetail();
  }
  byId('org-tree-stage').classList.toggle('org-present', on);
  document.documentElement.classList.toggle('org-presenting', on);
  tree().presentation = on;
  if (on) {
    try {
      await byId('org-tree-stage').requestFullscreen?.();
      state.fullscreen = isFullscreen();
    } catch {
      // Refused (or unsupported): the stage stays pinned over the page instead.
      state.fullscreen = false;
    }
  } else {
    if (isFullscreen()) {
      state.fullscreen = false;
      try { await document.exitFullscreen(); } catch { /* already left */ }
    }
  }
  // Leaving presentation gives the stage back its place in the page; the window may have resized meanwhile.
  requestAnimationFrame(fitStageHeight);
}

function pdfPages() {
  const chart = tree();
  const overview = chart.toSvg({ theme: 'light' });
  if (!overview) return [];
  const pages = [{ title: t('pdf_overview'), svg: overview.svg }];
  for (const unit of state.model.units) {
    if (unit.memberIds.length === 0) continue;
    const page = chart.toSvg({ theme: 'light', unitId: unit.id });
    if (page) pages.push({ title: unit.name, svg: page.svg });
  }
  return pages;
}

// The branch the export can be limited to: what is selected (a unit stands for its head).
function branchRoot() {
  const selection = state.selection;
  if (!selection) return null;
  return selection.kind === 'unit' ? state.unitById.get(selection.id).headId : selection.id;
}

function branchPages(rootId) {
  const page = tree().toSvg({ theme: 'light', subtreeOf: rootId });
  return page ? [{ title: chipLabel(state.nodeById.get(rootId)), svg: page.svg }] : [];
}

async function runExport(action) {
  const branch = action.startsWith('branch-');
  const kind = branch ? action.slice('branch-'.length) : action;
  const rootId = branch ? branchRoot() : null;
  const base = `${t('export_filename')}-${state.view.at}${branch ? '-branch' : ''}`;
  const options = branch ? { subtreeOf: rootId } : {};
  try {
    let done = false;
    if (branch && !rootId) done = false;
    else if (kind === 'svg') done = exportSvg(tree(), `${base}.svg`, options);
    else if (kind === 'png') done = await exportPng(tree(), `${base}.png`, options);
    else if (kind.startsWith('pdf')) {
      const pages = branch ? branchPages(rootId) : pdfPages();
      if (pages.length) {
        printPages(pages, {
          title: t('title'), footer: t('pdf_footer', { date: formatDay(state.view.at) }), paper: kind === 'pdf-a3' ? 'A3' : 'A4',
        });
        done = true;
      }
    }
    if (!done) toast(t('export_nothing'), 'warning');
  } catch (err) {
    toast(t('export_failed', { message: err.message || '' }), 'error');
  }
}

function exportMenuItems() {
  const item = (action, icon, key) => `<tf-menu-item action="${action}" icon="${icon}">${escapeHtml(t(key))}</tf-menu-item>`;
  const whole = item('svg', 'file', 'export_svg') + item('png', 'image', 'export_png')
    + item('pdf-a4', 'file-text', 'export_pdf_a4') + item('pdf-a3', 'file-text', 'export_pdf_a3');
  if (!branchRoot()) return whole;
  return `${whole}<tf-menu-divider></tf-menu-divider>`
    + item('branch-svg', 'file', 'export_branch_svg') + item('branch-png', 'image', 'export_branch_png')
    + item('branch-pdf-a4', 'file-text', 'export_branch_pdf');
}

// The two switches of the chart live in one menu so the toolbar stays a single row in every language.
function viewMenuItems() {
  const item = (action, on, key, disabled = false) => `<tf-menu-item action="${action}" icon="${on ? 'check' : ''}"${disabled ? ' disabled' : ''}>${escapeHtml(t(key))}</tf-menu-item>`;
  return item('horizontal', tree().horizontal, 'layout_horizontal')
    + item('functional', tree().functional, 'functional_lines', tree().mode === 'units');
}

function openViewMenu(anchor) {
  const menu = byId('org-tree-view-menu');
  menu.innerHTML = viewMenuItems();
  menu.anchor = anchor;
  menu.open();
}

function onViewAction(action) {
  const chart = tree();
  if (action === 'horizontal') chart.horizontal = !chart.horizontal;
  else if (action === 'functional') chart.functional = !chart.functional;
}

function openExportMenu(anchor) {
  const menu = byId('org-tree-export-menu');
  // Rebuilt on every opening: the branch entries exist only while something is selected.
  menu.innerHTML = exportMenuItems();
  menu.anchor = anchor;
  menu.open();
}

function onClick(e) {
  const chip = e.target.closest('.org-path-chip');
  if (chip) {
    const id = chip.dataset.node;
    selectNode(id, 'position');
    tree().focusNode(id, { select: true });
    return;
  }
  const actEl = e.target.closest('[data-act]');
  const act = actEl?.dataset.act;
  // The edit mode's own buttons in the header; the ones inside the tab it handles itself.
  if (state.edit && act?.startsWith('edit-') && e.currentTarget === byId('org-header-actions')) {
    if (!actEl.hasAttribute('disabled')) state.edit.act(act, actEl);
    return;
  }
  switch (act) {
    case 'me': goToMe(); break;
    case 'edit': startEdit(); break;
    case 'empty-template': startEdit().templateWindow(); break;
    case 'empty-unit': startEdit().addUnitWindow(); break;
    case 'export': openExportMenu(e.target.closest('[data-act]')); break;
    case 'options': openViewMenu(e.target.closest('[data-act]')); break;
    case 'present': setPresenting(true); break;
    case 'present-exit': setPresenting(false); break;
    case 'match-prev': gotoMatch(state.matchIndex - 1); break;
    case 'match-next': gotoMatch(state.matchIndex + 1); break;
    case 'detail-close':
      state.selection = null;
      tree().selectedId = null;
      // The path opened branches for the selection; without it the chart returns to its automatic fit.
      applyPath([]);
      renderDetail();
      break;
    default:
  }
}

function onChange(e) {
  if (e.target.id === 'org-tree-mode') tree().mode = e.detail?.value === 'units' ? 'units' : 'persons';
}

function onKey(e) {
  if (e.key === 'Enter' && e.target.classList?.contains('org-path-chip')) e.target.click();
}

// "/" reaches the search box from anywhere on the screen while the Drzewo tab is showing.
function onDocumentKey(e) {
  if (!state) return;
  if (e.key === 'Escape' && state.presenting && !isFullscreen()) {
    setPresenting(false);
  } else if (e.key === '/' && !e.ctrlKey && !e.metaKey && !e.altKey
    && !byId('org-panel-tree')?.hidden && !e.target.closest?.('input, textarea, select, [contenteditable], tf-searchbox, tf-input')) {
    e.preventDefault();
    byId('org-tree-search')?.querySelector('input')?.focus();
  }
}

function onFullscreenChange() {
  if (state?.presenting && state.fullscreen && !isFullscreen()) {
    state.fullscreen = false;
    setPresenting(false);
  }
}

function wire(root) {
  root.addEventListener('click', onClick);
  root.addEventListener('change', onChange);
  root.addEventListener('search', (e) => onSearch(e.detail?.value));
  root.addEventListener('keydown', onKey);
  root.addEventListener('node-select', (e) => selectNode(e.detail.id, e.detail.kind));
  root.addEventListener('node-open', (e) => {
    selectNode(e.detail.id, e.detail.kind);
    byId('org-tree-detail')?.focus();
  });
  root.addEventListener('present-toggle', () => setPresenting(!state.presenting));
  root.addEventListener('export-menu', (e) => openExportMenu(e.detail.anchor));
  root.addEventListener('action', (e) => {
    if (e.target.id === 'org-tree-export-menu') runExport(e.detail.action);
    else if (e.target.id === 'org-tree-view-menu') onViewAction(e.detail.action);
  });
  document.addEventListener('keydown', onDocumentKey);
  document.addEventListener('fullscreenchange', onFullscreenChange);
}

function buildModel(view, unitTypes) {
  const model = buildTreeModel(view, { unitTypes, meHex: state.meHex, ...state.badges, t });
  // Another day's differences (history-asof.js) are marks on this model, so they survive every redraw.
  state.decorate?.(model);
  return model;
}

// Who is away and who is a deputy today, for the "nieobecny" and "zastępstwo" badges. The answer carries
// no reason; a failure only costs the badges, so the chart still opens.
async function readBadges() {
  try {
    return treeBadges(await ApiBinary.one('orgAvailabilityRequest', {}));
  } catch {
    return { absentKeys: new Set(), coveringKeys: new Set() };
  }
}

function sameKeys(a, b) {
  return a.size === b.size && [...a].every((key) => b.has(key));
}

function indexModel(model) {
  state.model = model;
  state.nodeById = new Map(model.nodes.map((n) => [n.id, n]));
  state.unitById = new Map(model.units.map((u) => [u.id, u]));
}

/**
 * Takes a newer structure into the chart already on screen. Zoom, expansion and selection
 * survive; a selection whose position no longer exists is dropped.
 */
function applyStructure({ view, unitTypes, myPermissions = [] }) {
  state.view = view;
  state.unitTypes = unitTypes;
  state.myPermissions = myPermissions;
  indexModel(buildModel(view, unitTypes));
  tree().updateModel(state.model);
  if (state.selection) {
    const still = state.selection.kind === 'unit' ? state.unitById.has(state.selection.id) : state.nodeById.has(state.selection.id);
    if (still) selectNode(state.selection.id, state.selection.kind);
    else state.selection = null;
  }
  if (state.query) {
    state.matches = searchNodes(state.model, state.query);
    state.matchIndex = Math.min(state.matchIndex, Math.max(0, state.matches.length - 1));
    tree().matchIds = state.matches;
  }
  state.pathIds = state.pathIds.filter((id) => state.nodeById.has(id));
  applyPath(state.pathIds);
  renderDetail();
  renderLegend();
  renderEmpty();
  renderActions();
}

/**
 * Draws the Drzewo tab into `host`. `view`, `unitTypes` and `myPermissions` come from a structure answer;
 * `changed` is called when the edit mode ends, so the rest of the screen can read the structure again.
 */
export async function mountTreeTab(host, { view, unitTypes, myPermissions = [] }, { changed = null } = {}) {
  unmountTreeTab();
  // The session user id is what "Moja pozycja" and the "You" badge hang on; a failure only
  // costs those two, so the chart still opens.
  const [me, badges] = await Promise.all([ApiBinary.one('authMeRequest').catch(() => null), readBadges()]);
  state = {
    host,
    view,
    unitTypes,
    myPermissions,
    edit: null,
    changed,
    meName: me?.username ?? '',
    meHex: userIdHex(me?.userId),
    badges,
    model: null,
    nodeById: null,
    unitById: null,
    query: '',
    matches: [],
    matchIndex: 0,
    pathIds: [],
    selection: null,
    presenting: false,
    fullscreen: false,
  };
  indexModel(buildModel(view, unitTypes));
  // A fresh root per mount: its listeners die with it, so a remount never doubles them.
  const root = document.createElement('div');
  root.className = 'org-tree-tab';
  state.root = root;
  root.innerHTML = shell(view);
  host.replaceChildren(root);
  wire(root);
  document.addEventListener('keydown', onDocumentKey);
  document.addEventListener('fullscreenchange', onFullscreenChange);
  window.addEventListener('resize', fitStageHeight);
  renderActions();
  const chart = byId('org-tree');
  chart.labels = treeLabels();
  chart.model = state.model;
  renderLegend();
  renderEmpty();
  state.asOf = mountAsOf(byId('org-tree-asof'), asOfApi(), { view, unitTypes, myPermissions }, byId('org-tree-asof-note'));
  // Fonts and the screen's own entry settle after the first paint; measure again then.
  requestAnimationFrame(() => requestAnimationFrame(fitStageHeight));
  document.fonts?.ready?.then(fitStageHeight);
}

/** Feeds a newer structure answer to the chart on screen; the edit mode, while open, owns what is shown. */
export function refreshTreeTab(input) {
  if (!state || state.edit) return;
  state.asOf?.setBase(input);
  if (state.asOf?.active()) return;
  applyStructure(input);
  // The badges can change without the structure changing (a leave starts); read them again and redraw
  // only when they did.
  const shown = state;
  readBadges().then((badges) => {
    if (state !== shown || state.edit) return;
    if (sameKeys(badges.absentKeys, state.badges.absentKeys) && sameKeys(badges.coveringKeys, state.badges.coveringKeys)) return;
    state.badges = badges;
    applyStructure({ view: state.view, unitTypes: state.unitTypes, myPermissions: state.myPermissions });
  });
}

export function unmountTreeTab() {
  if (!state) return;
  state.asOf?.dispose();
  state.edit?.dispose();
  document.removeEventListener('keydown', onDocumentKey);
  document.removeEventListener('fullscreenchange', onFullscreenChange);
  window.removeEventListener('resize', fitStageHeight);
  document.documentElement.classList.remove('org-presenting');
  if (isFullscreen()) document.exitFullscreen?.().catch(() => {});
  byId('org-header-actions')?.replaceChildren();
  state.host.replaceChildren();
  state = null;
}
