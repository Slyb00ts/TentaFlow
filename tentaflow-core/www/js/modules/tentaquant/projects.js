// ===== File: modules/tentaquant/projects.js — Q03, the Projekty tab of one laboratory =====
//
// Three sections over ONE list: what the caller owns, what somebody shared with
// them by name, and what is published to the whole laboratory. The wire resolves
// the role, so the section is a pure function of (my_role, visibility) and never
// a second guess at access — see `sectionOf` in format.js.
//
// A card opens the project (its notebook, Q06); the actions that act ON the
// project — share, archive, delete — sit on the card and stop the click from
// reaching it.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import {
  T, sprite, fmtAgo, fmtDate, sectionProjects,
} from '/js/modules/tentaquant/format.js';

const SECTIONS = ['mine', 'shared', 'lab'];
const SECTION_ICON = { mine: 'crown', shared: 'share', lab: 'file-text' };
const CARD_ICON = { mine: 'folder', shared: 'share', lab: 'file-text' };
// tf-empty-state draws from /img/icons.svg, which is a different set from the
// inline `#i-` sprite the section heads use — `crown` exists only in the latter.
const EMPTY_ICON = { mine: 'folder', shared: 'share', lab: 'file-text' };
const ROLE_TONE = { owner: 'accent', editor: 'ok', viewer: 'info' };

export function matchesProject(project, query) {
  const q = String(query || '').trim().toLowerCase();
  if (!q) return true;
  return `${project.name} ${project.description} ${project.ownerName}`.toLowerCase().includes(q);
}

export function sortProjects(projects, sort) {
  const rows = (projects || []).slice();
  if (sort === 'name') rows.sort((a, b) => String(a.name || '').localeCompare(String(b.name || '')));
  else if (sort === 'runs') rows.sort((a, b) => (Number(b.runCount) || 0) - (Number(a.runCount) || 0));
  else rows.sort((a, b) => String(b.updatedAt || '').localeCompare(String(a.updatedAt || '')));
  return rows;
}

// The ownership line of a card, which is the whole point of the ML-Studio model:
// who owns it, and — for the owner — who else can reach it.
function ownerLineHtml(project) {
  if (project.myRole === 'owner') {
    const shares = Number(project.shareCount) || 0;
    const visibility = project.visibility === 'lab'
      ? `<b>${escapeHtml(T('projects.visibility_lab'))}</b>`
      : `<b>${escapeHtml(T('projects.visibility_private'))}</b>`;
    const shared = shares > 0 ? ` · ${escapeHtml(T('projects.shared_with', { n: shares }))}` : '';
    return `${sprite(project.visibility === 'lab' ? 'eye' : 'lock')}${escapeHtml(T('projects.owner_you'))} · ${visibility}${shared}`;
  }
  return `${sprite('crown')}${escapeHtml(T('projects.owner_named', { name: project.ownerName || project.ownerUserId }))}`;
}

function menuItemsHtml(project) {
  if (project.myRole !== 'owner') return '';
  const archived = Boolean(project.archivedAt);
  return `
    <tf-menu-item action="share" icon="share">${escapeHtml(T('projects.menu_share'))}</tf-menu-item>
    <tf-menu-item action="${archived ? 'unarchive' : 'archive'}" icon="${archived ? 'refresh' : 'clock'}">${escapeHtml(T(archived ? 'projects.menu_unarchive' : 'projects.menu_archive'))}</tf-menu-item>
    <tf-menu-divider></tf-menu-divider>
    <tf-menu-item action="delete" icon="trash" danger>${escapeHtml(T('projects.menu_delete'))}</tf-menu-item>`;
}

function projectCardHtml(project, section) {
  const isOwner = project.myRole === 'owner';
  const archived = Boolean(project.archivedAt);
  return `
    <div class="q-card${archived ? ' is-archived' : ''}${section === 'lab' ? ' lab-mat' : ''}" data-project="${escapeAttr(project.projectId)}">
      ${isOwner ? `<tf-button class="qc-share" variant="ghost" size="sm" icon="share" data-share title="${escapeAttr(T('projects.share_action'))}"></tf-button>` : ''}
      <div class="qc-top">
        <div class="qc-ico">${sprite(CARD_ICON[section])}</div>
        <div class="qc-head">
          <div class="qc-name">${escapeHtml(project.name)}</div>
          <div class="qc-id mono">${escapeHtml(project.projectId)}</div>
        </div>
      </div>
      <div class="qc-desc">${escapeHtml(project.description || T('projects.no_description'))}</div>
      <div class="qc-owner">
        ${ownerLineHtml(project)}
        ${isOwner ? '' : `<span class="qc-role"><tf-chip status="${ROLE_TONE[project.myRole]}" label="${escapeAttr(T('projects.role_' + project.myRole))}"></tf-chip></span>`}
      </div>
      <div class="qc-stats">
        <div class="qc-stat"><div class="v">${Number(project.notebookCount) || 0}</div><div class="l">${escapeHtml(T('projects.stat_notebooks'))}</div></div>
        <div class="qc-stat"><div class="v">${Number(project.fileCount) || 0}</div><div class="l">${escapeHtml(T('projects.stat_files'))}</div></div>
        <div class="qc-stat"><div class="v">${Number(project.runCount) || 0}</div><div class="l">${escapeHtml(T('projects.stat_runs'))}</div></div>
      </div>
      <div class="qc-foot">
        <span class="qc-activity" title="${escapeAttr(fmtDate(project.updatedAt))}">${sprite('clock')}${escapeHtml(T('projects.updated', { when: fmtAgo(project.updatedAt) }))}</span>
        <span class="qc-foot-right">
          ${archived ? `<tf-chip status="warn" label="${escapeAttr(T('projects.archived_chip'))}"></tf-chip>` : ''}
          ${isOwner ? `<span class="menu-wrap">
            <tf-button variant="ghost" size="sm" icon="more" data-more title="${escapeAttr(T('projects.menu_more'))}"></tf-button>
            <tf-menu placement="bottom-end" data-project-menu>${menuItemsHtml(project)}</tf-menu>
          </span>` : ''}
        </span>
      </div>
    </div>`;
}

