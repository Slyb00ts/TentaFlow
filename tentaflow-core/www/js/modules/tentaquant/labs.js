// ===== File: modules/tentaquant/labs.js — Q01, the laboratories the caller may enter =====
//
// One instance of the `tentaquant` package is one laboratory, so this view is
// the instance list of LabList: name, the people the matrix admits, the role it
// resolves the caller to, the nodes the instance reconciled onto and the last
// change. A laboratory has no owner, so no tile shows one (§18 decision 26).
//
// Only the facts the wire carries are drawn. Runs, QPU pools and the execution
// tiers above T1 arrive with their own features and are absent here rather than
// mocked.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import {
  T, sprite, fmtAgo, isSolo, labIsReady, nodeStateLabel, permissionSummary, roleLabel, roleOf,
} from '/js/modules/tentaquant/format.js';

const ROLE_TONE = { admin: 'warn', supervisor: 'accent', user: 'info', observer: 'neutral' };

// Nodes, shortest useful form: "spark-01 · gotowy, ws-amd · offline".
function nodesLine(lab) {
  const nodes = lab.nodes || [];
  if (!nodes.length) return T('labs.nodes_none');
  return nodes.map((n) => `${n.nodeName} · ${nodeStateLabel(n)}`).join(', ');
}

// T0 always exists (the simulator ships with the dashboard); T1 is the Core
// instance and follows the node state. T2–T4 are declared by the plan but have
// no backend yet, so no tile claims them.
function tiersHtml(lab) {
  const ready = labIsReady(lab);
  return `
    <span class="tier t0">${escapeHtml(T('lab.tier_t0'))}</span>
    <span class="tier t1${ready ? '' : ' off'}">${escapeHtml(T('lab.tier_t1'))}</span>`;
}

function accessLine(lab) {
  if (isSolo(lab)) return `${sprite('user')}${escapeHtml(T('labs.access_only_you'))}`;
  return `${sprite('users')}${escapeHtml(T('labs.access_matrix'))} · ${escapeHtml(T('labs.people', { n: Number(lab.peopleCount) || 0 }))}`;
}

function statusChipHtml(lab) {
  const ready = labIsReady(lab);
  return `<tf-chip status="${ready ? 'ok' : 'warn'}" dot label="${escapeAttr(ready ? T('lab.status_ready') : T('lab.status_not_ready'))}"></tf-chip>`;
}

function labCardHtml(lab) {
  const role = roleOf(lab.myPermissions);
  const solo = isSolo(lab);
  const disabled = !lab.enabled;
  return `
    <div class="q-card${solo ? ' local' : ''}${disabled ? ' is-disabled' : ''}" data-lab="${escapeAttr(lab.instanceId)}">
      <div class="qc-top">
        <div class="qc-ico${solo ? '' : ' qpu'}">${sprite(solo ? 'desktop' : 'atom')}</div>
        <div class="qc-head">
          <div class="qc-name">${escapeHtml(lab.displayName || lab.instanceId)}</div>
          <div class="qc-type">${escapeHtml(nodesLine(lab))}</div>
          <div class="qc-id mono">tentaquant · ${escapeHtml(lab.instanceId)}</div>
        </div>
      </div>
      <div class="qc-tiers">
        ${tiersHtml(lab)}
        ${disabled ? `<tf-chip status="warn" label="${escapeAttr(T('labs.disabled'))}"></tf-chip>` : ''}
        <span class="qc-role"><tf-chip status="${ROLE_TONE[role]}" icon="user" label="${escapeAttr(roleLabel(lab.myPermissions))}"></tf-chip></span>
      </div>
      <div class="qc-owner">${accessLine(lab)}</div>
      <div class="qc-perms mono">${escapeHtml(permissionSummary(lab.myPermissions) || T('labs.perms_none'))}</div>
      <div class="qc-stats">
        <div class="qc-stat"><div class="v">${Number(lab.projectCount) || 0}</div><div class="l">${escapeHtml(T('labs.stat_projects'))}</div></div>
        <div class="qc-stat"><div class="v">${solo ? escapeHtml(T('labs.access_only_you')) : Number(lab.peopleCount) || 0}</div><div class="l">${escapeHtml(T('labs.stat_people'))}</div></div>
        <div class="qc-stat"><div class="v">${escapeHtml(fmtAgo(lab.lastActivityAt))}</div><div class="l">${escapeHtml(T('labs.stat_activity'))}</div></div>
      </div>
      <div class="qc-foot">
        ${statusChipHtml(lab)}
        <span class="qc-foot-right">
          <tf-button variant="ghost" size="sm" icon="arrow" data-open ${disabled ? 'disabled' : ''}>${escapeHtml(T('labs.open'))}</tf-button>
          <span class="menu-wrap">
            <tf-button variant="ghost" size="sm" icon="more" data-more title="${escapeAttr(T('labs.menu_more'))}"></tf-button>
            <tf-menu placement="bottom-end" data-lab-menu>
              <tf-menu-item action="open" icon="external-link">${escapeHtml(T('labs.menu_open'))}</tf-menu-item>
              <tf-menu-divider></tf-menu-divider>
              <tf-menu-item action="addons" icon="shield">${escapeHtml(T('labs.menu_permissions'))}</tf-menu-item>
            </tf-menu>
          </span>
        </span>
      </div>
    </div>`;
}

