// =============================================================================
// File: modules/tentaquant.js — the TentaQuant screen (quantum laboratories,
//       mockups Q01–Q05). One screen, two views: the laboratory list (no
//       instance selected) and one laboratory with its tabs.
//
//       TentaQuant is the first MULTI-INSTANCE native app: one instance is one
//       laboratory with its own database, content and permission matrix, so
//       every request but LabList carries `instanceId` and the route names it
//       (`#/tentaquant?instance=…`). Membership is that matrix in Addons — this
//       screen never edits it and never invents a members table of its own.
//
//       Tabs stop at Pulpit and Projekty on purpose: Runy, Urządzenia,
//       Przykłady, Kurs and Ustawienia arrive with their backends. Nothing here
//       renders a section whose data does not exist.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { Router } from '/js/router.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, chooseEntryLab, errMessage, has, isSolo, labIsReady, nodeStateLabel,
  permissionSummary, roleLabel,
} from '/js/modules/tentaquant/format.js';
import { drawLabs } from '/js/modules/tentaquant/labs.js';
import { drawDashboard } from '/js/modules/tentaquant/dashboard.js';
import { drawProjects } from '/js/modules/tentaquant/projects.js';
import { openNewProjectWindow, openShareWindow, confirmDeleteProject } from '/js/modules/tentaquant/dialogs.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-input.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-select.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-table.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-textarea.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-window.js';

const TABS = ['dashboard', 'projects'];

// The catalog package one laboratory is an instance of.
const PACKAGE_ID = 'tentaquant';