function sectionHtml(section, rows) {
  return `
    <div class="tq-section-head">
      <h3>${sprite(SECTION_ICON[section])}${escapeHtml(T('projects.section_' + section))}</h3>
      <span class="sub">${escapeHtml(T('projects.section_' + section + '_sub'))}</span>
    </div>
    ${rows.length || section === 'mine'
      ? `<div class="card-grid">
          ${section === 'mine' ? `<div class="card-new" data-new-project>
            <div class="cn-ico">${sprite('plus')}</div>
            <div class="cn-title">${escapeHtml(T('projects.new'))}</div>
            <div class="cn-sub">${escapeHtml(T('projects.new_sub'))}</div>
          </div>` : ''}
          ${rows.map((p) => projectCardHtml(p, section)).join('')}
        </div>`
      : `<tf-empty-state icon="${EMPTY_ICON[section]}" title="${escapeAttr(T('projects.empty_' + section))}" message="${escapeAttr(T('projects.empty_' + section + '_sub'))}"></tf-empty-state>`}`;
}

// The flat list view of the same rows. An empty result is an empty STATE, not an
// empty table: tf-table draws headers over nothing and says why by itself.
function tableHtml(rows) {
  if (!rows.length) {
    return `<tf-empty-state icon="folder" title="${escapeAttr(T('projects.empty_list'))}" message="${escapeAttr(T('projects.empty_list_sub'))}"></tf-empty-state>`;
  }
  return `
    <tf-table id="tq-project-table">
      <tf-column key="name" label="${escapeAttr(T('projects.col_name'))}" renderer="html" fill></tf-column>
      <tf-column key="owner" label="${escapeAttr(T('projects.col_owner'))}" renderer="text" nowrap></tf-column>
      <tf-column key="visibility" label="${escapeAttr(T('projects.col_visibility'))}" renderer="html" nowrap></tf-column>
      <tf-column key="role" label="${escapeAttr(T('projects.col_role'))}" renderer="html" nowrap></tf-column>
      <tf-column key="runs" label="${escapeAttr(T('projects.col_runs'))}" renderer="text" nowrap></tf-column>
      <tf-column key="updated" label="${escapeAttr(T('projects.col_updated'))}" renderer="text" nowrap></tf-column>
    </tf-table>`;
}