function newLabCardHtml(canCreate) {
  return `
    <div class="card-new${canCreate ? '' : ' locked'}" data-new-lab>
      <div class="cn-ico">${sprite('plus')}</div>
      <div class="cn-title">${escapeHtml(T('labs.new_title'))}</div>
      <div class="cn-sub">${escapeHtml(T('labs.new_sub'))}</div>
      ${canCreate ? '' : `<div class="cn-lock">${sprite('shield')}${escapeHtml(T('labs.new_locked'))}</div>`}
    </div>`;
}

export function matchesLab(lab, query, filter) {
  const q = String(query || '').trim().toLowerCase();
  if (q && !`${lab.displayName} ${lab.instanceId}`.toLowerCase().includes(q)) return false;
  if (filter === 'supervisor') return ['admin', 'supervisor'].includes(roleOf(lab.myPermissions));
  return true;
}

export function sortLabs(labs, sort) {
  const rows = (labs || []).slice();
  if (sort === 'name') rows.sort((a, b) => String(a.displayName || '').localeCompare(String(b.displayName || '')));
  else if (sort === 'projects') rows.sort((a, b) => (Number(b.projectCount) || 0) - (Number(a.projectCount) || 0));
  else rows.sort((a, b) => String(b.lastActivityAt || '').localeCompare(String(a.lastActivityAt || '')));
  return rows;
}

