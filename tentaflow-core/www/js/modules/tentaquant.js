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
//       Tabs stop at Pulpit, Projekty and Runy on purpose: Urządzenia,
//       Przykłady, Kurs and Ustawienia arrive with their backends. Nothing here
//       renders a section whose data does not exist.
//
//       A project is the second level of the same screen (`?project=…&ptab=…`):
//       the laboratory stays in the breadcrumb and the project draws its own
//       tabs — Notatnik, Studio obwodów, Runy projektu, Pliki — for the same
//       reason. One run may be named by the route as well (`&run=…`), which is
//       what "Otwórz run" hands back after a T1 run.
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
import { PROJECT_TABS, drawProject, drawProjectTab } from '/js/modules/tentaquant/project.js';
import { studioState } from '/js/modules/tentaquant/studio.js';
import { drawRuns, runFilterState } from '/js/modules/tentaquant/runs.js';
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

const TABS = ['dashboard', 'projects', 'runs'];

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
    this.projectId = params.project || null;
    this.projectTab = PROJECT_TABS.includes(params.ptab) ? params.ptab : 'notebook';
    this.project = null;
    this.notebooks = [];
    this.files = [];
    this.notebookId = null;
    this.studio = studioState();
    // A project view owns a wasm simulator and an animation frame; whatever
    // draws one leaves the handle that releases them here.
    this.projectViewDispose = null;
    // ...and a view that holds unsaved work (the notebook) leaves the question
    // that has to be answered before the handle above is pulled.
    this.projectViewGuard = null;
    this.lab = null;
    this.overview = null;
    this.overviewError = '';
    this.projects = [];
    this.projectsError = '';
    this.runs = [];
    this.runsError = '';
    this.runFilters = runFilterState();
    // Which listing the loaded rows are: the whole laboratory (null) or one
    // project. A redraw has to reload the SAME listing, or the project tab
    // would quietly start showing the laboratory.
    this.runsProjectId = null;
    this.runsHost = null;
    this.runId = params.run || null;
    // The run detail owns a live stream while its run is going; whatever draws
    // one leaves the handle that stops it here.
    this.runViewDispose = null;
    this.includeArchived = false;
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

  /// The router's leave guard. `unmount` is told, not asked, and the open
  /// notebook holds cells that exist nowhere but in that view until a save
  /// lands — so the same question the project tabs ask also stands between this
  /// screen and every other view of the dashboard.
  canUnmount() {
    return this.confirmLeaveProjectView();
  },

  unmount() {
    this.disposed = true;
    this.disposeProjectView();
    this.disposeRunView();
    document.querySelectorAll('tf-window.tq-modal').forEach((w) => w.remove());
  },

  /// The open run detail hands back the stop for the stream it follows; every
  /// path that replaces the panel it lives in pulls it first.
  setRunViewDispose(dispose) {
    this.disposeRunView();
    this.runViewDispose = dispose;
  },

  disposeRunView() {
    const dispose = this.runViewDispose;
    this.runViewDispose = null;
    if (dispose) dispose();
  },

  disposeProjectView() {
    const dispose = this.projectViewDispose;
    this.projectViewDispose = null;
    this.projectViewGuard = null;
    if (dispose) dispose();
  },

  /// Asks the open project view whether it may be dropped. False keeps the
  /// screen where it is — the view still holds work that exists nowhere else.
  async confirmLeaveProjectView() {
    const guard = this.projectViewGuard;
    return guard ? guard() : true;
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

  /// Opens one stream of this laboratory and answers the unsubscribe. Views go
  /// through the screen for the same reason they do for unary requests: the
  /// instance is the screen's business, and a view driven by a fake screen is a
  /// view that can be tested.
  tqSubscribe(kind, payload = {}, handlers = {}) {
    return ApiBinary.subscribe(kind, { instanceId: this.instanceId, ...payload }, handlers);
  },

  /// Transport lifecycle, so a stream can resume after the socket came back.
  onTransport(callback) {
    return ApiBinary.onLifecycle(callback);
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
    const [overview, projects, runs] = await Promise.all([
      this.tq('tentaQuantLabOverviewRequest').then((r) => r, (e) => errMessage(e)),
      this.tq('tentaQuantProjectListRequest', { includeArchived: this.includeArchived })
        .then((r) => r.projects || [], (e) => errMessage(e)),
      // The laboratory listing, which both the Runy tab and the dashboard's
      // "ostatnie runy" section read; a project tab narrows it on entry.
      this.tq('tentaQuantRunListRequest', {}).then((r) => r.runs || [], (e) => errMessage(e)),
    ]);
    if (typeof overview === 'string') { this.overview = null; this.overviewError = overview; } else { this.overview = overview; this.overviewError = ''; }
    if (typeof projects === 'string') { this.projects = []; this.projectsError = projects; } else { this.projects = projects; this.projectsError = ''; }
    if (typeof runs === 'string') { this.runs = []; this.runsError = runs; } else { this.runs = runs; this.runsError = ''; }
    this.runsProjectId = null;
  },

  /// Re-reads the run listing the screen is showing and repaints it in place.
  /// `projectId` names the listing; leaving it out keeps the one already on
  /// screen, so an action's reload cannot widen a project tab to the whole lab.
  async reloadRuns({ projectId = this.runsProjectId } = {}) {
    if (this.disposed || !this.instanceId) return;
    this.runsProjectId = projectId || null;
    try {
      const res = await this.tq('tentaQuantRunListRequest', projectId ? { projectId } : {});
      this.runs = res.runs || [];
      this.runsError = '';
    } catch (e) {
      this.runs = [];
      this.runsError = errMessage(e);
    }
    if (this.disposed || !this.runsHost || !this.runsHost.isConnected) return;
    this.disposeRunView();
    drawRuns(this, this.runsHost, { projectId: this.runsProjectId });
  },

  /// Loads and draws a run listing into `host`. Both tabs go through it, so
  /// the loading state, the narrowing and the redraw handle are one thing.
  async showRuns(host, { projectId = null } = {}) {
    this.runsHost = host;
    host.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await this.reloadRuns({ projectId });
  },

  /// Opens (or closes, with null) one run's detail and records it in the route.
  selectRun(runId) {
    this.runId = runId || null;
    if (!this.runId) this.disposeRunView();
    this.setLocation();
  },

  /// Jumps from a run started elsewhere — the Studio, a notebook cell — to that
  /// run's row in the laboratory's Runy tab.
  openRun(runId) {
    this.runId = runId;
    if (this.projectId) { this.selectProjectTab('runs'); return; }
    this.selectTab('runs');
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
    if (this.projectId) {
      q.set('project', this.projectId);
      if (this.projectTab !== 'notebook') q.set('ptab', this.projectTab);
    }
    if (this.runId) q.set('run', this.runId);
    const qs = q.toString();
    const hash = '#/tentaquant' + (qs ? '?' + qs : '');
    if (window.location.hash !== hash) window.history.replaceState(null, '', hash);
  },

  async enter() {
    this.setLocation();
    this.disposeProjectView();
    this.disposeRunView();
    this.runsHost = null;
    if (!this.instanceId) { drawLabs(this); return; }
    this.root.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await this.loadLab();
    if (this.disposed) return;
    if (this.projectId) { await this.enterProject(); return; }
    this.drawLab();
  },

  async openLab(instanceId) {
    this.instanceId = instanceId;
    this.tab = 'dashboard';
    this.projectId = null;
    await this.enter();
  },

  async backToLabs() {
    if (!await this.confirmLeaveProjectView()) return;
    this.instanceId = null;
    this.projectId = null;
    this.project = null;
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

  selectTab(tab) {
    if (!TABS.includes(tab)) return;
    this.tab = tab;
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
        <tf-tab id="runs" icon="clock" count="${this.runs.length}">${escapeHtml(T('lab.tab_runs'))}</tf-tab>
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
    // Only the Runy tab keeps a live view; every other tab replaces the panel
    // it lived in, so the stream goes with it.
    this.disposeRunView();
    if (this.tab !== 'runs') this.runsHost = null;
    if (this.tab === 'runs') {
      this.showRuns(panel, {});
      return;
    }
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
  // One project (Q06, Q07 and the file list)
  // ---------------------------------------------------------------------------

  async loadProject() {
    const [project, notebooks, files] = await Promise.all([
      this.tq('tentaQuantProjectGetRequest', { projectId: this.projectId }).then((r) => r.project, () => null),
      this.tq('tentaQuantNotebookListRequest', { projectId: this.projectId }).then((r) => r.notebooks || [], () => []),
      this.tq('tentaQuantFileListRequest', { projectId: this.projectId }).then((r) => r.files || [], () => []),
    ]);
    this.project = project;
    this.notebooks = notebooks;
    this.files = files;
    if (!notebooks.some((n) => n.notebookId === this.notebookId)) this.notebookId = null;
  },

  async enterProject() {
    this.root.innerHTML = `<div class="tq-loading">${escapeHtml(I18n.t('common.loading'))}</div>`;
    await this.loadProject();
    if (this.disposed) return;
    drawProject(this);
  },

  async openProject(projectId, { tab = 'notebook' } = {}) {
    this.projectId = projectId;
    this.projectTab = PROJECT_TABS.includes(tab) ? tab : 'notebook';
    // A project is opened fresh: the Studio's working circuit belongs to the
    // project that was open, never to the next one.
    this.studio = studioState();
    this.notebookId = null;
    this.setLocation();
    await this.enterProject();
  },

  async closeProject() {
    if (!await this.confirmLeaveProjectView()) return;
    this.disposeProjectView();
    this.projectId = null;
    this.project = null;
    this.notebooks = [];
    this.files = [];
    this.tab = 'projects';
    await this.enter();
  },

  async selectProjectTab(tab) {
    if (!PROJECT_TABS.includes(tab) || tab === this.projectTab) return;
    const tabs = this.root.querySelector('#tq-project-tabs');
    // tf-tabs has already moved its own highlight by the time the change event
    // arrives, so a refused switch has to put it back on the tab that stayed.
    if (!await this.confirmLeaveProjectView()) {
      if (tabs) tabs.setAttribute('value', this.projectTab);
      return;
    }
    this.projectTab = tab;
    this.setLocation();
    if (tabs) tabs.setAttribute('value', tab);
    drawProjectTab(this);
  },

  // The notebook hands a circuit cell to the Studio; the Studio saves back into
  // that cell, which is why it carries the ids and not just the program.
  openStudioWithCell({ notebookId, cellId, source, name }) {
    this.studio = studioState({ notebookId, cellId, source, name });
    return this.selectProjectTab('studio');
  },

  async reloadNotebooks() {
    try {
      const res = await this.tq('tentaQuantNotebookListRequest', { projectId: this.projectId });
      this.notebooks = res.notebooks || [];
    } catch (e) {
      toast(`${T('notebook.load_failed')}: ${errMessage(e)}`, 'error');
    }
  },

  async reloadFiles() {
    try {
      const res = await this.tq('tentaQuantFileListRequest', { projectId: this.projectId });
      this.files = res.files || [];
    } catch (e) {
      toast(`${T('files.action_failed')}: ${errMessage(e)}`, 'error');
      return;
    }
    if (this.disposed) return;
    const tab = this.root.querySelector('#tq-project-tabs tf-tab#files');
    if (tab) tab.setAttribute('count', String(this.files.length));
    if (this.projectTab === 'files') drawProjectTab(this);
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