export function drawProjects(screen, host) {
  const all = screen.projects.filter((p) => matchesProject(p, screen.projectQuery));
  const grouped = sectionProjects(sortProjects(all, screen.projectSort));
  const shown = screen.projectFilter === 'all' ? SECTIONS : [screen.projectFilter];
  const listRows = shown.flatMap((s) => grouped[s]);
  const visibleCount = listRows.length;

  host.innerHTML = `
    <div class="tf-toolbar">
      <tf-searchbox id="tq-project-search" placeholder="${escapeAttr(T('projects.search_placeholder'))}" debounce="200" value="${escapeAttr(screen.projectQuery)}"></tf-searchbox>
      <tf-select id="tq-project-sort" value="${escapeAttr(screen.projectSort)}">
        <option value="updated">${escapeHtml(T('projects.sort_updated'))}</option>
        <option value="name">${escapeHtml(T('projects.sort_name'))}</option>
        <option value="runs">${escapeHtml(T('projects.sort_runs'))}</option>
      </tf-select>
      <tf-filter-chips id="tq-project-filter"></tf-filter-chips>
      <span class="tf-toolbar-spacer"></span>
      <tf-segmented id="tq-project-view" value="${escapeAttr(screen.projectView)}">
        <option value="cards" icon="apps">${escapeHtml(T('projects.view_cards'))}</option>
        <option value="list" icon="list">${escapeHtml(T('projects.view_list'))}</option>
      </tf-segmented>
      <tf-button variant="primary" icon="plus" data-act="new-project">${escapeHtml(T('projects.new'))}</tf-button>
    </div>

    <tf-alert tone="info" message="${escapeAttr(T('projects.callout'))}"></tf-alert>

    <div id="tq-project-body">
      ${screen.projectView === 'list'
        ? tableHtml(listRows)
        : shown.map((s) => sectionHtml(s, grouped[s])).join('')}
    </div>

    <div class="tq-table-footer">
      <span>${escapeHtml(T('projects.footer', { n: visibleCount }))}</span>
      <span>${escapeHtml(T('projects.footer_mine', { n: grouped.mine.length }))}</span>
      <span>${escapeHtml(T('projects.footer_shared', { n: grouped.shared.length }))}</span>
      <span>${escapeHtml(T('projects.footer_lab', { n: grouped.lab.length }))}</span>
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="ghost" size="sm" icon="clock" data-act="toggle-archived">${escapeHtml(T(screen.includeArchived ? 'projects.hide_archived' : 'projects.show_archived'))}</tf-button>
    </div>`;

  const chips = host.querySelector('#tq-project-filter');
  // Icons and counts follow Q03: the chips say how much each section holds, so
  // the user picks a filter without opening it first.
  chips.filters = [
    { id: 'all', label: T('projects.filter_all'), count: all.length, active: screen.projectFilter === 'all' },
    { id: 'mine', label: T('projects.filter_mine'), icon: 'crown', count: grouped.mine.length, active: screen.projectFilter === 'mine' },
    { id: 'shared', label: T('projects.filter_shared'), icon: 'share', count: grouped.shared.length, active: screen.projectFilter === 'shared' },
    { id: 'lab', label: T('projects.filter_lab'), icon: 'file-text', count: grouped.lab.length, active: screen.projectFilter === 'lab' },
  ];

  const table = host.querySelector('#tq-project-table');
  if (table) {
    table.rows = listRows.map((p) => ({
      _project: p.projectId,
      // controls.css classes, not tentaquant.css: the cell lands inside the
      // tf-table shadow root, which adopts controls.css and nothing else.
      name: `<div class="tf-table__cell-title">${escapeHtml(p.name)}</div><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(p.projectId)}</div>`,
      owner: p.myRole === 'owner' ? T('projects.owner_you') : (p.ownerName || p.ownerUserId),
      visibility: `<tf-chip status="${p.visibility === 'lab' ? 'info' : 'neutral'}" label="${escapeAttr(T(p.visibility === 'lab' ? 'projects.visibility_lab' : 'projects.visibility_private'))}"></tf-chip>`,
      role: `<tf-chip status="${ROLE_TONE[p.myRole]}" label="${escapeAttr(T('projects.role_' + p.myRole))}"></tf-chip>`,
      runs: String(Number(p.runCount) || 0),
      updated: fmtAgo(p.updatedAt),
    }));
  }

  wire(screen, host);
}

function wire(screen, host) {
  host.querySelector('#tq-project-search').addEventListener('search', (e) => {
    screen.projectQuery = String(e.detail?.value ?? '');
    drawProjects(screen, host);
  });
  host.querySelector('#tq-project-sort').addEventListener('change', (e) => {
    screen.projectSort = e.detail?.value || 'updated';
    drawProjects(screen, host);
  });
  host.querySelector('#tq-project-filter').addEventListener('change', (e) => {
    screen.projectFilter = e.detail?.id || 'all';
    drawProjects(screen, host);
  });
  host.querySelector('#tq-project-view').addEventListener('change', (e) => {
    screen.projectView = e.detail?.value || 'cards';
    drawProjects(screen, host);
  });
  host.querySelector('[data-act="new-project"]').addEventListener('click', () => screen.openNewProject());
  host.querySelector('[data-act="toggle-archived"]').addEventListener('click', () => screen.setIncludeArchived(!screen.includeArchived));

  const body = host.querySelector('#tq-project-body');
  body.addEventListener('click', (e) => {
    if (e.target.closest('[data-new-project]')) { screen.openNewProject(); return; }
    const share = e.target.closest('[data-share]');
    if (share) {
      screen.openShare(share.closest('[data-project]').dataset.project);
      return;
    }
    const more = e.target.closest('[data-more]');
    if (more) { more.parentElement.querySelector('[data-project-menu]').toggle(); return; }
    // The menu is a CHILD of the card, so a click on one of its items bubbles
    // out through the card: without this guard 'Usuń projekt' would open its
    // confirm window and navigate into the project underneath it.
    if (e.target.closest('[data-project-menu]')) return;
    const card = e.target.closest('[data-project]');
    if (card) screen.openProject(card.dataset.project);
  });
  body.addEventListener('action', (e) => {
    const card = e.target.closest('[data-project]');
    if (!card || !e.target.closest('[data-project-menu]')) return;
    const id = card.dataset.project;
    switch (e.detail?.action) {
      case 'share': screen.openShare(id); break;
      case 'archive': screen.setArchived(id, true); break;
      case 'unarchive': screen.setArchived(id, false); break;
      case 'delete': screen.confirmDelete(id); break;
      default: break;
    }
  });
  const table = host.querySelector('#tq-project-table');
  if (table) {
    table.addEventListener('row-click', (e) => screen.openProject(e.detail.row._project));
  }
}