export function drawLabs(screen) {
  const visible = sortLabs(screen.labs.filter((l) => matchesLab(l, screen.labQuery, screen.labFilter)), screen.labSort);
  const supervised = screen.labs.filter((l) => ['admin', 'supervisor'].includes(roleOf(l.myPermissions))).length;

  screen.root.innerHTML = `
    <div class="tq-page-head">
      <div>
        <div class="tq-page-title">${escapeHtml(T('title'))}</div>
        <div class="tq-page-sub">${escapeHtml(T('labs.subtitle'))}</div>
      </div>
      <div class="tq-page-actions">
        <tf-button variant="secondary" icon="puzzle" data-act="addons">${escapeHtml(T('labs.catalog_action'))}</tf-button>
        <tf-button variant="primary" icon="plus" data-act="new-lab" ${screen.canCreate ? '' : 'disabled'}>${escapeHtml(T('labs.new_title'))}</tf-button>
      </div>
    </div>

    <div class="tf-toolbar">
      <tf-searchbox id="tq-lab-search" placeholder="${escapeAttr(T('labs.search_placeholder'))}" debounce="200" value="${escapeAttr(screen.labQuery)}"></tf-searchbox>
      <tf-filter-chips id="tq-lab-filter"></tf-filter-chips>
      <span class="tf-toolbar-spacer"></span>
      <tf-select id="tq-lab-sort" value="${escapeAttr(screen.labSort)}">
        <option value="activity">${escapeHtml(T('labs.sort_activity'))}</option>
        <option value="name">${escapeHtml(T('labs.sort_name'))}</option>
        <option value="projects">${escapeHtml(T('labs.sort_projects'))}</option>
      </tf-select>
      <tf-segmented id="tq-lab-view" value="${escapeAttr(screen.labView)}">
        <option value="cards" icon="apps">${escapeHtml(T('labs.view_cards'))}</option>
        <option value="list" icon="list">${escapeHtml(T('labs.view_list'))}</option>
      </tf-segmented>
    </div>

    <div id="tq-lab-body">
      ${visible.length === 0
        ? `${screen.labView === 'cards' ? `<div class="card-grid">${newLabCardHtml(screen.canCreate)}</div>` : ''}
           <tf-empty-state icon="puzzle" title="${escapeAttr(T('labs.empty_title'))}" message="${escapeAttr(T('labs.empty_message'))}"></tf-empty-state>`
        : screen.labView === 'list'
          ? `<tf-table id="tq-lab-table">
              <tf-column key="name" label="${escapeAttr(T('labs.col_name'))}" renderer="html" fill></tf-column>
              <tf-column key="role" label="${escapeAttr(T('labs.col_role'))}" renderer="html" nowrap></tf-column>
              <tf-column key="people" label="${escapeAttr(T('labs.col_people'))}" renderer="text" nowrap></tf-column>
              <tf-column key="projects" label="${escapeAttr(T('labs.col_projects'))}" renderer="text" nowrap></tf-column>
              <tf-column key="status" label="${escapeAttr(T('labs.col_status'))}" renderer="html" nowrap></tf-column>
              <tf-column key="activity" label="${escapeAttr(T('labs.col_activity'))}" renderer="text" nowrap></tf-column>
            </tf-table>`
          : `<div class="card-grid" id="tq-lab-grid">
              ${visible.map(labCardHtml).join('')}
              ${newLabCardHtml(screen.canCreate)}
            </div>`}
    </div>

    <div class="tq-table-footer">
      <span>${escapeHtml(T('labs.footer', { n: visible.length }))}</span>
      <span>${escapeHtml(T('labs.footer_supervisor', { n: supervised }))}</span>
      <span class="tf-toolbar-spacer"></span>
      <span>${escapeHtml(T('labs.sort_label', { name: T('labs.sort_' + screen.labSort) }))}</span>
    </div>

    <tf-alert tone="info" message="${escapeAttr(T('labs.hint'))}"></tf-alert>`;

  const chips = screen.root.querySelector('#tq-lab-filter');
  chips.filters = [
    { id: 'all', label: T('labs.filter_all'), count: screen.labs.length, active: screen.labFilter === 'all' },
    { id: 'supervisor', label: T('labs.filter_supervisor'), icon: 'shield', count: supervised, active: screen.labFilter === 'supervisor' },
  ];

  const table = screen.root.querySelector('#tq-lab-table');
  if (table) {
    table.rows = visible.map((lab) => ({
      _lab: lab.instanceId,
      // Two-line cells use the controls.css classes: tf-table builds its rows in
      // a shadow root that adopts controls.css alone, so a feature stylesheet
      // cannot reach them.
      name: `<div class="tf-table__cell-title">${escapeHtml(lab.displayName || lab.instanceId)}</div><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(lab.instanceId)}</div>`,
      role: `<tf-chip status="${ROLE_TONE[roleOf(lab.myPermissions)]}" label="${escapeAttr(roleLabel(lab.myPermissions))}"></tf-chip>`,
      people: isSolo(lab) ? T('labs.access_only_you') : String(Number(lab.peopleCount) || 0),
      projects: String(Number(lab.projectCount) || 0),
      status: statusChipHtml(lab),
      activity: fmtAgo(lab.lastActivityAt),
    }));
    table.addEventListener('row-click', (e) => {
      const lab = screen.labs.find((l) => l.instanceId === e.detail.row._lab);
      if (lab && lab.enabled) screen.openLab(lab.instanceId);
    });
  }

  wire(screen);
}

function wire(screen) {
  const root = screen.root;
  root.querySelector('#tq-lab-search').addEventListener('search', (e) => {
    screen.labQuery = String(e.detail?.value ?? '');
    drawLabs(screen);
  });
  root.querySelector('#tq-lab-filter').addEventListener('change', (e) => {
    screen.labFilter = e.detail?.id || 'all';
    drawLabs(screen);
  });
  root.querySelector('#tq-lab-sort').addEventListener('change', (e) => {
    screen.labSort = e.detail?.value || 'activity';
    drawLabs(screen);
  });
  root.querySelector('#tq-lab-view').addEventListener('change', (e) => {
    screen.labView = e.detail?.value || 'cards';
    drawLabs(screen);
  });
  root.querySelector('[data-act="addons"]').addEventListener('click', () => screen.openAddons());
  root.querySelector('[data-act="new-lab"]').addEventListener('click', () => screen.openNewLab());

  const body = root.querySelector('#tq-lab-body');
  body.addEventListener('click', (e) => {
    if (e.target.closest('[data-new-lab]')) {
      if (screen.canCreate) screen.openNewLab();
      return;
    }
    const more = e.target.closest('[data-more]');
    if (more) {
      more.parentElement.querySelector('[data-lab-menu]').toggle();
      return;
    }
    if (e.target.closest('[data-lab-menu]')) return;
    const card = e.target.closest('[data-lab]');
    if (card && !card.classList.contains('is-disabled')) screen.openLab(card.dataset.lab);
  });
  body.addEventListener('action', (e) => {
    const card = e.target.closest('[data-lab]');
    if (!card || !e.target.closest('[data-lab-menu]')) return;
    if (e.detail?.action === 'open') screen.openLab(card.dataset.lab);
    else screen.openAddons();
  });
}