const TentaQuantScreen = {
  get title() { return T('title'); },

  render() {
    return '<div id="tq-root" class="tq-root"></div>';
  },

  async mount(params = {}) {
    this.root = byId('tq-root');
    this.disposed = false;
    this.userId = '';
    this.labs = [];
    this.canCreate = false;
    this.instanceId = params.instance || null;
    this.tab = TABS.includes(params.tab) ? params.tab : 'dashboard';
    this.lab = null;
    this.overview = null;
    this.overviewError = '';
    this.projects = [];
    this.projectsError = '';
    this.includeArchived = false;
    this.focusProject = null;
    // List-view state; kept on the screen so a tab switch does not reset the
    // filters the user set.
    this.labQuery = '';
    this.labFilter = 'all';
    this.labSort = 'activity';
    this.labView = 'cards';
    this.projectQuery = '';
    this.projectFilter = 'all';
    this.projectSort = 'updated';
    this.projectView = 'cards';

    try {
      const me = await ApiBinary.one('authMeRequest');
      this.userId = me?.userId ?? me?.user_id ?? '';
    } catch {
      this.userId = '';
    }

    const ok = await this.loadLabs();
    if (this.disposed || !ok) return;

    // Plan §19.8: the list is a choice, and there is no choice to make with one
    // laboratory — that user goes straight in.
    this.instanceId = chooseEntryLab(this.labs, this.instanceId);
    await this.enter();
  },

  unmount() {
    this.disposed = true;
    document.querySelectorAll('tf-window.tq-modal').forEach((w) => w.remove());
  },

  // ---------------------------------------------------------------------------
  // Requests
  // ---------------------------------------------------------------------------

  // Every laboratory request carries the instance it means; the handler resolves
  // it against this package's enabled instances and evaluates THAT instance's
  // matrix before anything else happens.
  tq(kind, payload = {}) {
    return ApiBinary.action(kind, { instanceId: this.instanceId, ...payload });
  },

  async loadLabs() {
    try {
      const res = await ApiBinary.one('tentaQuantLabListRequest', {});
      this.labs = res.labs || [];
      this.canCreate = Boolean(res.canCreate);
      return true;
    } catch (e) {
      this.root.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('labs.load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return false;
    }
  },

  async loadLab() {
    this.lab = this.labs.find((l) => l.instanceId === this.instanceId) || null;
    const [overview, projects] = await Promise.all([
      this.tq('tentaQuantLabOverviewRequest').then((r) => r, (e) => errMessage(e)),
      this.tq('tentaQuantProjectListRequest', { includeArchived: this.includeArchived })
        .then((r) => r.projects || [], (e) => errMessage(e)),
    ]);
    if (typeof overview === 'string') { this.overview = null; this.overviewError = overview; } else { this.overview = overview; this.overviewError = ''; }
    if (typeof projects === 'string') { this.projects = []; this.projectsError = projects; } else { this.projects = projects; this.projectsError = ''; }
  },

  async reloadProjects() {
    if (this.disposed || !this.instanceId) return;
    try {
      const res = await this.tq('tentaQuantProjectListRequest', { includeArchived: this.includeArchived });
      this.projects = res.projects || [];
      this.projectsError = '';
    } catch (e) {
      this.projectsError = errMessage(e);
    }
    // The counters follow the same write, but a failure here is the dashboard's
    // problem, not the project list's — reporting it as one would blank a list
    // that loaded perfectly well.
    try {
      this.overview = await this.tq('tentaQuantLabOverviewRequest');
      this.overviewError = '';
    } catch (e) {
      this.overviewError = errMessage(e);
    }
    if (!this.disposed) this.drawTab();
  },

  // ---------------------------------------------------------------------------
  // Navigation
  // ---------------------------------------------------------------------------

  setLocation() {
    const q = new URLSearchParams();
    if (this.instanceId) q.set('instance', this.instanceId);
    if (this.instanceId && this.tab !== 'dashboard') q.set('tab', this.tab);
    const qs = q.toString();
    const hash = '#/tentaquant' + (qs ? '?' + qs : '');
    if (window.location.hash !== hash) window.history.replaceState(null, '', hash);
  },

  async enter() {
    this.setLocation();
    if (!this.instanceId) { drawLabs(this); return; }
    this.root.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await this.loadLab();
    if (this.disposed) return;
    this.drawLab();
  },

  async openLab(instanceId) {
    this.instanceId = instanceId;
    this.tab = 'dashboard';
    await this.enter();
  },

  async backToLabs() {
    this.instanceId = null;
    this.lab = null;
    this.overview = null;
    this.projects = [];
    await this.enter();
  },

  openAddons() {
    Router.navigate('addons');
  },

  // A new laboratory is a new INSTANCE of this package, and instances are
  // installed in Addons — this screen has no wizard of its own and must not
  // grow one. The route carries the package so Addons opens its install window
  // straight away instead of dropping the user in the catalog to find it.
  openNewLab() {
    Router.navigate('addons', { install: PACKAGE_ID });
  },

  selectTab(tab, { focusProject = null } = {}) {
    if (!TABS.includes(tab)) return;
    this.tab = tab;
    this.focusProject = focusProject;
    this.setLocation();
    const tabs = this.root.querySelector('#tq-tabs');
    if (tabs) tabs.setAttribute('value', tab);
    this.drawTab();
  },

  // ---------------------------------------------------------------------------
  // Laboratory view
  // ---------------------------------------------------------------------------

  headerHtml() {
    const lab = this.lab;
    const ready = labIsReady(lab);
    const nodes = (lab.nodes || []).map((n) => `<tf-chip status="${n.online && n.instanceStatus === 'ready' ? 'ok' : 'warn'}" mono label="${escapeAttr(`${n.nodeName} · ${nodeStateLabel(n)}`)}"></tf-chip>`).join('');
    const people = isSolo(lab)
      ? T('labs.access_only_you')
      : T('lab.header_people', { n: Number(lab.peopleCount) || 0 });
    return `
      <div class="tf-detail-header">
        <div class="big-ico tq-ico">${sprite('atom')}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(lab.displayName || lab.instanceId)}
            <tf-chip status="${ready ? 'ok' : 'warn'}" dot label="${escapeAttr(ready ? T('lab.status_ready') : T('lab.status_not_ready'))}"></tf-chip>
            ${lab.enabled ? '' : `<tf-chip status="warn" label="${escapeAttr(T('labs.disabled'))}"></tf-chip>`}
          </div>
          <div class="d-sub mono">${escapeHtml(T('lab.header_id', { id: lab.instanceId }))} · ${escapeHtml(people)}</div>
          <div class="d-badges">
            <span class="tier t0">${escapeHtml(T('lab.tier_t0'))}</span>
            <span class="tier t1${ready ? '' : ' off'}">${escapeHtml(T('lab.tier_t1'))}</span>
            ${nodes}
            <span class="your-role">${sprite('user')}${escapeHtml(T('lab.your_role', { role: roleLabel(lab.myPermissions), perms: permissionSummary(lab.myPermissions) }))}</span>
          </div>
        </div>
        <div class="d-actions">
          <tf-button variant="primary" icon="plus" data-act="new-project" ${has(lab.myPermissions, 'quant.run') ? '' : 'disabled'}>${escapeHtml(T('lab.action_new_project'))}</tf-button>
        </div>
      </div>`;
  },

  drawLab() {
    if (!this.lab) {
      this.root.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('lab.not_found'))}" message="${escapeAttr(T('lab.not_found_sub'))}"></tf-alert>`;
      return;
    }
    const projectCount = this.projects.filter((p) => !p.archivedAt).length;
    this.root.innerHTML = `
      <tf-breadcrumb class="tq-crumbs">
        <tf-breadcrumb-item href="#/tentaquant" data-crumb="root">${escapeHtml(T('title'))}</tf-breadcrumb-item>
        <tf-breadcrumb-item current>${escapeHtml(this.lab.displayName || this.lab.instanceId)}</tf-breadcrumb-item>
      </tf-breadcrumb>
      ${this.headerHtml()}
      <tf-tabs variant="underline" value="${escapeAttr(this.tab)}" id="tq-tabs">
        <tf-tab id="dashboard" icon="home">${escapeHtml(T('lab.tab_dashboard'))}</tf-tab>
        <tf-tab id="projects" icon="folder" count="${projectCount}">${escapeHtml(T('lab.tab_projects'))}</tf-tab>
      </tf-tabs>
      <div id="tq-panel"></div>`;

    this.root.querySelector('tf-breadcrumb.tq-crumbs').addEventListener('click', (e) => {
      const link = e.target.closest('a.tf-breadcrumb-item');
      if (!link) return;
      e.preventDefault();
      this.backToLabs();
    });
    this.root.querySelector('#tq-tabs').addEventListener('change', (e) => {
      if (e.detail.value !== this.tab) this.selectTab(e.detail.value);
    });
    this.root.querySelector('[data-act="new-project"]').addEventListener('click', () => this.openNewProject());
    this.drawTab();
  },

  drawTab() {
    const panel = this.root.querySelector('#tq-panel');
    if (!panel) return;
    if (this.tab === 'projects') {
      if (this.projectsError) {
        panel.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('projects.load_failed'))}" message="${escapeAttr(this.projectsError)}"></tf-alert>`;
        return;
      }
      drawProjects(this, panel);
      return;
    }
    drawDashboard(this, panel);
  },

  // ---------------------------------------------------------------------------
  // Project actions
  // ---------------------------------------------------------------------------

  openNewProject() {
    openNewProjectWindow(this);
  },

  openShare(projectId) {
    openShareWindow(this, projectId);
  },

  async setIncludeArchived(value) {
    this.includeArchived = value;
    await this.reloadProjects();
  },

  async setArchived(projectId, archived) {
    try {
      await this.tq('tentaQuantProjectArchiveRequest', { projectId, archived });
      toast(T(archived ? 'projects.archived_ok' : 'projects.unarchived_ok'), 'success');
      await this.reloadProjects();
    } catch (e) {
      toast(`${T('projects.action_failed')}: ${errMessage(e)}`, 'error');
    }
  },

  async confirmDelete(projectId) {
    const project = this.projects.find((p) => p.projectId === projectId);
    if (!project) return;
    if (!await confirmDeleteProject(project.name)) return;
    try {
      await this.tq('tentaQuantProjectDeleteRequest', { projectId });
      toast(T('projects.deleted_ok', { name: project.name }), 'success');
      await this.reloadProjects();
    } catch (e) {
      toast(`${T('projects.action_failed')}: ${errMessage(e)}`, 'error');
    }
  },
};

export default TentaQuantScreen;
