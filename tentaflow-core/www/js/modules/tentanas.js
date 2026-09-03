// =============================================================================
// File: modules/tentanas.js — the TentaNas screen (plan-02, mockups n01–n18).
//       One screen, two views: the fleet grid (no node selected) and the node
//       view with six tabs (overview, disks, pools, shares, tasks,
//       environment). The pools tab (list, wizard, pool detail with
//       datasets/snapshots), the shares tab (SMB/NFS, wizard, users, fleet
//       mounts), the config export/import and the tasks tab live in
//       modules/tentanas/*; this file stays the shell: navigation, header,
//       overview, disks, environment and the privilege plumbing (sudo prompt,
//       channel wizard, job log) the modules call back into. Every request
//       goes through `nas()` which adds the envelope
//       forward target when the selected node is not the local one — the
//       admin manages any node from any node, the core forwards over the mesh.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import {
  T, sprite, POLL_DISKS_MS, POLL_OVERVIEW_MS, IO_WINDOW_SECS, TEMP_WINDOW_SECS, POLL_FLEET_MS, POLL_JOB_MODAL_MS, ADMIN_TIMEOUT_MS,
  parseServerTs, fmtDate, fmtAgo, fmtDuration, fmtWindow, fmtBytes, fmtMBps, pct, healthClass, healthChip, errMessage, jobTone, jobKindLabel,
  layoutLabel, stateChipHtml, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { drawPools, poolDescription } from '/js/modules/tentanas/pools.js';
import { drawPoolDetail, openReplaceWizard } from '/js/modules/tentanas/pool-detail.js';
import { openPoolWizard } from '/js/modules/tentanas/pool-wizard.js';
import { drawTasks, openSmartScheduleEditor } from '/js/modules/tentanas/tasks.js';
import { drawShares, protocolChipHtml } from '/js/modules/tentanas/shares.js';
import { warningHtml } from '/js/modules/tentanas/dialogs.js';
import { exportConfig, openConfigImportDialog, mountImportPicker, applyImport, planBlocked } from '/js/modules/tentanas/config-transfer.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-slider.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-key-value.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-progress-bar.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-section-card.js';
import '/js/components/tf-choice-card.js';
import '/js/components/tf-line-chart.js';
import '/js/components/tf-stream-chart.js';

// -----------------------------------------------------------------------------
// Screen-local helpers
// -----------------------------------------------------------------------------

function sparklineSvg(points, cls = '', w = 90, h = 22) {
  const pts = (points || []).map(Number).filter((v) => Number.isFinite(v));
  if (pts.length < 2) return '<svg viewBox="0 0 90 22"></svg>';
  const max = Math.max(1, ...pts);
  const step = w / (pts.length - 1);
  const coords = pts.map((v, i) => `${(i * step).toFixed(1)},${(h - 1 - (v / max) * (h - 2)).toFixed(1)}`).join(' ');
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none"><polyline class="${escapeAttr(cls)}" points="${coords}"/></svg>`;
}

// The one breadcrumb of the screen; every level but the last carries a
// `data-crumb` action the caller wires, plus the hash the level really points
// at so the rendered link survives a middle-click or a copy.
function crumbsHtml(items) {
  return `<tf-breadcrumb class="nas-crumbs">${items.map((it, i) => (i === items.length - 1
    ? `<tf-breadcrumb-item current>${escapeHtml(it.label)}</tf-breadcrumb-item>`
    : `<tf-breadcrumb-item href="${escapeAttr('#/tentanas' + (it.query ? '?' + it.query : ''))}" data-crumb="${escapeAttr(it.act)}">${escapeHtml(it.label)}</tf-breadcrumb-item>`)).join('')}</tf-breadcrumb>`;
}

// tf-breadcrumb renders its links into a <nav> SIBLING of the
// <tf-breadcrumb-item> elements, so a listener on an item never sees the
// click — the container delegates instead and maps link order to item order
// (only the non-current items become links, in the same sequence).
function wireCrumbs(root, handlers) {
  root.querySelectorAll('tf-breadcrumb.nas-crumbs').forEach((nav) => {
    const acts = [...nav.querySelectorAll('tf-breadcrumb-item')].map((i) => i.dataset.crumb || '');
    nav.addEventListener('click', (e) => {
      const link = e.target.closest('a.tf-breadcrumb-item');
      if (!link) return;
      e.preventDefault();
      const fn = handlers[acts[[...nav.querySelectorAll('a.tf-breadcrumb-item')].indexOf(link)]];
      if (fn) fn();
    });
  });
}

// ARC hit ratio (n02): a conic-gradient ring with the percentage inside.
function donutHtml(pctValue, label) {
  const p = Math.max(0, Math.min(100, Number(pctValue) || 0));
  return `<div class="donut" style="background: conic-gradient(var(--success) 0 ${p}%, var(--bg-3) ${p}% 100%);">
      <div class="dn-center"><div class="dn-val">${p.toFixed(1)}%</div><div class="dn-lbl">${escapeHtml(label)}</div></div>
    </div>`;
}

// One square per fleet node, in node order: the mount state of a share.
function mountDotsHtml(mounts, nodes) {
  return `<span class="mount-dots">${nodes.map((n) => {
    const m = (mounts || []).find((x) => x.nodeId === n.nodeId);
    const state = m ? m.state : 'na';
    const cls = state === 'mounted' || state === 'source' ? '' : state === 'pending' ? 'pending' : state === 'error' ? 'error' : 'na';
    return `<span class="md ${cls}" title="${escapeAttr(`${n.nodeName}: ${m ? (m.detail || m.state) : T('fleet.mount_na')}`)}"></span>`;
  }).join('')}</span>`;
}

// -----------------------------------------------------------------------------
// Screen
// -----------------------------------------------------------------------------

const TentaNasScreen = {
  get title() { return T('title'); },

  render() {
    return '<div id="nas-root" class="nas-root"></div>';
  },

  async mount(params = {}) {
    this.root = byId('nas-root');
    this.timers = new Set();
    this.disposed = false;
    this.me = null;
    this.localNodeId = null;
    this.nodes = [];
    this.nodeId = params.node || null;
    this.tab = params.tab || 'overview';
    this.diskId = params.disk || null;
    // Pools tab: the open pool, its inner tab and the dataset it focuses on
    // survive a reload through the hash (n06/n09).
    this.pool = params.pool || null;
    this.poolTab = params.ptab || 'topology';
    this.dataset = params.dataset || null;
    this.diskFilter = 'all';
    this.diskQuery = '';
    this.diskPool = 'all';
    this.diskPoolSig = null;
    this.diskSelection = new Set();
    this.isAdmin = false;
    this.runningJobs = 0;
    // The fleet aggregate is per-mount: a screen re-entered after a config
    // change must re-ask every node instead of painting the old fold.
    this.fleet = null;
    this.fleetVersion = null;

    try {
      const me = await ApiBinary.one('authMeRequest', {});
      this.me = me;
      this.isAdmin = Boolean(me && (me.role === 'admin' || me.isAdmin));
    } catch (e) {
      this.root.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }

    await this.loadNodes();
    if (this.disposed) return;
    if (this.nodeId && !this.nodes.some((n) => n.nodeId === this.nodeId)) this.nodeId = null;
    this.draw();
  },

  unmount() {
    this.disposed = true;
    this.clearTimers();
    if (this.openWindow) { this.openWindow.remove(); this.openWindow = null; }
  },

  clearTimers() {
    for (const t of this.timers) clearTimeout(t);
    this.timers.clear();
  },

  // Schedules `fn` once; the callee re-arms itself after each successful
  // paint so a slow node never stacks requests.
  later(fn, ms) {
    if (this.disposed) return;
    const t = setTimeout(() => { this.timers.delete(t); if (!this.disposed) fn(); }, ms);
    this.timers.add(t);
  },

  // Forwarded requests target the selected node; the local node answers
  // directly. Admin actions get a long deadline because provisioning and
  // package installs answer only after the job is enqueued on the far node.
  nas(kind, payload = {}, opts = {}) {
    return this.nasOn(this.currentNode(), kind, payload, opts);
  },

  // Same envelope rule for a node that is not the selected one — the fleet
  // aggregation and the "arm another node" action address a node directly.
  nasOn(node, kind, payload = {}, opts = {}) {
    const forward = node && !node.isLocal ? { targetNodeId: node.nodeId } : {};
    return ApiBinary.action(kind, payload, { ...forward, ...opts });
  },

  currentNode() {
    return this.nodes.find((n) => n.nodeId === this.nodeId) || null;
  },

  async loadNodes() {
    try {
      const res = await ApiBinary.one('tentaNasNodesListRequest', {});
      this.localNodeId = res.localNodeId;
      this.nodes = (res.nodes || []).map(normalizeNode);
    } catch (e) {
      toast(T('nodes_failed', { error: errMessage(e) }), 'error');
      if (!this.nodes.length) this.nodes = [];
    }
  },

  setLocation(extra = {}) {
    const q = new URLSearchParams();
    if (this.nodeId) q.set('node', this.nodeId);
    if (this.nodeId && this.tab !== 'overview') q.set('tab', this.tab);
    if (this.nodeId && this.diskId) q.set('disk', this.diskId);
    if (this.nodeId && this.tab === 'pools' && this.pool) {
      q.set('pool', this.pool);
      if (this.poolTab && this.poolTab !== 'topology') q.set('ptab', this.poolTab);
      if (this.dataset) q.set('dataset', this.dataset);
    }
    for (const [k, v] of Object.entries(extra)) if (v != null) q.set(k, v);
    const qs = q.toString();
    const hash = '#/tentanas' + (qs ? '?' + qs : '');
    if (window.location.hash !== hash) window.history.replaceState(null, '', hash);
  },

  draw() {
    if (this.disposed) return;
    this.clearTimers();
    this.setLocation();
    if (!this.nodeId) this.drawFleet();
    else this.drawNode();
  },

  selectNode(nodeId, tab = null, extra = {}) {
    this.nodeId = nodeId;
    this.diskId = extra.disk || null;
    this.pool = extra.pool || null;
    this.dataset = null;
    this.diskFilter = extra.diskFilter || 'all';
    this.tab = tab || this.tab || 'overview';
    this.draw();
  },

  // Six tabs, identical on the fleet grid and the node view; on the fleet the
  // strip carries no active tab because the tabs belong to a node — clicking
  // one opens that tab on the default node.
  tabsHtml(active, node) {
    const n = node || {};
    const running = Number(this.runningJobs) || 0;
    return `
      <tf-tabs variant="underline" value="${escapeAttr(active || '')}" id="nas-tabs">
        <tf-tab id="overview" icon="bar-chart">${escapeHtml(T('tabs.overview'))}</tf-tab>
        <tf-tab id="disks" icon="cylinder" count="${Number(n.disksTotal) || 0}">${escapeHtml(T('tabs.disks'))}</tf-tab>
        <tf-tab id="pools" icon="layers" count="${Number(n.poolsTotal) || 0}">${escapeHtml(T('tabs.pools'))}</tf-tab>
        <tf-tab id="shares" icon="share" count="${Number(n.sharesTotal) || 0}">${escapeHtml(T('tabs.shares'))}</tf-tab>
        <tf-tab id="jobs" icon="list" ${running ? `count="${running}" count-tone="accent"` : ''}>${escapeHtml(T('tabs.jobs'))}</tf-tab>
        <tf-tab id="environment" icon="os">${escapeHtml(T('tabs.environment'))}</tf-tab>
      </tf-tabs>`;
  },

  wireTabs(el, defaultNode) {
    if (!el) return;
    el.addEventListener('change', (e) => {
      const value = e.detail.value;
      if (!this.nodeId) {
        el.setAttribute('value', '');
        if (defaultNode) this.selectNode(defaultNode.nodeId, value);
        return;
      }
      if (value === this.tab) return;
      this.tab = value;
      this.diskId = null;
      this.pool = null;
      this.dataset = null;
      this.clearTimers();
      this.setLocation();
      this.drawTab();
    });
  },

  // The Zadania tab counts what is running right now; every place that already
  // has a job list feeds it instead of polling again.
  setJobsBadge(jobs) {
    this.runningJobs = (jobs || []).filter((j) => j.status === 'running' || j.status === 'queued').length;
    const tab = this.root?.querySelector('#nas-tabs tf-tab#jobs');
    if (!tab) return;
    if (this.runningJobs) { tab.setAttribute('count', String(this.runningJobs)); tab.setAttribute('count-tone', 'accent'); } else tab.removeAttribute('count');
  },

  // ---------------------------------------------------------------------------
  // Fleet view (n01)
  // ---------------------------------------------------------------------------

  // The fleet has no aggregated request: every supported node answers its own
  // alerts and shares and the screen folds them together. A node that fails to
  // answer keeps a row of its own — an unreachable node is a fleet fact, not
  // something to hide.
  async loadFleetData() {
    const supported = this.nodes.filter((n) => n.instanceStatus === 'ready');
    const rows = await Promise.all(supported.map(async (n) => {
      const [alerts, shares] = await Promise.all([
        this.nasOn(n, 'tentaNasAlertsListRequest', { includeAcked: false }).then((r) => r.alerts || [], (e) => errMessage(e)),
        this.nasOn(n, 'tentaNasSharesListRequest', {}).then((r) => r, (e) => errMessage(e)),
      ]);
      return { node: n, alerts, shares };
    }));
    if (this.fleetVersion == null && supported.length) {
      const env = await this.nasOn(supported[0], 'tentaNasEnvironmentRequest', { refresh: false }).catch(() => null);
      this.fleetVersion = env?.environment?.elevation?.coreVersion || '';
    }
    this.fleet = { rows, at: new Date().toISOString() };
  },

  fleetShares() {
    return (this.fleet?.rows || []).flatMap((r) => (typeof r.shares === 'string' ? [] : (r.shares.shares || []).map((s) => ({ share: s, node: r.node }))));
  },

  fleetServicesUp() {
    return (this.fleet?.rows || []).some((r) => typeof r.shares !== 'string' && (r.shares.services || []).some((s) => s.running));
  },

  drawFleet() {
    this.clearTimers();
    const nodes = this.nodes;
    const ready = nodes.filter((n) => n.instanceStatus === 'ready');
    const warnNodes = nodes.filter((n) => n.disksWarning > 0);
    const warnDisks = nodes.reduce((a, n) => a + n.disksWarning, 0);
    const cap = nodes.reduce((a, n) => a + n.capacityBytes, 0);
    const used = nodes.reduce((a, n) => a + n.usedBytes, 0);
    const pools = nodes.reduce((a, n) => a + n.poolsTotal, 0);
    const nasNodes = ready.filter((n) => n.poolsTotal > 0);
    const unarmed = ready.filter((n) => (n.elevationMode || 'unarmed') === 'unarmed');
    const shares = this.fleetShares();
    const loaded = Boolean(this.fleet);

    const channelParts = ['helper', 'interactive', 'unarmed']
      .map((mode) => ({ mode, n: ready.filter((n) => (n.elevationMode || 'unarmed') === mode).length }))
      .filter((p) => p.n > 0)
      .map((p) => T('fleet.badge_channel_part', { n: p.n, mode: T('elevation.short_' + p.mode) }))
      .join(' · ');

    const sub = [
      T('fleet.head_scope'),
      T('fleet.head_nodes', { n: nodes.length }),
      T('fleet.head_supported', { n: ready.length }),
      this.fleetVersion ? T('fleet.head_version', { v: this.fleetVersion }) : null,
      this.fleet ? T('refreshed', { t: fmtAgo(this.fleet.at) }) : null,
    ].filter(Boolean).join(' · ');

    const protoCounts = [...new Set(shares.map((s) => s.share.protocol))]
      .map((p) => T('fleet.kpi_protocol', { n: shares.filter((s) => s.share.protocol === p).length, protocol: p.toUpperCase() }))
      .join(' · ');

    this.root.innerHTML = `
      ${crumbsHtml([{ label: T('title') }])}
      <div class="tf-detail-header">
        <div class="big-ico">${sprite('cylinder')}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(T('title'))}
            <tf-chip status="${warnDisks ? 'warn' : 'ok'}" dot label="${escapeAttr(warnDisks
              ? T('fleet.chip_warnings', { n: warnDisks, nodes: warnNodes.map((n) => n.nodeName).join(', ') })
              : T('fleet.chip_ok'))}"></tf-chip>
            ${loaded ? `<tf-chip status="${this.fleetServicesUp() ? 'ok' : 'warn'}" dot label="${escapeAttr(this.fleetServicesUp() ? T('fleet.chip_services') : T('fleet.chip_services_down'))}"></tf-chip>` : ''}
          </div>
          <div class="d-sub">${escapeHtml(sub)}</div>
          <div class="d-badges">
            <tf-chip status="accent" label="${escapeAttr(T('fleet.badge_nas', { n: nasNodes.length, nodes: nasNodes.map((n) => n.nodeName).join(' · ') }))}"></tf-chip>
            <tf-chip status="${unarmed.length ? 'warn' : 'ok'}" icon="shield" label="${escapeAttr(T('fleet.badge_channels', { parts: channelParts || '—' }))}"></tf-chip>
            <tf-chip label="${escapeAttr(T('fleet.badge_pools', { n: pools, capacity: fmtBytes(cap) }))}"></tf-chip>
            <tf-chip status="info" icon="network" label="${escapeAttr(T('fleet.badge_mesh', { n: nodes.length }))}"></tf-chip>
          </div>
        </div>
        <div class="d-actions">
          <tf-button variant="ghost" icon="download" data-act="export-config">${escapeHtml(T('config.export'))}</tf-button>
          <tf-button variant="ghost" icon="refresh" data-act="refresh">${escapeHtml(T('reprobe'))}</tf-button>
        </div>
      </div>
      ${this.tabsHtml(null, ready[0] || null)}
      <div class="kpi">
        <tf-stat-card label="${escapeAttr(T('kpi.fleet_capacity'))}" value="${escapeAttr(fmtBytes(cap))}" icon="database"
          delta="${escapeAttr(T('kpi.capacity_delta', { used: fmtBytes(used), pct: pct(used, cap), n: pools }))}"></tf-stat-card>
        <tf-stat-card id="nas-fleet-health" class="clickable" label="${escapeAttr(T('kpi.fleet_health'))}" value="${warnDisks}" suffix="${escapeAttr(T('kpi.warnings_suffix', { n: warnDisks }))}" icon="cylinder"
          ${warnDisks ? `accent="warning" delta="${escapeAttr(T('kpi.fleet_health_on', { nodes: warnNodes.map((n) => n.nodeName).join(', ') }))}" delta-type="negative"` : `delta="${escapeAttr(T('kpi.fleet_health_ok'))}"`}></tf-stat-card>
        <tf-stat-card id="nas-fleet-res" class="clickable" label="${escapeAttr(T('kpi.fleet_resources'))}" value="${loaded ? shares.length : '—'}" icon="share"
          ${protoCounts ? `delta="${escapeAttr(protoCounts)}"` : ''}></tf-stat-card>
        <tf-stat-card label="${escapeAttr(T('kpi.nodes'))}" value="${ready.length}" suffix="${escapeAttr(T('kpi.fleet_nodes_suffix', { total: nodes.length }))}" icon="network"
          ${unarmed.length ? `delta="${escapeAttr(T('kpi.node_unarmed', { node: unarmed[0].nodeName }))}" delta-type="negative"` : ''}></tf-stat-card>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('desktop')} ${escapeHtml(T('fleet.nodes_title'))}</div>
          <span class="hint">${escapeHtml(T('fleet.nodes_hint'))}</span>
        </div>
        <div class="node-grid" id="nas-node-grid">${nodes.map((n) => this.nodeCardHtml(n)).join('')}</div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('alert')} ${escapeHtml(T('fleet.alerts_title'))} <tf-chip size="sm" status="${this.fleetAlertRows().length ? 'err' : 'neutral'}" label="${this.fleetAlertRows().length}"></tf-chip></div>
          <div class="actions"><tf-button variant="ghost" size="sm" icon="clock" data-act="alert-history">${escapeHtml(T('alerts.history'))}</tf-button></div>
        </div>
        <tf-table id="nas-fleet-alerts" empty-message="${escapeAttr(loaded ? T('fleet.alerts_none') : I18n.t('common.loading'))}">
          <tf-column key="level" label="${escapeAttr(T('fleet.col_level'))}" renderer="html" width="110"></tf-column>
          <tf-column key="node" label="${escapeAttr(T('fleet.col_node'))}" renderer="html" width="120"></tf-column>
          <tf-column key="alert" label="${escapeAttr(T('fleet.col_alert'))}" renderer="html" fill></tf-column>
          <tf-column key="since" label="${escapeAttr(T('fleet.col_since'))}" renderer="text" nowrap width="110"></tf-column>
        </tf-table>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('share')} ${escapeHtml(T('fleet.resources_title'))}</div>
          <span class="hint">${escapeHtml(T('fleet.resources_hint'))}</span>
        </div>
        <tf-table id="nas-fleet-res-table" empty-message="${escapeAttr(loaded ? T('fleet.resources_none') : I18n.t('common.loading'))}">
          <tf-column key="resource" label="${escapeAttr(T('fleet.col_resource'))}" renderer="html" fill></tf-column>
          <tf-column key="protocol" label="${escapeAttr(T('fleet.col_protocol'))}" renderer="html" nowrap></tf-column>
          <tf-column key="source" label="${escapeAttr(T('fleet.col_source'))}" renderer="html"></tf-column>
          <tf-column key="mounts" label="${escapeAttr(T('fleet.col_mounts'))}" renderer="html" nowrap width="140"></tf-column>
          <tf-column key="sessions" label="${escapeAttr(T('fleet.col_sessions'))}" renderer="num" width="90"></tf-column>
        </tf-table>
      </div>`;

    wireCrumbs(this.root, {});
    this.root.querySelector('[data-act="refresh"]').addEventListener('click', () => this.refreshFleet());
    this.root.querySelector('[data-act="export-config"]').addEventListener('click', () => exportConfig(this));
    this.root.querySelector('[data-act="alert-history"]').addEventListener('click', () => {
      const target = this.fleetAlertRows().find((r) => r.node)?.node || ready[0];
      if (target) this.selectNode(target.nodeId, 'jobs');
    });
    this.root.querySelector('#nas-fleet-health').addEventListener('click', () => {
      const target = warnNodes[0] || ready[0];
      if (target) this.selectNode(target.nodeId, 'disks', { diskFilter: 'problems' });
    });
    this.root.querySelector('#nas-fleet-res').addEventListener('click', () => {
      const target = shares[0]?.node || ready[0];
      if (target) this.selectNode(target.nodeId, 'shares');
    });
    this.wireTabs(this.root.querySelector('#nas-tabs'), ready[0] || null);
    this.root.querySelector('#nas-node-grid').addEventListener('click', (e) => {
      const card = e.target.closest('.node-card[data-node]');
      if (!card || card.classList.contains('unsupported')) return;
      this.selectNode(card.dataset.node);
    });
    this.paintFleetAlerts();
    this.paintFleetResources();

    if (!this.fleet) this.loadFleetData().then(() => { if (!this.disposed && !this.nodeId) this.drawFleet(); });
    this.later(() => this.refreshFleet(), POLL_FLEET_MS);
  },

  async refreshFleet() {
    await this.loadNodes();
    await this.loadFleetData();
    if (!this.disposed && !this.nodeId) this.drawFleet();
  },

  // One row per active alert plus one row per node that did not answer.
  fleetAlertRows() {
    return (this.fleet?.rows || []).flatMap((r) => (typeof r.alerts === 'string'
      ? [{ node: r.node, error: r.alerts }]
      : r.alerts.map((a) => ({ node: r.node, alert: a }))));
  },

  paintFleetAlerts() {
    const table = this.root.querySelector('#nas-fleet-alerts');
    if (!table) return;
    table.rows = this.fleetAlertRows().map((r) => (r.error ? {
      _row: r,
      level: `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('fleet.node_offline'))}"></tf-chip>`,
      node: `<span class="mono">${escapeHtml(r.node.nodeName)}</span>`,
      alert: escapeHtml(T('fleet.node_unreachable', { error: r.error })),
      since: '—',
    } : {
      _row: r,
      level: `<tf-chip size="sm" status="${r.alert.severity === 'critical' ? 'err' : r.alert.severity === 'warning' ? 'warn' : 'info'}" dot label="${escapeAttr(T('health.' + (r.alert.severity === 'critical' ? 'critical' : r.alert.severity === 'warning' ? 'warning' : 'ok')))}"></tf-chip>`,
      node: `<span class="mono">${escapeHtml(r.node.nodeName)}</span>`,
      alert: `<div class="cell-2"><div class="l1">${escapeHtml(r.alert.title)}</div><div class="l2">${escapeHtml(r.alert.detail)}</div></div>`,
      since: fmtAgo(r.alert.raisedAt),
    }));
    table.rowActions = (row) => {
      const { node, alert } = row._row;
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      const target = alertTarget(alert);
      wrap.innerHTML = `<tf-button size="sm" variant="secondary" icon="chevron-right" data-act="go">${escapeHtml(T('fleet.act_' + target.act))}</tf-button>`;
      wrap.querySelector('[data-act="go"]').addEventListener('click', (e) => { e.stopPropagation(); this.selectNode(node.nodeId, target.tab, target.extra); });
      return wrap;
    };
  },

  paintFleetResources() {
    const table = this.root.querySelector('#nas-fleet-res-table');
    if (!table) return;
    const offline = (this.fleet?.rows || []).filter((r) => typeof r.shares === 'string');
    table.rows = [
      ...this.fleetShares().map(({ share, node }) => ({
        _share: share,
        _node: node,
        resource: `<span class="fw-700">${escapeHtml(share.name)}</span>`,
        protocol: protocolChipHtml(share.protocol),
        source: `<span class="mono">${escapeHtml(share.dataset || share.sourcePath)}</span>`,
        mounts: share.fleetMount ? mountDotsHtml(share.mounts, this.nodes) : `<span class="text-3 text-xs">${escapeHtml(T('shares.fleet_off'))}</span>`,
        sessions: share.sessions,
      })),
      ...offline.map((r) => ({
        _node: r.node,
        resource: `<span class="mono">${escapeHtml(r.node.nodeName)}</span>`,
        protocol: `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('fleet.node_offline'))}"></tf-chip>`,
        source: escapeHtml(T('fleet.node_unreachable', { error: r.shares })),
        mounts: '—',
        sessions: 0,
      })),
    ];
    table.rowActions = (row) => {
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      wrap.innerHTML = `<tf-button size="sm" variant="secondary" icon="chevron-right" data-act="go">${escapeHtml(T('fleet.act_manage'))}</tf-button>`;
      wrap.querySelector('[data-act="go"]').addEventListener('click', (e) => { e.stopPropagation(); this.selectNode(row._node.nodeId, 'shares'); });
      return wrap;
    };
  },

  nodeCardHtml(n) {
    const unsupported = n.instanceStatus !== 'ready';
    const cls = ['node-card', unsupported ? 'unsupported' : '', !n.online ? 'offline' : ''].filter(Boolean).join(' ');
    const usedPct = pct(n.usedBytes, n.capacityBytes);
    const statusChip = unsupported
      ? `<tf-chip status="warn" label="${escapeAttr(T('instance.' + n.instanceStatus))}"></tf-chip>`
      : !n.online
        ? `<tf-chip status="info" label="${escapeAttr(T('offline'))}"></tf-chip>`
        : `<tf-chip status="${healthChip(n.health).status}" dot label="${escapeAttr(healthChip(n.health).label)}"></tf-chip>`;
    const sub = [n.osName || '—', n.zfsVersion ? `ZFS ${n.zfsVersion}` : null, n.isLocal ? T('this_node') : null].filter(Boolean).join(' · ');
    return `
      <div class="${cls}" data-node="${escapeAttr(n.nodeId)}">
        <div class="nc-head">
          ${sprite('desktop')}
          <div style="flex:1;min-width:0">
            <div class="nc-name">${escapeHtml(n.nodeName)} ${statusChip}</div>
            <div class="nc-sub">${escapeHtml(sub)}</div>
          </div>
        </div>
        <div class="nc-stats">
          <div class="st"><b>${n.disksTotal}</b><span>${escapeHtml(T('kpi.disks'))}${n.disksWarning ? ` · <span class="num-warn">${n.disksWarning}!</span>` : ''}</span></div>
          <div class="st"><b>${n.poolsTotal}</b><span>${escapeHtml(T('kpi.pools'))}</span></div>
          <div class="st"><b>${n.sharesTotal}</b><span>${escapeHtml(T('kpi.shares'))}</span></div>
          <div class="st"><b>${n.alertsActive}</b><span>${escapeHtml(T('kpi.alerts'))}</span></div>
        </div>
        <div class="split-bar" title="${usedPct}%"><span class="${usedPct > 90 ? 'err' : usedPct > 75 ? 'warn' : ''}" style="width:${usedPct}%"></span></div>
        <div class="nc-foot">
          <span>${escapeHtml(fmtBytes(n.usedBytes))} / ${escapeHtml(fmtBytes(n.capacityBytes))}</span>
          <span>${escapeHtml(T('elevation.mode_' + (n.elevationMode || 'unarmed')))}</span>
        </div>
      </div>`;
  },

  // ---------------------------------------------------------------------------
  // Node view (n02 header + tabs)
  // ---------------------------------------------------------------------------

  async drawNode() {
    const node = this.currentNode();
    this.root.innerHTML = `
      ${crumbsHtml([{ label: T('title'), act: 'fleet' }, { label: node.nodeName }])}
      <div class="tf-detail-header">
        <div class="big-ico">${sprite('cylinder')}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(T('title'))} <span id="nas-head-chips"></span></div>
          <div class="d-sub" id="nas-head-sub">${escapeHtml(T('node.head_sub_node', { name: node.nodeName }))}</div>
          <div class="d-badges" id="nas-head-badges"></div>
        </div>
        <div class="d-actions">
          <tf-select id="nas-node-select"></tf-select>
          <tf-button variant="ghost" icon="download" data-act="export-config">${escapeHtml(T('config.export'))}</tf-button>
          <tf-button variant="ghost" icon="refresh" data-act="reprobe">${escapeHtml(T('reprobe'))}</tf-button>
        </div>
      </div>
      ${this.tabsHtml(this.tab, node)}
      <div id="nas-tab-body"></div>
    `;

    wireCrumbs(this.root, { fleet: () => { this.nodeId = null; this.diskId = null; this.draw(); } });
    const sel = this.root.querySelector('#nas-node-select');
    sel.setOptions(this.nodes.map((n) => ({
      value: n.nodeId,
      label: n.nodeName + (n.isLocal ? ` (${T('this_node')})` : '') + (n.instanceStatus !== 'ready' ? ` — ${T('instance.' + n.instanceStatus)}` : ''),
      disabled: n.instanceStatus !== 'ready',
    })), this.nodeId);
    sel.addEventListener('change', (e) => { if (e.detail.value !== this.nodeId) this.selectNode(e.detail.value); });
    this.root.querySelector('[data-act="reprobe"]').addEventListener('click', () => this.reprobe());
    this.root.querySelector('[data-act="export-config"]').addEventListener('click', () => exportConfig(this));
    this.wireTabs(this.root.querySelector('#nas-tabs'), node);

    this.refreshHeader();
    this.refreshJobsBadge();
    this.drawTab();
  },

  async refreshJobsBadge() {
    const res = await this.nas('tentaNasJobsListRequest', { limit: 100 }).catch(() => null);
    if (this.disposed || !res) return;
    this.setJobsBadge(res.jobs || []);
  },

  async refreshHeader(refresh = false) {
    try {
      const res = await this.nas('tentaNasEnvironmentRequest', { refresh });
      if (this.disposed || !this.root.querySelector('#nas-head-badges')) return;
      this.environment = res.environment;
      const env = res.environment;
      const node = this.currentNode();
      const zfs = (env.features || []).find((f) => f.id === 'zfs');
      const servicesUp = (env.features || []).some((f) => ['samba', 'nfs', 'iscsi', 'nvmet'].includes(f.id) && f.status === 'ok');
      this.root.querySelector('#nas-head-chips').innerHTML = [
        `<tf-chip status="${node.disksWarning ? 'warn' : 'ok'}" dot label="${escapeAttr(node.disksWarning ? T('node.chip_disks_warn', { n: node.disksWarning }) : T('node.chip_ok'))}"></tf-chip>`,
        `<tf-chip status="${servicesUp ? 'ok' : 'warn'}" dot label="${escapeAttr(servicesUp ? T('fleet.chip_services') : T('fleet.chip_services_down'))}"></tf-chip>`,
      ].join('');
      const badges = [
        zfs && zfs.version ? `<tf-chip status="accent" label="${escapeAttr(T('node.badge_zfs', { v: zfs.version }))}"></tf-chip>` : `<tf-chip status="warn" label="${escapeAttr(T('env.no_zfs'))}"></tf-chip>`,
        `<tf-chip status="${env.elevation.mode === 'unarmed' ? 'warn' : 'ok'}" icon="${env.elevation.mode === 'unarmed' ? 'lock' : 'shield'}" label="${escapeAttr(T('node.badge_channel', { mode: T('elevation.short_' + env.elevation.mode) }))}"></tf-chip>`,
        env.fullSupport ? '' : `<tf-chip status="warn" label="${escapeAttr(T('env.partial_support'))}"></tf-chip>`,
        `<tf-chip status="info" icon="network" label="${escapeAttr(T('fleet.badge_mesh', { n: this.nodes.length }))}"></tf-chip>`,
      ];
      this.root.querySelector('#nas-head-badges').innerHTML = badges.join('');
      const sub = [
        T('node.head_sub_node', { name: node.nodeName }),
        T('uptime', { d: fmtDuration(env.uptimeSecs) }),
        env.elevation.coreVersion ? T('fleet.head_version', { v: env.elevation.coreVersion }) : null,
        T('refreshed', { t: fmtAgo(env.probedAt) }),
      ];
      this.root.querySelector('#nas-head-sub').textContent = sub.filter(Boolean).join(' · ');
    } catch (e) {
      if (!this.disposed) toast(T('env.failed', { error: errMessage(e) }), 'error');
    }
  },

  async reprobe() {
    await this.refreshHeader(true);
    if (this.tab === 'environment') this.drawTab();
  },

  drawTab() {
    const body = this.root.querySelector('#nas-tab-body');
    if (!body) return;
    body.innerHTML = '';
    switch (this.tab) {
      case 'disks': return this.diskId ? this.drawDiskDetail(body) : this.drawDisks(body);
      case 'pools': return this.pool ? drawPoolDetail(this, body) : drawPools(this, body);
      case 'shares': return drawShares(this, body);
      case 'jobs': return drawTasks(this, body);
      case 'environment': return this.drawEnvironment(body);
      default: return this.drawOverview(body);
    }
  },

  // ---------------------------------------------------------------------------
  // Overview tab (n02)
  // ---------------------------------------------------------------------------

  async drawOverview(body) {
    body.innerHTML = `
      <div class="stack">
        <div class="kpi" id="nas-ov-kpi"></div>
        <div id="nas-ov-telemetry"></div>
        <div class="section-card">
          <div class="section-card-head">
            <div class="title">${sprite('database')} ${escapeHtml(T('arc.title'))}</div>
            <div class="actions" id="nas-ov-arc-actions"></div>
          </div>
          <div id="nas-ov-arc"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
        </div>
        <div class="grid-2">
          <div class="section-card">
            <div class="chart-head">
              <div class="ch-title">${sprite('trend')} ${escapeHtml(T('overview.io_title'))}</div>
              <div class="ch-val" id="nas-ov-io-val"></div>
            </div>
            <tf-stream-chart id="nas-ov-io"></tf-stream-chart>
            <div class="live-label"><span class="live-dot"></span>${escapeHtml(T('overview.live_window', { w: fmtWindow(IO_WINDOW_SECS) }))}</div>
          </div>
          <div class="section-card">
            <div class="chart-head">
              <div class="ch-title">${sprite('zap')} ${escapeHtml(T('overview.temp_title'))}</div>
              <div class="ch-val" id="nas-ov-temp-val"></div>
            </div>
            <tf-stream-chart id="nas-ov-temp"></tf-stream-chart>
            <div class="live-label"><span class="live-dot"></span>${escapeHtml(T('overview.live_window', { w: fmtWindow(TEMP_WINDOW_SECS) }))}</div>
          </div>
        </div>
        <div class="grid-2">
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('tabs.pools'))}</div>
              <div class="actions"><tf-button variant="secondary" size="sm" icon="plus" data-act="create-pool">${escapeHtml(T('pools.create'))}</tf-button></div></div>
            <div id="nas-ov-pools"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
          </div>
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('bell')} ${escapeHtml(T('alerts.title'))} <tf-chip size="sm" id="nas-ov-alerts-count" status="neutral" label="0"></tf-chip></div>
              <div class="actions"><tf-button variant="ghost" size="sm" icon="clock" data-act="alert-history">${escapeHtml(T('alerts.history'))}</tf-button></div></div>
            <div id="nas-ov-alerts"></div>
            <div id="nas-ov-jobs" class="mt-sm"></div>
          </div>
        </div>
      </div>`;
    body.querySelector('[data-act="alert-history"]').addEventListener('click', () => this.switchTab('jobs'));
    body.querySelector('[data-act="create-pool"]').addEventListener('click', () => {
      openPoolWizard(this, { freeDisks: this.overviewFreeDisks || [], pools: this.overviewPools || [], onDone: () => this.refreshOverview(body) });
    });

    const io = body.querySelector('#nas-ov-io');
    io.height = 150;
    io.window = IO_WINDOW_SECS;
    io.legend = { position: 'none' };
    io.tooltip = { valueFormat: (v) => `${fmtMBps(v)} MB/s` };
    io.yAxis = { min: 0, ticks: 4, format: (v) => fmtMBps(v) };
    io.series = [
      { id: 'read', name: T('disk.legend_read'), tone: 'primary', style: 'solid', showInLegend: false, points: [] },
      { id: 'write', name: T('disk.legend_write'), tone: 'info', style: 'solid', showInLegend: false, points: [] },
    ];
    const temp = body.querySelector('#nas-ov-temp');
    temp.height = 150;
    temp.window = TEMP_WINDOW_SECS;
    temp.legend = { position: 'none' };
    temp.fill = false;
    temp.tooltip = { valueFormat: (v) => `${Math.round(v)}°C` };
    temp.yAxis = { min: 0, ticks: 4, format: (v) => `${v}°` };
    temp.series = [
      { id: 'max', name: T('overview.temp_max'), tone: 'warning', style: 'solid', showInLegend: false, points: [] },
      { id: 'avg', name: T('overview.temp_avg'), tone: 'info', style: 'solid', showInLegend: false, points: [] },
    ];
    await this.refreshOverview(body);
  },

  // Every poll becomes one sample on both live charts; the charts keep their
  // own window, this only feeds them and refreshes the header readouts.
  pushOverviewSamples(body, disks, read, write, maxTemp, hottest) {
    const now = Date.now();
    const io = body.querySelector('#nas-ov-io');
    if (io) {
      io.push(now, { read, write });
      body.querySelector('#nas-ov-io-val').innerHTML =
        `<span class="sw primary"></span>${escapeHtml(T('disk.legend_read'))} ${escapeHtml(fmtMBps(read))} MB/s&nbsp;&nbsp;<span class="sw info"></span>${escapeHtml(T('disk.legend_write'))} ${escapeHtml(fmtMBps(write))} MB/s`;
    }
    const temp = body.querySelector('#nas-ov-temp');
    if (temp) {
      const temps = disks.map((d) => d.temperatureC).filter((t) => t != null);
      const avg = temps.length ? temps.reduce((a, t) => a + t, 0) / temps.length : null;
      const sample = {};
      if (maxTemp != null) sample.max = maxTemp;
      if (avg != null) sample.avg = avg;
      temp.push(now, sample);
      body.querySelector('#nas-ov-temp-val').innerHTML = maxTemp == null
        ? escapeHtml(T('overview.temp_none'))
        : `<span class="sw warning"></span>${escapeHtml(T('overview.temp_max'))} ${maxTemp}°C${hottest ? ` (${escapeHtml(hottest)})` : ''}&nbsp;&nbsp;<span class="sw info"></span>${escapeHtml(T('overview.temp_avg'))} ${Math.round(avg)}°C`;
    }
  },

  switchTab(tab) {
    this.tab = tab;
    this.diskId = null;
    this.pool = null;
    this.dataset = null;
    this.clearTimers();
    this.setLocation();
    const tabs = this.root.querySelector('#nas-tabs');
    if (tabs) tabs.setAttribute('value', tab);
    this.drawTab();
  },

  // Opens a pool inside the pools tab (from a card, an alert or a disk's
  // "member of" link); `poolTab` picks the inner tab, `dataset` focuses one
  // row of the datasets/snapshots tab.
  openPool(name, poolTab = 'topology', dataset = null) {
    this.pool = name;
    this.poolTab = poolTab;
    this.dataset = dataset;
    if (this.tab !== 'pools') {
      this.tab = 'pools';
      const tabs = this.root.querySelector('#nas-tabs');
      if (tabs) tabs.setAttribute('value', 'pools');
    }
    this.diskId = null;
    this.clearTimers();
    this.setLocation();
    this.drawTab();
  },

  async refreshOverview(body) {
    let disksRes, jobsRes, alertsRes, poolsRes, arcRes;
    try {
      [disksRes, jobsRes, alertsRes, poolsRes, arcRes] = await Promise.all([
        this.nas('tentaNasDisksListRequest', {}),
        this.nas('tentaNasJobsListRequest', { limit: 20 }),
        this.nas('tentaNasAlertsListRequest', { includeAcked: false }),
        this.nas('tentaNasPoolsListRequest', {}),
        this.nas('tentaNasArcStatsRequest', {}).catch(() => ({ arc: null })),
      ]);
    } catch (e) {
      if (this.disposed) return;
      body.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }
    if (this.disposed || !body.isConnected) return;

    const disks = disksRes.disks || [];
    const warned = disks.filter((d) => d.health === 'warning' || d.health === 'critical');
    const read = disks.reduce((a, d) => a + (Number(d.io?.readBps) || 0), 0);
    const write = disks.reduce((a, d) => a + (Number(d.io?.writeBps) || 0), 0);
    const iops = Math.round(disks.reduce((a, d) => a + (Number(d.io?.readIops) || 0) + (Number(d.io?.writeIops) || 0), 0));
    const awaits = disks.map((d) => Number(d.io?.awaitMs) || 0).filter((v) => v > 0);
    const latency = awaits.length ? (awaits.reduce((a, v) => a + v, 0) / awaits.length) : 0;
    const temps = disks.map((d) => d.temperatureC).filter((t) => t != null);
    const maxTemp = temps.length ? Math.max(...temps) : null;
    const hottest = maxTemp == null ? null : (disks.find((d) => d.temperatureC === maxTemp) || {}).name;
    const jobs = jobsRes.jobs || [];
    const running = jobs.filter((j) => j.status === 'running' || j.status === 'queued');
    const alerts = alertsRes.alerts || [];
    const pools = poolsRes.pools || [];
    this.overviewPools = pools;
    this.overviewFreeDisks = poolsRes.freeDisks || [];
    const cap = pools.reduce((a, p) => a + (Number(p.usableBytes) || 0), 0) || disks.reduce((a, d) => a + (Number(d.sizeBytes) || 0), 0);
    const used = pools.reduce((a, p) => a + (Number(p.usedBytes) || 0), 0);
    this.setJobsBadge(jobs);

    const kpi = body.querySelector('#nas-ov-kpi');
    if (!kpi) return;
    kpi.innerHTML = `
      <tf-stat-card data-kpi="pools" class="clickable" label="${escapeAttr(T('kpi.capacity_total'))}" value="${escapeAttr(fmtBytes(cap))}" icon="database"
        delta="${escapeAttr(T('kpi.capacity_delta', { used: fmtBytes(used), pct: pct(used, cap), n: pools.length }))}"></tf-stat-card>
      <tf-stat-card data-kpi="disks" class="clickable" label="${escapeAttr(T('kpi.disk_health'))}" value="${warned.length}" suffix="${escapeAttr(T('kpi.warnings_suffix', { n: warned.length }))}" icon="cylinder"
        ${warned.length
          ? `accent="warning" delta="${escapeAttr(warned.slice(0, 3).map((d) => `${d.name}: ${d.healthReason}`).join(' · '))}" delta-type="negative"`
          : `delta="${escapeAttr(T('kpi.disk_health_ok'))}"`}></tf-stat-card>
      <tf-stat-card label="${escapeAttr(T('kpi.iops'))}" value="${iops}" icon="trend"
        delta="${escapeAttr(T('kpi.iops_delta', { r: fmtMBps(read), w: fmtMBps(write) }))}"></tf-stat-card>
      <tf-stat-card label="${escapeAttr(T('kpi.throughput'))}" value="${escapeAttr(fmtMBps(read))}" suffix="${escapeAttr(T('kpi.throughput_suffix'))}" icon="zap"
        delta="${escapeAttr(T('kpi.throughput_delta', { w: fmtMBps(write), lat: latency.toFixed(1) }))}"></tf-stat-card>`;
    kpi.querySelector('[data-kpi="pools"]').addEventListener('click', () => this.switchTab('pools'));
    kpi.querySelector('[data-kpi="disks"]').addEventListener('click', () => { this.diskFilter = warned.length ? 'problems' : 'all'; this.switchTab('disks'); });

    this.pushOverviewSamples(body, disks, read, write, maxTemp, hottest);

    body.querySelector('#nas-ov-telemetry').innerHTML = this.telemetryAlertHtml(disksRes.telemetry);
    this.wireTelemetryAlert(body.querySelector('#nas-ov-telemetry'));

    this.paintArcCard(body, arcRes.arc);
    this.paintPoolsMini(body, pools);

    body.querySelector('#nas-ov-alerts-count').setAttribute('label', String(alerts.length));
    body.querySelector('#nas-ov-alerts-count').setAttribute('status', alerts.length ? 'err' : 'neutral');
    this.renderAlertList(body.querySelector('#nas-ov-alerts'), alerts, () => this.refreshOverview(body));

    const jobsEl = body.querySelector('#nas-ov-jobs');
    jobsEl.innerHTML = running.map((j) => this.jobRowHtml(j)).join('');
    this.wireJobRows(jobsEl, () => this.refreshOverview(body));

    this.later(() => this.refreshOverview(body), POLL_OVERVIEW_MS);
  },

  // n02 ARC card: the hit-ratio ring plus the five rows that say where the
  // cache memory went and what backs it (SLOG, L2ARC).
  paintArcCard(body, arc) {
    const host = body.querySelector('#nas-ov-arc');
    const actions = body.querySelector('#nas-ov-arc-actions');
    if (!host || !actions) return;
    if (!arc) {
      host.innerHTML = `<div class="muted">${escapeHtml(T('arc.unavailable'))}</div>`;
      actions.innerHTML = '';
      return;
    }
    const ramPct = arc.ramBytes ? Math.round((Number(arc.maxBytes) || 0) / Number(arc.ramBytes) * 100) : 0;
    const mru = Number(arc.mruBytes) || 0;
    const mfu = Number(arc.mfuBytes) || 0;
    const demand = Number(arc.demandHits) || 0;
    const prefetch = Number(arc.prefetchHits) || 0;
    const l2 = (arc.l2arcPools || []);
    const biggest = (this.overviewPools || []).slice().sort((a, b) => (Number(b.sizeBytes) || 0) - (Number(a.sizeBytes) || 0))[0];
    const rows = [
      [T('arc.row_usage'), `${escapeHtml(fmtBytes(arc.sizeBytes))} / ${escapeHtml(fmtBytes(arc.maxBytes))}`],
      [T('arc.row_split'), `${pct(mru, mru + mfu)}% / ${pct(mfu, mru + mfu)}%`],
      [T('arc.row_demand'), `${pct(demand, demand + prefetch)}% / ${pct(prefetch, demand + prefetch)}%`],
      [T('arc.row_slog'), (arc.slogPools || []).length ? `<span class="mono">${escapeHtml(arc.slogPools.join(', '))}</span>` : `<span class="text-3">${escapeHtml(T('arc.slog_none'))}</span>`],
      [T('arc.row_l2arc'), l2.length
        ? `<span class="mono">${escapeHtml(l2.join(', '))}</span>`
        : biggest
          ? `<span class="text-3">${escapeHtml(T('arc.l2arc_none'))} — <a data-act="arc-l2arc">${escapeHtml(T('arc.l2arc_add', { pool: biggest.name }))}</a></span>`
          : `<span class="text-3">${escapeHtml(T('arc.l2arc_none'))}</span>`],
      [T('arc.row_limit_source'), escapeHtml(T('arc.limit_source_' + (arc.limitSource || 'default')))],
    ];
    host.innerHTML = `
      <div class="arc-flex">
        ${donutHtml(arc.hitRatio, T('arc.hit_ratio'))}
        <div class="stat-rows" style="flex:1">${rows.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${v}</span></div>`).join('')}</div>
      </div>`;
    actions.innerHTML = `<tf-button variant="ghost" size="sm" icon="settings" data-act="arc-limit">${escapeHtml(T('arc.change_limit', { pct: ramPct }))}</tf-button>`;
    actions.querySelector('[data-act="arc-limit"]').addEventListener('click', () => this.switchTab('environment'));
    host.querySelector('[data-act="arc-l2arc"]')?.addEventListener('click', () => this.openPool(biggest.name));
  },

  // n02 pool mini-list: name + state, the one-line topology and the fill bar.
  paintPoolsMini(body, pools) {
    const host = body.querySelector('#nas-ov-pools');
    if (!host) return;
    if (!pools.length) {
      host.innerHTML = `<div class="muted">${escapeHtml(T('pools.empty_title'))}</div>`;
      return;
    }
    host.innerHTML = pools.map((p) => `
      <div class="pool-mini" data-pool="${escapeAttr(p.name)}">
        <div class="pm-ico">${sprite('layers')}</div>
        <div class="pm-main">
          <div class="pm-name"><span class="mono">${escapeHtml(p.name)}</span> ${stateChipHtml(p.state)}</div>
          <div class="pm-sub">${escapeHtml(poolDescription(p))}</div>
          <tf-progress-bar value="${pct(p.usedBytes, p.usableBytes)}" size="sm" tone="accent"></tf-progress-bar>
        </div>
        <div class="kv-inline"><span class="v">${escapeHtml(fmtBytes(p.usedBytes))} / ${escapeHtml(fmtBytes(p.usableBytes))}</span></div>
      </div>`).join('');
    host.querySelectorAll('.pool-mini[data-pool]').forEach((el) => el.addEventListener('click', () => this.openPool(el.dataset.pool)));
  },

  telemetryAlertHtml(t) {
    if (!t || t.smartState === 'live') return '';
    if (t.smartState === 'stale_unarmed') {
      return `<tf-alert tone="warning" title="${escapeAttr(T('telemetry.stale_title'))}" message="${escapeAttr(T('telemetry.stale_msg', { t: t.smartReadAt ? fmtAgo(t.smartReadAt) : T('never') }))}">
        ${this.isAdmin ? `<div slot="actions"><tf-button size="sm" variant="primary" icon="unlock" data-act="arm">${escapeHtml(T('elevation.arm'))}</tf-button></div>` : ''}
      </tf-alert>`;
    }
    return `<tf-alert tone="info" title="${escapeAttr(T('telemetry.unavailable_title'))}" message="${escapeAttr(t.detail || '')}"></tf-alert>`;
  },

  wireTelemetryAlert(el) {
    const btn = el && el.querySelector('[data-act="arm"]');
    if (btn) btn.addEventListener('click', () => this.openChannelWizard());
  },

  renderAlertList(el, alerts, onChange) {
    if (!alerts.length) {
      el.innerHTML = `<div class="muted">${escapeHtml(T('alerts.none'))}</div>`;
      return;
    }
    el.innerHTML = alerts.map((a) => `
      <div class="alert-row ${escapeAttr(a.severity)} ${a.ackedAt ? 'acked' : ''}">
        ${sprite(a.severity === 'critical' ? 'alert' : a.severity === 'warning' ? 'alert' : 'info')}
        <div class="a-main">
          <div class="a-title">${escapeHtml(a.title)}</div>
          <div class="a-sub">${escapeHtml(a.detail)} · ${escapeHtml(a.subjectKind)} ${escapeHtml(a.subjectId)} · ${escapeHtml(fmtAgo(a.raisedAt))}</div>
        </div>
        ${a.ackedAt ? `<tf-chip status="info" label="${escapeAttr(T('alerts.acked'))}"></tf-chip>` : `<tf-button size="sm" variant="ghost" icon="check" data-ack="${escapeAttr(a.alertId)}">${escapeHtml(T('alerts.ack'))}</tf-button>`}
      </div>`).join('');
    el.querySelectorAll('[data-ack]').forEach((b) => b.addEventListener('click', async () => {
      try {
        await this.nas('tentaNasAlertAckRequest', { alertId: b.dataset.ack });
        onChange();
      } catch (e) {
        toast(errMessage(e), 'error');
      }
    }));
  },

  // ---------------------------------------------------------------------------
  // Disks tab (n03)
  // ---------------------------------------------------------------------------

  async drawDisks(body) {
    this.diskSelection = this.diskSelection || new Set();
    body.innerHTML = `
      <div class="section-card">
        <div id="nas-disks-telemetry"></div>
        <div class="toolbar">
          <tf-searchbox id="nas-disk-search" placeholder="${escapeAttr(T('disks.search'))}" debounce="150"></tf-searchbox>
          <tf-filter-chips id="nas-disk-filters"></tf-filter-chips>
          <span id="nas-disk-pool-host"></span>
          <span class="ml-auto"></span>
          <tf-button variant="secondary" icon="play" data-act="smart-bulk" disabled>${escapeHtml(T('disks.smart_selected', { n: 0 }))}</tf-button>
        </div>
        <tf-table id="nas-disk-table" selectable="multi" empty-message="${escapeAttr(T('disks.none'))}">
          <tf-column key="health" label="${escapeAttr(T('disks.col_health'))}" renderer="html" width="140"></tf-column>
          <tf-column key="device" label="${escapeAttr(T('disks.col_device'))}" renderer="html" fill></tf-column>
          <tf-column key="model" label="${escapeAttr(T('disks.col_model'))}" renderer="html" hide-below="900"></tf-column>
          <tf-column key="size" label="${escapeAttr(T('disks.col_size'))}" renderer="text" nowrap></tf-column>
          <tf-column key="role" label="${escapeAttr(T('disks.col_role'))}" renderer="chip"></tf-column>
          <tf-column key="temp" label="${escapeAttr(T('disks.col_temp'))}" renderer="html" nowrap></tf-column>
          <tf-column key="rw" label="${escapeAttr(T('disks.col_rw'))}" renderer="text" nowrap hide-below="1100"></tf-column>
          <tf-column key="lat" label="${escapeAttr(T('disks.col_lat'))}" renderer="text" nowrap hide-below="1100"></tf-column>
          <tf-column key="wear" label="${escapeAttr(T('disks.col_wear'))}" renderer="html" nowrap hide-below="1200"></tf-column>
          <tf-column key="trend" label="${escapeAttr(T('disks.col_trend'))}" renderer="html" hide-below="1000"></tf-column>
        </tf-table>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('info')} ${escapeHtml(T('disks.legend_title'))}</div>
          <span class="hint">${escapeHtml(T('disks.legend_hint'))}</span>
        </div>
        <div class="legend-strip">
          <span class="li"><tf-chip size="sm" status="ok" dot label="${escapeAttr(T('health.ok'))}"></tf-chip>${escapeHtml(T('disks.legend_ok'))}</span>
          <span class="li"><tf-chip size="sm" status="warn" dot label="${escapeAttr(T('health.warning'))}"></tf-chip>${escapeHtml(T('disks.legend_warn'))}</span>
          <span class="li"><tf-chip size="sm" status="err" dot label="${escapeAttr(T('health.critical'))}"></tf-chip>${escapeHtml(T('disks.legend_crit'))}</span>
          <span class="li text-3">${sprite('info')} ${escapeHtml(T('disks.legend_note', { w: fmtWindow(IO_WINDOW_SECS) }))}</span>
        </div>
      </div>`;

    const filters = body.querySelector('#nas-disk-filters');
    filters.addEventListener('change', (e) => { this.diskFilter = e.detail.id; this.applyDiskRows(); });
    body.querySelector('#nas-disk-search').addEventListener('search', (e) => { this.diskQuery = (e.detail.value || '').trim().toLowerCase(); this.applyDiskRows(); });
    body.querySelector('[data-act="smart-bulk"]').addEventListener('click', () => this.startSmartTestBulk());

    const table = body.querySelector('#nas-disk-table');
    table.rowActions = (row) => {
      const d = row._disk;
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      wrap.innerHTML = `
        <tf-button size="sm" variant="ghost" icon="${d.locateActive ? 'eye' : 'search'}" data-act="locate" title="${escapeAttr(T('disks.locate'))}"></tf-button>
        <tf-button size="sm" variant="ghost" icon="play" data-act="smart" title="${escapeAttr(T('disks.smart_test'))}"></tf-button>
        ${d.role === 'free' ? `<tf-button size="sm" variant="ghost" icon="layers" data-act="use" title="${escapeAttr(T('disks.use_in_pool'))}"></tf-button>` : ''}
        <tf-button size="sm" variant="secondary" icon="chevron-right" data-act="details">${escapeHtml(T('disks.details'))}</tf-button>`;
      wrap.querySelector('[data-act="details"]').addEventListener('click', (e) => { e.stopPropagation(); this.openDisk(d.diskId); });
      wrap.querySelector('[data-act="locate"]').addEventListener('click', (e) => { e.stopPropagation(); this.locateDisk(d, !d.locateActive); });
      wrap.querySelector('[data-act="smart"]').addEventListener('click', (e) => { e.stopPropagation(); this.startSmartTest(d); });
      wrap.querySelector('[data-act="use"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.openPoolWizardForDisk(); });
      return wrap;
    };
    table.addEventListener('row-click', (e) => this.openDisk(e.detail.row._disk.diskId));
    table.addEventListener('row-select', (e) => {
      const id = e.detail.row?._disk?.diskId;
      if (!id) return;
      if (e.detail.selected) this.diskSelection.add(id); else this.diskSelection.delete(id);
      this.paintSmartBulkButton();
    });
    table.addEventListener('select-all', (e) => {
      const visible = table.rows || [];
      for (const row of visible) {
        if (e.detail.selected) this.diskSelection.add(row._disk.diskId); else this.diskSelection.delete(row._disk.diskId);
      }
      this.applyDiskRows();
    });

    this.locateState = this.locateState || {};
    await this.refreshDisks(body);
  },

  async openPoolWizardForDisk() {
    const res = await this.nas('tentaNasPoolsListRequest', {}).catch((e) => { toast(errMessage(e), 'error'); return null; });
    if (!res) return;
    openPoolWizard(this, { freeDisks: res.freeDisks || [], pools: res.pools || [], onDone: () => this.drawTab() });
  },

  paintSmartBulkButton() {
    const btn = this.root.querySelector('[data-act="smart-bulk"]');
    if (!btn) return;
    const n = this.diskSelection.size;
    btn.textContent = T('disks.smart_selected', { n });
    if (n) btn.removeAttribute('disabled'); else btn.setAttribute('disabled', '');
  },

  // One password for the whole batch: the prompt appears once and every
  // selected disk starts its short self-test with it.
  async startSmartTestBulk() {
    const targets = (this.disks || []).filter((d) => this.diskSelection.has(d.diskId));
    if (!targets.length) return;
    const ok = await this.withSudo(async (sudoPassword) => {
      for (const d of targets) await this.nas('tentaNasDiskSmartTestRequest', { diskId: d.diskId, kind: 'short', sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS });
      return true;
    }, T('disks.smart_selected_title', { n: targets.length }));
    if (!ok) return;
    toast(T('disks.smart_selected_done', { n: targets.length }), 'success');
    this.diskSelection.clear();
    this.applyDiskRows();
    this.refreshJobsBadge();
  },

  async refreshDisks(body) {
    try {
      const res = await this.nas('tentaNasDisksListRequest', {});
      if (this.disposed || !body.isConnected) return;
      this.disks = res.disks || [];
      this.telemetry = res.telemetry;
      const tel = body.querySelector('#nas-disks-telemetry');
      tel.innerHTML = this.telemetryAlertHtml(res.telemetry);
      this.wireTelemetryAlert(tel);
      this.applyDiskRows();
    } catch (e) {
      if (this.disposed || !body.isConnected) return;
      toast(T('disks.failed', { error: errMessage(e) }), 'error');
    }
    this.later(() => this.refreshDisks(body), POLL_DISKS_MS);
  },

  // The filter chips carry their own counts and the pool selector is built
  // from what the node actually reports, so an empty pool never gets a segment.
  paintDiskFilters() {
    const all = this.disks || [];
    const counts = {
      all: all.length,
      hdd: all.filter((d) => d.kind === 'hdd').length,
      flash: all.filter((d) => d.kind === 'ssd' || d.kind === 'nvme').length,
      problems: all.filter((d) => d.health !== 'ok').length,
      free: all.filter((d) => d.role === 'free').length,
    };
    const chips = this.root.querySelector('#nas-disk-filters');
    if (chips) {
      chips.filters = ['all', 'hdd', 'flash', 'problems', 'free']
        .map((id) => ({ id, label: `${T('disks.filter_' + id)} ${counts[id]}`, active: id === this.diskFilter }));
    }
    const pools = [...new Set(all.map((d) => d.memberOf).filter(Boolean))].sort();
    const host = this.root.querySelector('#nas-disk-pool-host');
    const sig = pools.join('|');
    if (!host || sig === this.diskPoolSig) return;
    this.diskPoolSig = sig;
    if (!pools.includes(this.diskPool)) this.diskPool = 'all';
    host.innerHTML = `
      <tf-segmented id="nas-disk-pool" size="sm" value="${escapeAttr(this.diskPool || 'all')}">
        <option value="all">${escapeHtml(T('disks.pool_filter_all'))}</option>
        ${pools.map((p) => `<option value="${escapeAttr(p)}">${escapeHtml(p)}</option>`).join('')}
      </tf-segmented>`;
    host.querySelector('#nas-disk-pool').addEventListener('change', (e) => { this.diskPool = e.detail.value || 'all'; this.applyDiskRows(); });
  },

  applyDiskRows() {
    const table = this.root.querySelector('#nas-disk-table');
    if (!table) return;
    this.paintDiskFilters();
    const q = this.diskQuery;
    const f = this.diskFilter;
    const list = (this.disks || []).filter((d) => {
      if (f === 'hdd' && d.kind !== 'hdd') return false;
      if (f === 'flash' && d.kind !== 'ssd' && d.kind !== 'nvme') return false;
      if (f === 'problems' && d.health === 'ok') return false;
      if (f === 'free' && d.role !== 'free') return false;
      if (this.diskPool && this.diskPool !== 'all' && d.memberOf !== this.diskPool) return false;
      if (q && ![d.name, d.path, d.model, d.serial, d.wwn].some((s) => (s || '').toLowerCase().includes(q))) return false;
      return true;
    });
    table.rows = list.map((d) => this.diskRow(d));
    this.paintSmartBulkButton();
  },

  diskRow(d) {
    const locateActive = Boolean(this.locateState[d.diskId]);
    const hist = d.ioHistoryBps || [];
    return {
      _disk: { ...d, locateActive },
      _selected: this.diskSelection?.has(d.diskId) || false,
      _class: d.health === 'critical' ? 'row-danger' : '',
      health: `<span class="health-cell"><span class="health-dot ${healthClass(d.health)}"></span>${escapeHtml(T('health.' + d.health))}</span>`,
      device: `<div class="cell-2"><div class="l1"><span class="mono">${escapeHtml(d.name)}</span><span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span></div><div class="l2">${escapeHtml(d.transport || '')}${d.mountpoints && d.mountpoints.length ? ' · ' + escapeHtml(d.mountpoints.join(', ')) : ''}</div></div>`,
      model: `<div class="cell-2"><div class="l1">${escapeHtml(d.model || '—')}</div><div class="l2 mono">${escapeHtml(d.serial || '')}</div></div>`,
      size: fmtBytes(d.sizeBytes),
      role: { status: roleTone(d.role), label: d.memberOf ? `${T('role.' + d.role)} · ${d.memberOf}` : T('role.' + d.role) },
      temp: d.temperatureC == null ? '<span class="text-3">—</span>' : `<span class="${d.temperatureC >= 55 ? 'num-err' : d.temperatureC >= 45 ? 'num-warn' : ''}">${d.temperatureC}°C</span>`,
      rw: `${fmtMBps(d.io?.readBps)} / ${fmtMBps(d.io?.writeBps)}`,
      lat: d.io ? (Number(d.io.awaitMs) || 0).toFixed(1) : '—',
      wear: d.wearPct == null ? '<span class="text-3">—</span>' : `<span class="${d.wearPct >= 90 ? 'num-err' : d.wearPct >= 70 ? 'num-warn' : ''}">${d.wearPct}%</span>`,
      trend: `<div class="trend">${sparklineSvg(hist)}</div>`,
    };
  },

  openDisk(diskId) {
    this.diskId = diskId;
    this.clearTimers();
    this.setLocation();
    this.drawTab();
  },

  async locateDisk(disk, enable) {
    try {
      const res = await this.nas('tentaNasDiskLocateRequest', { diskId: disk.diskId, enable });
      if (res.method === 'none') {
        toast(res.detail || T('disks.locate_unsupported'), 'warning');
        return;
      }
      this.locateState[disk.diskId] = Boolean(res.active);
      toast(res.active ? T('disks.locate_on', { name: disk.name }) : T('disks.locate_off', { name: disk.name }), 'success');
      this.applyDiskRows();
    } catch (e) {
      toast(errMessage(e), 'error');
    }
  },

  // A SMART self-test needs root on the node; without an armed channel the
  // core answers `elevation_required` and the sudo prompt collects the
  // password for that single call.
  async startSmartTest(disk, kind = 'short') {
    const job = await this.withSudo((sudoPassword) => this.nas('tentaNasDiskSmartTestRequest', { diskId: disk.diskId, kind, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('disks.smart_test_title', { name: disk.name }));
    if (!job) return;
    toast(T('jobs.started', { kind: jobKindLabel('smart_test') }), 'success');
    this.refreshJobsBadge();
    if (this.tab === 'jobs') this.drawTab();
  },

  // ---------------------------------------------------------------------------
  // Disk detail (n04)
  // ---------------------------------------------------------------------------

  async drawDiskDetail(body) {
    body.innerHTML = `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`;
    let res;
    try {
      res = await this.nas('tentaNasDiskGetRequest', { diskId: this.diskId });
    } catch (e) {
      if (this.disposed || !body.isConnected) return;
      body.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }
    if (this.disposed || !body.isConnected) return;
    const d = res.disk;
    const attrs = res.attributes || [];
    const tests = res.selfTests || [];
    const history = res.history || [];
    const historyDays = Number(res.historyDays) || 0;
    // The pool's own error counters and the SMART self-test cadence come from
    // two more reads; neither is fatal for the page.
    const [poolRes, schedRes] = await Promise.all([
      d.memberOf ? this.nas('tentaNasPoolGetRequest', { name: d.memberOf }).catch(() => null) : Promise.resolve(null),
      this.nas('tentaNasSchedulesListRequest', {}).catch(() => null),
    ]);
    if (this.disposed || !body.isConnected) return;
    const pool = poolRes?.pool || null;
    const vdev = pool ? (pool.vdevs || []).find((v) => (v.disks || []).some((x) => x.diskId === d.diskId || x.name === d.name)) : null;
    const leaf = vdev ? (vdev.disks || []).find((x) => x.diskId === d.diskId || x.name === d.name) : null;
    const smart = schedRes?.smart || null;

    const field = (k, v) => `<div class="f"><div class="k">${escapeHtml(k)}</div><div class="v">${escapeHtml(v)}</div></div>`;
    const counter = (n) => `<span class="${Number(n) > 0 ? 'num-err' : 'num-ok'}">${Number(n) || 0}</span>`;

    body.innerHTML = `
      <div class="stack">
        ${crumbsHtml([
          { label: T('tabs.disks'), act: 'disks', query: `node=${this.nodeId}&tab=disks` },
          { label: d.name },
        ])}
        <div class="section-card">
          <div class="section-card-head">
            <div class="title">${sprite('cylinder')} ${escapeHtml(T('disk.identification'))}</div>
            <tf-chip status="${healthChip(d.health).status}" dot label="${escapeAttr(healthChip(d.health).label)}"></tf-chip>
          </div>
          <div class="id-grid">
            <div class="id-badge">${sprite('cylinder')}<span class="k">${escapeHtml(d.kind)}</span></div>
            <div class="id-fields">
              ${field(T('disks.col_device'), d.name)}
              ${field(T('disk.serial'), d.serial || '—')}
              ${field('WWN', d.wwn || '—')}
              ${field(T('disk.model'), `${d.model || '—'} · ${fmtBytes(d.sizeBytes)}`)}
              ${field(T('disk.path'), d.path)}
              ${field(T('disk.firmware'), d.firmware || '—')}
              ${field(T('disk.transport'), `${d.transport}${d.rotational ? ` · ${T('disk.rotational')}` : ''}${d.removable ? ` · ${T('disk.removable')}` : ''}`)}
              ${field(T('disks.col_role'), d.memberOf ? `${T('role.' + d.role)} · ${d.memberOf}` : T('role.' + d.role))}
              ${field(T('disk.power_on'), d.powerOnHours == null ? '—' : fmtDuration(d.powerOnHours * 3600))}
              ${field(T('disk.mountpoints'), d.mountpoints && d.mountpoints.length ? d.mountpoints.join(', ') : '—')}
              ${field(T('disk.reallocated'), d.reallocatedSectors == null ? '—' : String(d.reallocatedSectors))}
              ${field(T('disk.pending'), d.pendingSectors == null ? '—' : String(d.pendingSectors))}
              ${field(T('disk.crc'), d.crcErrors == null ? '—' : String(d.crcErrors))}
              ${field(T('disk.media_errors'), d.mediaErrors == null ? '—' : String(d.mediaErrors))}
              ${field(T('disks.col_wear'), d.wearPct == null ? '—' : `${d.wearPct}%`)}
            </div>
            <div class="row">
              <tf-button variant="primary" icon="search" data-act="locate">${escapeHtml(T('disks.locate'))}</tf-button>
              <tf-button variant="secondary" icon="copy" data-act="copy-serial" ${d.serial ? '' : 'disabled'}>${escapeHtml(T('disk.copy_serial'))}</tf-button>
            </div>
          </div>
          ${warningHtml('info', T('disk.led_fallback'))}
        </div>
        <div class="grid-2">
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('info')} ${escapeHtml(T('disk.why_title', { status: healthChip(d.health).label }))}</div></div>
            <div class="explain-box">${escapeHtml(d.healthReason || T('disk.why_ok'))}</div>
            <div class="row mt-md">
              ${d.memberOf ? `<tf-button variant="danger" icon="refresh" data-act="replace">${escapeHtml(T('disk.replace'))}</tf-button>` : ''}
              <tf-button variant="secondary" icon="play" data-act="smart-short">${escapeHtml(T('disks.smart_short'))}</tf-button>
              <tf-button variant="secondary" icon="clock" data-act="smart-long">${escapeHtml(T('disks.smart_long'))}</tf-button>
            </div>
          </div>
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('alert')} ${escapeHtml(T('disk.pool_errors_title', { pool: d.memberOf || '—' }))}</div></div>
            ${pool && leaf ? `
              <div class="stat-rows">
                <div class="sr"><span class="k">READ</span><span class="v">${counter(leaf.readErrors)}</span></div>
                <div class="sr"><span class="k">WRITE</span><span class="v">${counter(leaf.writeErrors)}</span></div>
                <div class="sr"><span class="k">CKSUM</span><span class="v">${counter(leaf.cksumErrors)}</span></div>
                <div class="sr"><span class="k">${escapeHtml(T('disk.pool_vdev_state'))}</span><span class="v">${stateChipHtml(leaf.state)} <span class="mono text-3">${escapeHtml(vdev.id)} · ${escapeHtml(layoutLabel(vdev.kind))}</span></span></div>
                <div class="sr"><span class="k">${escapeHtml(T('disk.pool_last_scrub'))}</span><span class="v">${escapeHtml(pool.lastScrubAt ? T('disk.pool_scrub_value', { t: fmtDate(pool.lastScrubAt), n: Number(pool.scan?.errors) || 0 }) : T('disk.pool_no_scrub'))}</span></div>
              </div>
              ${warningHtml('info', T('disk.pool_errors_info'))}
              <div class="row mt-md"><tf-button variant="ghost" size="sm" icon="layers" data-act="open-pool">${escapeHtml(T('disk.pool_open', { pool: pool.name }))}</tf-button></div>
            ` : `<div class="muted">${escapeHtml(T('disk.pool_none'))}</div>`}
          </div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('disk.smart_attributes'))}</div>
            <span class="hint">${d.smartReadAt ? escapeHtml(T('disk.smart_read', { t: fmtAgo(d.smartReadAt) })) : escapeHtml(T('disk.smart_never'))}${d.smartPassed === false ? ' · ' + escapeHtml(T('disk.smart_failed')) : ''} · ${escapeHtml(T('disk.attr_hint'))}</span></div>
          ${attrs.length ? `<tf-table id="nas-attr-table">
            <tf-column key="id" label="ID" renderer="text" width="60"></tf-column>
            <tf-column key="name" label="${escapeAttr(T('disk.attr_name'))}" renderer="text" fill></tf-column>
            <tf-column key="value" label="${escapeAttr(T('disk.attr_value'))}" renderer="num"></tf-column>
            <tf-column key="raw" label="${escapeAttr(T('disk.attr_raw'))}" renderer="text" nowrap></tf-column>
            <tf-column key="trend" label="${escapeAttr(T('disk.attr_trend'))}" renderer="html" nowrap hide-below="1000"></tf-column>
            <tf-column key="status" label="${escapeAttr(T('disks.col_health'))}" renderer="chip"></tf-column>
          </tf-table>` : `<div class="muted">${escapeHtml(d.smartAvailable ? T('disk.smart_no_attrs') : T('disk.smart_unavailable'))}</div>`}
        </div>
        <div class="grid-2">
          <div class="section-card">
            <div class="chart-head">
              <div class="ch-title">${sprite('zap')} ${escapeHtml(T('disk.temp_history', { d: historyDays }))}</div>
              <div class="ch-val" id="nas-disk-temp-val"></div>
            </div>
            <div id="nas-disk-temp-chart"></div>
          </div>
          <div class="section-card">
            <div class="chart-head">
              <div class="ch-title">${sprite('alert')} ${escapeHtml(T('disk.realloc_history', { d: historyDays }))}</div>
              <div class="ch-val" id="nas-disk-realloc-val"></div>
            </div>
            <div id="nas-disk-realloc-chart"></div>
          </div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('play')} ${escapeHtml(T('disk.self_tests'))}</div>
            <div class="actions">
              ${smart ? `<span class="sched-pill">${sprite('clock')} ${escapeHtml(T('disk.st_short', { when: fmtSchedule(smart.short) }))}</span>
              <span class="sched-pill">${sprite('clock')} ${escapeHtml(T('disk.st_long', { when: fmtSchedule(smart.long) }))}</span>` : ''}
              <tf-button variant="ghost" size="sm" icon="edit" data-act="smart-schedule" ${smart ? '' : 'disabled'}>${escapeHtml(T('disk.edit_schedule'))}</tf-button>
            </div>
          </div>
          ${tests.length ? `<tf-table id="nas-st-table">
            <tf-column key="date" label="${escapeAttr(T('disk.st_col_date'))}" renderer="text" nowrap width="170"></tf-column>
            <tf-column key="kind" label="${escapeAttr(T('disk.st_col_kind'))}" renderer="chip" width="100"></tf-column>
            <tf-column key="result" label="${escapeAttr(T('disk.st_col_result'))}" renderer="html" fill></tf-column>
            <tf-column key="hours" label="${escapeAttr(T('disk.st_col_hours'))}" renderer="text" nowrap width="120"></tf-column>
          </tf-table>` : `<div class="muted">${escapeHtml(T('disk.no_self_tests'))}</div>`}
        </div>
      </div>`;

    // The shell header already says "TentaNas › node"; this tail adds
    // "Dyski › sdd", the same shape pool-detail.js uses for "Pule › tank".
    wireCrumbs(body, { disks: () => { this.diskId = null; this.clearTimers(); this.setLocation(); this.drawTab(); } });
    body.querySelector('[data-act="locate"]').addEventListener('click', () => this.locateDisk(d, !this.locateState?.[d.diskId]));
    body.querySelector('[data-act="copy-serial"]').addEventListener('click', async () => {
      await navigator.clipboard?.writeText(d.serial || '');
      toast(T('disk.serial_copied'), 'success');
    });
    body.querySelector('[data-act="smart-short"]').addEventListener('click', () => this.startSmartTest(d, 'short'));
    body.querySelector('[data-act="smart-long"]').addEventListener('click', () => this.startSmartTest(d, 'long'));
    body.querySelector('[data-act="replace"]')?.addEventListener('click', () => this.openReplaceForDisk(d));
    body.querySelector('[data-act="open-pool"]')?.addEventListener('click', () => this.openPool(pool.name));
    body.querySelector('[data-act="smart-schedule"]')?.addEventListener('click', () => openSmartScheduleEditor(this, smart, () => this.drawTab()));
    this.locateState = this.locateState || {};

    const attrTable = body.querySelector('#nas-attr-table');
    if (attrTable) {
      attrTable.rows = attrs.map((a) => ({
        id: String(a.id),
        name: a.name,
        value: a.value,
        raw: a.rawText || String(a.raw),
        trend: a.rawWeekAgo == null ? '<span class="text-3">—</span>' : trendHtml(a.raw, a.rawWeekAgo),
        status: { status: a.status === 'ok' ? 'ok' : a.status === 'critical' ? 'err' : a.status === 'warning' ? 'warn' : 'info', label: T('health.' + (['ok', 'warning', 'critical'].includes(a.status) ? a.status : 'unknown')), dot: true },
      }));
    }
    const stTable = body.querySelector('#nas-st-table');
    if (stTable) {
      stTable.rows = tests.map((t) => ({
        date: t.startedAt ? fmtDate(t.startedAt) : '—',
        kind: { status: t.kind.toLowerCase().includes('extended') || t.kind.toLowerCase().includes('long') ? 'accent' : 'neutral', label: t.kind },
        result: `<tf-chip size="sm" status="${t.status === 'passed' ? 'ok' : t.status === 'running' ? 'info' : t.status === 'failed' ? 'err' : 'warn'}" dot label="${escapeAttr(T('disk.st_status_' + (['passed', 'failed', 'running'].includes(t.status) ? t.status : 'unknown')))}"></tf-chip> <span class="text-3">${escapeHtml(t.detail || '')}</span>`,
        hours: `${t.lifetimeHours} h`,
      }));
    }
    this.drawDiskHistory(body, history);
  },

  // The replace wizard needs the pool topology and the free disks of the node;
  // both come fresh so a disk that was claimed meanwhile cannot be offered.
  async openReplaceForDisk(disk) {
    let poolsRes, disksRes;
    try {
      [poolsRes, disksRes] = await Promise.all([
        this.nas('tentaNasPoolsListRequest', {}),
        this.nas('tentaNasDisksListRequest', {}),
      ]);
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    const pool = (poolsRes.pools || []).find((p) => p.name === disk.memberOf);
    const vdev = pool ? (pool.vdevs || []).find((v) => (v.disks || []).some((x) => x.diskId === disk.diskId || x.name === disk.name)) : null;
    const leaf = vdev ? (vdev.disks || []).find((x) => x.diskId === disk.diskId || x.name === disk.name) : null;
    if (!pool || !vdev || !leaf) {
      toast(T('disk.replace_not_in_pool', { device: disk.name }), 'error');
      return;
    }
    openReplaceWizard(this, { pool, vdev, disk: leaf, freeDisks: poolsRes.freeDisks || [], disks: disksRes.disks || [], onDone: () => this.drawTab() });
  },

  // The disk's sample history (n04): temperature and the reallocated-sector
  // counter. Each card is a tf-line-chart on a time axis; fewer than two
  // samples shows the empty note instead of an empty plot.
  drawDiskHistory(body, history) {
    const samples = (history || [])
      .map((h) => ({ ...h, t: parseServerTs(h.at)?.getTime() }))
      .filter((h) => h.t != null)
      .sort((a, b) => a.t - b.t);
    // A multi-day window is unreadable with clock ticks; the axis follows the
    // span the backend actually returned.
    const spanDays = samples.length ? (samples[samples.length - 1].t - samples[0].t) / 86400000 : 0;
    const tickOpts = spanDays > 1 ? { day: '2-digit', month: '2-digit' } : { hour: '2-digit', minute: '2-digit' };
    const timeAxis = { scale: 'time', ticks: 6, format: (v) => new Intl.DateTimeFormat(I18n.getLanguage(), tickOpts).format(new Date(v)) };
    const mount = (hostId, valId, series, yAxis, valueFormat, summary) => {
      const host = body.querySelector('#' + hostId);
      const val = body.querySelector('#' + valId);
      const points = series.flatMap((s) => s.points);
      if (points.length < 2) {
        host.innerHTML = `<div class="muted">${escapeHtml(T('disk.history_empty'))}</div>`;
        val.textContent = '';
        return;
      }
      const chart = document.createElement('tf-line-chart');
      chart.height = 150;
      chart.legend = series.length > 1 ? { position: 'bottom', alignment: 'start' } : { position: 'none' };
      chart.xAxis = timeAxis;
      chart.yAxis = yAxis;
      chart.tooltip = { valueFormat };
      chart.narrow = null;
      chart.series = series;
      host.replaceChildren(chart);
      val.textContent = summary;
    };
    const temps = samples.filter((h) => h.temperatureC != null);
    mount('nas-disk-temp-chart', 'nas-disk-temp-val',
      [{ id: 'temp', name: T('disk.legend_temp'), tone: 'warning', style: 'solid', showInLegend: false, points: temps.map((h) => ({ x: h.t, y: Number(h.temperatureC) })) }],
      { min: 0, ticks: 4, format: (v) => `${v}°` },
      (v) => `${Math.round(v)}°C`,
      temps.length ? T('disk.minmax', { min: Math.min(...temps.map((h) => h.temperatureC)), max: Math.max(...temps.map((h) => h.temperatureC)) }) : '');
    const realloc = samples.filter((h) => h.reallocatedSectors != null);
    mount('nas-disk-realloc-chart', 'nas-disk-realloc-val',
      [{ id: 'realloc', name: T('disk.reallocated'), tone: 'critical', style: 'solid', showInLegend: false, points: realloc.map((h) => ({ x: h.t, y: Number(h.reallocatedSectors) })) }],
      { min: 0, ticks: 4 },
      (v) => String(Math.round(v)),
      realloc.length ? `${realloc[0].reallocatedSectors} → ${realloc[realloc.length - 1].reallocatedSectors}` : '');
  },

  // ---------------------------------------------------------------------------
  // Job rows shared by the overview card and the tasks tab (n02/n15)
  // ---------------------------------------------------------------------------

  jobRowHtml(j) {
    const running = j.status === 'running';
    const last = (j.log || []).slice(-1)[0] || '';
    return `
      <div class="job-row" data-job="${escapeAttr(j.jobId)}">
        <div class="job-ico ${running ? 'running' : ''}">${sprite(running ? 'refresh' : 'clock')}</div>
        <div class="job-main">
          <div class="job-name">${escapeHtml(jobKindLabel(j.kind))} <span class="mono text-2">${escapeHtml(j.subject)}</span> <tf-chip status="${jobTone(j.status)}" label="${escapeAttr(T('jobs.status_' + j.status))}"></tf-chip></div>
          <div class="job-sub">${escapeHtml(T('jobs.started_by', { by: j.startedBy, t: fmtAgo(j.startedAt) }))}${last ? ' · ' + escapeHtml(last) : ''}</div>
          ${j.progressPct != null ? `<tf-progress-bar value="${Number(j.progressPct)}" size="sm" tone="accent"></tf-progress-bar>` : ''}
        </div>
        <div class="job-actions">
          <tf-button size="sm" variant="ghost" icon="file-text" data-act="log" title="${escapeAttr(T('jobs.log'))}"></tf-button>
          <tf-button size="sm" variant="ghost" tone="critical" icon="stop" data-act="cancel" title="${escapeAttr(T('jobs.cancel'))}"></tf-button>
        </div>
      </div>`;
  },

  wireJobRows(el, onChange) {
    el.querySelectorAll('.job-row').forEach((row) => {
      const id = row.dataset.job;
      row.querySelector('[data-act="log"]').addEventListener('click', () => this.openJobLog(id));
      row.querySelector('[data-act="cancel"]').addEventListener('click', async () => {
        const ok = await TfWindow.confirm({ title: T('jobs.cancel'), message: T('jobs.cancel_confirm'), confirmLabel: T('jobs.cancel'), cancelLabel: I18n.t('common.cancel'), danger: true });
        if (!ok) return;
        try {
          await this.nas('tentaNasJobCancelRequest', { jobId: id });
          onChange();
        } catch (e) {
          toast(errMessage(e), 'error');
        }
      });
    });
  },

  // ---------------------------------------------------------------------------
  // Environment tab (n16)
  // ---------------------------------------------------------------------------

  async drawEnvironment(body) {
    if (!this.environment) {
      body.innerHTML = `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`;
      await this.refreshHeader(false);
      if (this.disposed || !body.isConnected) return;
      if (!this.environment) return;
    }
    const env = this.environment;
    const el = env.elevation;
    const admin = this.isAdmin;
    const armed = el.armedUntil && parseServerTs(el.armedUntil) && parseServerTs(el.armedUntil).getTime() > Date.now();

    const channelChip = el.mode === 'helper'
      ? `<tf-chip status="${el.helperState === 'ok' ? 'ok' : 'warn'}" dot label="${escapeAttr(T('elevation.chip_helper'))}"></tf-chip>`
      : el.mode === 'interactive'
        ? `<tf-chip status="${armed ? 'ok' : 'info'}" dot label="${escapeAttr(armed ? T('elevation.chip_armed', { t: fmtDate(el.armedUntil) }) : T('elevation.chip_interactive'))}"></tf-chip>`
        : `<tf-chip status="warn" dot label="${escapeAttr(T('elevation.chip_unarmed'))}"></tf-chip>`;

    // A helper that is not "ok" says WHY there instead of claiming a version
    // match — the state is the actionable fact.
    const helperCompat = el.helperState === 'ok'
      ? T(el.coreCompatible ? 'elevation.compat_ok' : 'elevation.compat_bad')
      : T('elevation.helper_' + el.helperState);
    const helperValue = el.mode === 'helper'
      ? `${escapeHtml(T('elevation.helper_value', { v: el.helperVersion || '—', compat: helperCompat }))} <span class="mono text-3">${escapeHtml(el.helperPath)}</span>`
      : escapeHtml(T('elevation.helper_absent'));
    const channelRows = [
      [T('elevation.row_helper'), helperValue],
      [T('elevation.row_sudoers'), el.mode === 'helper' && el.helperState !== 'sudoers_missing' ? `${escapeHtml(T('elevation.sudoers_value'))} · <span class="mono">${escapeHtml(el.sudoersPath)}</span>` : '—'],
      [T('elevation.row_provisioning'), el.provisionedAt ? escapeHtml(T('elevation.provisioning_value', { date: fmtDate(el.provisionedAt), user: el.provisionedBy || '—' })) : escapeHtml(T('elevation.provisioning_none'))],
      [T('elevation.row_audit'), `${escapeHtml(T('elevation.audit_value', { n: Number(el.auditEntries) || 0 }))} · <a data-act="audit-log">${escapeHtml(T('elevation.audit_link'))}</a>`],
      ...(el.mode === 'helper' ? [] : [
        [T('elevation.row_user'), `<span class="mono">${escapeHtml(el.coreUser)}</span>`],
        [T('elevation.row_ttl'), escapeHtml(fmtDuration(el.ttlSecs))],
      ]),
    ];

    const actions = [];
    if (admin) {
      if (el.mode === 'unarmed') {
        actions.push(`<tf-button variant="primary" icon="unlock" data-act="wizard">${escapeHtml(T('elevation.configure'))}</tf-button>`);
      } else if (el.mode === 'helper') {
        actions.push(`<tf-button variant="secondary" size="sm" icon="refresh" data-act="wizard-helper">${escapeHtml(T('elevation.reprovision'))}</tf-button>`);
        actions.push(`<tf-button variant="ghost" size="sm" icon="list" data-act="catalog">${escapeHtml(T('elevation.catalog'))}</tf-button>`);
        actions.push(`<tf-button variant="danger" size="sm" icon="lock" data-act="remove">${escapeHtml(T('elevation.remove'))}</tf-button>`);
      } else {
        if (armed) actions.push(`<tf-button variant="ghost" icon="lock" data-act="disarm">${escapeHtml(T('elevation.disarm'))}</tf-button>`);
        else actions.push(`<tf-button variant="primary" icon="unlock" data-act="arm">${escapeHtml(T('elevation.arm'))}</tf-button>`);
        actions.push(`<tf-button variant="ghost" icon="shield" data-act="wizard-helper">${escapeHtml(T('elevation.switch_helper'))}</tf-button>`);
      }
    }

    const features = env.features || [];
    const others = this.nodes.filter((n) => n.nodeId !== this.nodeId);

    body.innerHTML = `
      <div class="stack">
        ${env.fullSupport ? '' : `<tf-alert tone="warning" title="${escapeAttr(T('env.partial_support'))}" message="${escapeAttr(T('env.partial_support_msg', { os: env.osName }))}"></tf-alert>`}
        <div class="grid-2">
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('key')} ${escapeHtml(T('elevation.title'))}</div><span class="hint">${escapeHtml(T('elevation.hint'))}</span></div>
            ${channelChip}
            <div class="stat-rows mt-sm">${channelRows.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${v}</span></div>`).join('')}</div>
            ${el.mode === 'helper' ? `<details class="sudoers"><summary>${escapeHtml(T('elevation.show_sudoers'))}</summary><pre class="cmd mono" id="nas-sudoers">${escapeHtml(T('elevation.plan_loading'))}</pre></details>` : ''}
            ${actions.length ? `<div class="row mt-md">${actions.join('')}</div>` : admin ? '' : `<div class="muted mt-md">${escapeHtml(T('elevation.admin_only'))}</div>`}
          </div>
          <div class="stack">
            <div class="explain-box">
              <h4>${sprite('shield')} ${escapeHtml(T('elevation.explain_helper_title'))}</h4>
              <p>${escapeHtml(T('elevation.explain_helper_p'))}</p>
              <ul class="loss-list">
                <li class="ll good">${sprite('check')}<span>${escapeHtml(T('elevation.explain_helper_1'))}</span></li>
                <li class="ll good">${sprite('check')}<span>${escapeHtml(T('elevation.explain_helper_2'))}</span></li>
                <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('elevation.explain_helper_3'))}</span></li>
              </ul>
            </div>
            <div class="explain-box">
              <h4>${sprite('key')} ${escapeHtml(T('elevation.explain_interactive_title'))}</h4>
              <p>${escapeHtml(T('elevation.explain_interactive_p'))}</p>
              <ul class="loss-list">
                <li class="ll good">${sprite('check')}<span>${escapeHtml(T('elevation.explain_interactive_1'))}</span></li>
                <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('elevation.explain_interactive_2'))}</span></li>
                <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('elevation.explain_interactive_3'))}</span></li>
              </ul>
            </div>
          </div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('os')} ${escapeHtml(T('env.system'))}</div><span class="hint">${escapeHtml(T('probed', { t: fmtAgo(env.probedAt) }))}</span></div>
          <div class="grid-2">
            <div class="stat-rows">
              <div class="sr"><span class="k">${escapeHtml(T('env.os'))}</span><span class="v">${escapeHtml(env.osName)} ${escapeHtml(env.osVersion || '')}</span></div>
              <div class="sr"><span class="k">${escapeHtml(T('env.kernel'))}</span><span class="v mono">${escapeHtml(env.kernel)}</span></div>
              <div class="sr"><span class="k">${escapeHtml(T('env.hostname'))}</span><span class="v mono">${escapeHtml(env.hostname)}</span></div>
            </div>
            <div class="stat-rows">
              <div class="sr"><span class="k">${escapeHtml(T('env.package_manager'))}</span><span class="v">${env.packageManager ? `<span class="mono">${escapeHtml(env.packageManager)}</span>` : escapeHtml(T('env.package_manager_none'))}</span></div>
              <div class="sr"><span class="k">${escapeHtml(T('env.ram'))}</span><span class="v">${escapeHtml(fmtBytes(env.ramBytes))}</span></div>
              <div class="sr"><span class="k">${escapeHtml(T('env.uptime'))}</span><span class="v">${escapeHtml(fmtDuration(env.uptimeSecs))}</span></div>
            </div>
          </div>
        </div>
        <div class="section-card">
          <div class="section-card-head">
            <div class="title">${sprite('database')} ${escapeHtml(T('arc.settings_title'))}</div>
            <span class="hint">${escapeHtml(T('arc.settings_hint', { node: this.currentNode().nodeName, ram: fmtBytes(env.ramBytes) }))}</span>
          </div>
          <div id="nas-env-arc"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('env.features'))}</div>
            <span class="hint">${escapeHtml(T('env.features_hint'))}</span>
            <div class="actions"><tf-button variant="secondary" size="sm" icon="refresh" data-act="reprobe">${escapeHtml(T('reprobe'))}</tf-button></div></div>
          <tf-table id="nas-feature-table">
            <tf-column key="name" label="${escapeAttr(T('env.col_feature'))}" renderer="html" fill></tf-column>
            <tf-column key="status" label="${escapeAttr(T('env.col_status'))}" renderer="chip"></tf-column>
            <tf-column key="version" label="${escapeAttr(T('env.col_version_detail'))}" renderer="html"></tf-column>
          </tf-table>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('network')} ${escapeHtml(T('env.other_nodes'))}</div><span class="hint">${escapeHtml(T('env.other_nodes_hint'))}</span></div>
          <tf-table id="nas-others-table" empty-message="${escapeAttr(T('env.no_other_nodes'))}">
            <tf-column key="name" label="${escapeAttr(T('env.col_node'))}" renderer="html" fill></tf-column>
            <tf-column key="platform" label="${escapeAttr(T('env.col_platform'))}" renderer="text"></tf-column>
            <tf-column key="channel" label="${escapeAttr(T('env.col_channel'))}" renderer="chip"></tf-column>
            <tf-column key="features" label="${escapeAttr(T('env.col_features'))}" renderer="text"></tf-column>
          </tf-table>
        </div>
        <div class="section-card">
          <div class="section-card-head">
            <div class="title">${sprite('save')} ${escapeHtml(T('config.title'))}</div>
            <span class="hint">${escapeHtml(T('config.hint'))}</span>
            <div class="actions">
              <tf-button size="sm" variant="secondary" icon="download" data-act="export-config">${escapeHtml(T('config.export'))}</tf-button>
              <tf-button size="sm" variant="secondary" icon="file" data-act="import-config" ${this.isAdmin ? '' : 'disabled'} title="${this.isAdmin ? '' : escapeAttr(T('elevation.admin_only'))}">${escapeHtml(T('config.import'))}</tf-button>
            </div>
          </div>
          <div class="explain-box">${escapeHtml(T('config.explain'))}</div>
        </div>
      </div>`;

    body.querySelector('[data-act="export-config"]').addEventListener('click', () => exportConfig(this));
    body.querySelector('[data-act="import-config"]').addEventListener('click', () => openConfigImportDialog(this, { onDone: () => { this.loadNodes(); if (!this.disposed) this.drawTab(); } }));
    body.querySelector('[data-act="wizard"]')?.addEventListener('click', () => this.openChannelWizard());
    body.querySelector('[data-act="wizard-helper"]')?.addEventListener('click', () => this.openChannelWizard('helper'));
    body.querySelector('[data-act="arm"]')?.addEventListener('click', () => this.openChannelWizard('interactive'));
    body.querySelector('[data-act="catalog"]')?.addEventListener('click', () => this.openHelperCatalog());
    body.querySelector('[data-act="audit-log"]')?.addEventListener('click', () => this.switchTab('jobs'));
    body.querySelector('[data-act="reprobe"]')?.addEventListener('click', () => this.reprobe());
    body.querySelector('[data-act="disarm"]')?.addEventListener('click', async () => {
      try {
        await this.nas('tentaNasElevationDisarmRequest', {});
        toast(T('elevation.disarmed'), 'success');
        await this.reprobe();
      } catch (e) {
        toast(errMessage(e), 'error');
      }
    });
    body.querySelector('[data-act="remove"]')?.addEventListener('click', () => this.removeHelper());

    const sudoersPre = body.querySelector('#nas-sudoers');
    if (sudoersPre) {
      this.nas('tentaNasElevationPlanRequest', {}).then((r) => {
        if (sudoersPre.isConnected) sudoersPre.textContent = `${r.plan.sudoersPath}\n${r.plan.sudoersLine}`;
      }).catch((e) => { if (sudoersPre.isConnected) sudoersPre.textContent = errMessage(e); });
    }

    const ftable = body.querySelector('#nas-feature-table');
    ftable.rows = features.map((f) => ({
      _feature: f,
      name: `<div class="cell-2"><div class="l1">${escapeHtml(T('feature.' + f.id))}${f.optional ? ` <span class="text-3">(${escapeHtml(T('env.optional'))})</span>` : ''}</div><div class="l2 mono">${escapeHtml([...(f.binaries || []), f.kernelModule ? `mod:${f.kernelModule}` : ''].filter(Boolean).join(' '))}</div></div>`,
      status: { status: f.status === 'ok' ? 'ok' : f.status === 'outdated' || f.status === 'version_too_low' ? 'warn' : f.optional ? 'info' : 'err', label: T('feature_status.' + f.status), dot: true },
      version: `<span class="mono">${escapeHtml([
        f.version ? `${f.version}${f.requiredVersion ? ` (≥ ${f.requiredVersion})` : ''}` : f.requiredVersion ? `≥ ${f.requiredVersion}` : '',
        f.detail || '',
      ].filter(Boolean).join(' · ') || '—')}</span>`,
    }));
    ftable.rowActions = (row) => {
      const f = row._feature;
      const installable = admin && f.status !== 'ok' && f.status !== 'unsupported_platform' && (f.packages || []).length > 0;
      if (!installable) return null;
      const b = document.createElement('tf-button');
      b.setAttribute('size', 'sm');
      b.setAttribute('variant', 'ghost');
      b.setAttribute('icon', 'download');
      b.textContent = T('env.install_sudo');
      if (!env.packageManager) {
        b.setAttribute('disabled', '');
        b.title = T('env.package_manager_none');
      }
      b.addEventListener('click', (e) => { e.stopPropagation(); this.installFeature(f); });
      return b;
    };

    const otable = body.querySelector('#nas-others-table');
    otable.rows = others.map((n) => ({
      _node: n,
      name: `<div class="cell-2"><div class="l1">${escapeHtml(n.nodeName)}${n.isLocal ? ` <span class="text-3">(${escapeHtml(T('this_node'))})</span>` : ''}${n.online ? '' : ` <tf-chip status="info" label="${escapeAttr(T('offline'))}"></tf-chip>`}</div><div class="l2 mono">${escapeHtml(n.nodeId.slice(0, 16))}…</div></div>`,
      platform: n.instanceStatus === 'ready' ? (n.osName || '—') : T('instance.' + n.instanceStatus),
      channel: { status: n.elevationMode === 'unarmed' ? 'warn' : 'ok', label: T('elevation.mode_' + (n.elevationMode || 'unarmed')), dot: true },
      features: (n.features || []).join(' · ') || (n.instanceStatus === 'ready' ? T('env.features_unknown') : T('instance.' + n.instanceStatus)),
    }));
    otable.rowActions = (row) => {
      const n = row._node;
      if (n.instanceStatus !== 'ready') return null;
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      const unarmed = (n.elevationMode || 'unarmed') === 'unarmed';
      wrap.innerHTML = unarmed && admin
        ? `<tf-button size="sm" variant="secondary" icon="unlock" data-act="arm-node">${escapeHtml(T('env.arm_node'))}</tf-button>`
        : `<tf-button size="sm" variant="ghost" icon="chevron-right" data-act="go">${escapeHtml(T('env.go_to_node'))}</tf-button>`;
      wrap.querySelector('[data-act="go"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.selectNode(n.nodeId); });
      wrap.querySelector('[data-act="arm-node"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.armNode(n); });
      return wrap;
    };
    otable.addEventListener('row-click', (e) => { if (e.detail.row._node.instanceStatus === 'ready') this.selectNode(e.detail.row._node.nodeId); });

    this.paintArcSettings(body, env);
  },

  // n17b for a node other than the selected one: the password goes straight to
  // that node, the view stays where it is.
  async armNode(node) {
    const creds = await this.promptSudo(T('env.arm_node_title', { node: node.nodeName }), node);
    if (!creds) return;
    try {
      await this.nasOn(node, 'tentaNasElevationArmRequest', { sudoPassword: creds.password, ttlSecs: 0 }, { timeoutMs: ADMIN_TIMEOUT_MS });
      toast(T('env.armed_node', { node: node.nodeName }), 'success');
      await this.loadNodes();
      if (!this.disposed) this.drawTab();
    } catch (e) {
      toast(errMessage(e), 'error');
    }
  },

  // n16 ARC slider: the cap is a share of the node's RAM, written through the
  // permission channel so it survives a reboot.
  async paintArcSettings(body, env) {
    const host = body.querySelector('#nas-env-arc');
    if (!host) return;
    const res = await this.nas('tentaNasArcStatsRequest', {}).catch(() => ({ arc: null }));
    if (this.disposed || !host.isConnected) return;
    const arc = res.arc;
    if (!arc || !arc.ramBytes) {
      host.innerHTML = `<div class="muted">${escapeHtml(T('arc.unavailable'))}</div>`;
      return;
    }
    const ram = Number(arc.ramBytes);
    const current = Math.max(10, Math.min(90, Math.round((Number(arc.maxBytes) || 0) / ram * 100)));
    host.innerHTML = `
      <div class="slider-row">
        <div>
          <div class="sl-name">${escapeHtml(T('arc.slider_name'))}</div>
          <div class="sl-desc">${escapeHtml(T('arc.slider_desc'))}</div>
        </div>
        <tf-slider id="nas-arc-slider" min="10" max="90" step="1" value="${current}" ${this.isAdmin ? '' : 'disabled'}></tf-slider>
        <div class="sl-val" id="nas-arc-val">${escapeHtml(T('arc.slider_value', { pct: current, size: fmtBytes(ram * current / 100) }))}</div>
      </div>
      <div class="grid-2 mt-md">
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('arc.live_usage'))}</span><span class="v">${escapeHtml(fmtBytes(arc.sizeBytes))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('arc.hit_ratio_24h'))}</span><span class="v num-ok">${(Number(arc.hitRatio) || 0).toFixed(1)}% · <a data-act="arc-details">${escapeHtml(T('arc.details'))}</a></span></div>
          <div class="sr"><span class="k">${escapeHtml(T('arc.row_limit_source'))}</span><span class="v">${escapeHtml(T('arc.limit_source_' + (arc.limitSource || 'default')))}</span></div>
        </div>
        ${warningHtml('warning', T('arc.warning'))}
      </div>
      <div class="row mt-md" style="justify-content:flex-end">
        <tf-button variant="primary" icon="check" data-act="arc-apply" disabled>${escapeHtml(T('arc.apply'))}</tf-button>
      </div>`;
    const slider = host.querySelector('#nas-arc-slider');
    const valEl = host.querySelector('#nas-arc-val');
    const apply = host.querySelector('[data-act="arc-apply"]');
    slider.addEventListener('input', (e) => {
      const p = Number(e.detail.value) || current;
      valEl.textContent = T('arc.slider_value', { pct: p, size: fmtBytes(ram * p / 100) });
      if (this.isAdmin && p !== current) apply.removeAttribute('disabled'); else apply.setAttribute('disabled', '');
    });
    host.querySelector('[data-act="arc-details"]').addEventListener('click', () => this.switchTab('overview'));
    apply.addEventListener('click', async () => {
      const p = Number(slider.value) || current;
      const ok = await this.withSudo((sudoPassword) => this.nas('tentaNasArcLimitSetRequest', { maxBytes: Math.round(ram * p / 100), sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('arc.settings_title'));
      if (!ok) return;
      toast(T('arc.applied'), 'success');
      this.paintArcSettings(body, env);
    });
  },

  // MAJ-27: what the helper is actually allowed to run, straight from the
  // catalog the core and the helper share.
  async openHelperCatalog() {
    const win = document.createElement('tf-window');
    win.className = 'nas-modal';
    win.setAttribute('title', T('elevation.catalog_title'));
    win.setAttribute('icon', 'list');
    win.setAttribute('buttons', 'close');
    win.setAttribute('draggable', '');
    win.setAttribute('width', '760');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    win.innerHTML = `
      <div slot="body" class="stack"><div id="nas-cat-body" class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
      <div slot="footer"><tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button></div>`;
    win.addEventListener('action', (e) => { if (e.detail.action === 'cancel') win.close(); });
    document.body.appendChild(win);
    let res;
    try {
      res = await this.nas('tentaNasElevationCatalogRequest', {});
    } catch (e) {
      const host = win.querySelector('#nas-cat-body');
      if (host) host.textContent = errMessage(e);
      return;
    }
    const host = win.querySelector('#nas-cat-body');
    if (!host) return;
    const commands = res.commands || [];
    if (!commands.length) {
      host.textContent = T('elevation.catalog_empty');
      return;
    }
    host.classList.remove('muted');
    host.innerHTML = `<tf-table id="nas-cat-table">
      <tf-column key="name" label="${escapeAttr(T('elevation.catalog_col_name'))}" renderer="html" nowrap></tf-column>
      <tf-column key="description" label="${escapeAttr(T('elevation.catalog_col_desc'))}" renderer="text" fill></tf-column>
      <tf-column key="tool" label="${escapeAttr(T('elevation.catalog_col_tool'))}" renderer="html" nowrap></tf-column>
    </tf-table>`;
    host.querySelector('#nas-cat-table').rows = commands.map((c) => ({
      name: `<span class="mono fw-700">${escapeHtml(c.name)}</span>`,
      description: c.description,
      tool: `<span class="mono">${escapeHtml(c.tool)}</span>${c.builtin ? ` <tf-chip size="sm" status="info" label="${escapeAttr(T('elevation.catalog_builtin'))}"></tf-chip>` : ''}${c.needsStdin ? ` <tf-chip size="sm" status="accent" label="${escapeAttr(T('elevation.catalog_stdin'))}"></tf-chip>` : ''}`,
    }));
  },

  async installFeature(feature) {
    const ok = await TfWindow.confirm({
      title: T('env.install_title', { name: T('feature.' + feature.id) }),
      message: T('env.install_confirm', { packages: (feature.packages || []).join(', '), pm: this.environment.packageManager }),
      confirmLabel: T('env.install'),
      cancelLabel: I18n.t('common.cancel'),
    });
    if (!ok) return;
    const job = await this.withSudo((sudoPassword) => this.nas('tentaNasPackagesInstallRequest', { featureId: feature.id, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), T('env.install_title', { name: T('feature.' + feature.id) }));
    if (!job) return;
    this.openJobLog(job.job.jobId, () => this.reprobe());
  },

  async removeHelper() {
    const ok = await TfWindow.confirm({
      title: T('elevation.remove'),
      message: T('elevation.remove_confirm'),
      confirmLabel: T('elevation.remove'),
      cancelLabel: I18n.t('common.cancel'),
      danger: true,
    });
    if (!ok) return;
    // Removing the sudoers rule is itself privileged and the helper is what
    // is being removed, so this call always carries a fresh password.
    const creds = await this.promptSudo(T('elevation.remove'));
    if (!creds) return;
    try {
      const res = await this.nas('tentaNasElevationRemoveRequest', { sudoPassword: creds.password }, { timeoutMs: ADMIN_TIMEOUT_MS });
      this.openJobLog(res.job.jobId, () => this.reprobe());
    } catch (e) {
      toast(errMessage(e), 'error');
    }
  },

  // ---------------------------------------------------------------------------
  // Privilege plumbing: sudo prompt (n17b) and the channel wizard (n16/n17)
  // ---------------------------------------------------------------------------

  // Runs `fn(sudoPassword)` with whatever the node's channel needs: nothing
  // when the helper is provisioned or an interactive arm is still live,
  // otherwise a password from the prompt. "Remember" arms the channel first
  // (the core keeps the secret in RAM for the node's TTL) and the action then
  // runs without a password. Returns the action's response or null.
  async withSudo(fn, title) {
    if (!this.environment) await this.refreshHeader(false);
    const el = this.environment?.elevation;
    const armed = el && el.armedUntil && parseServerTs(el.armedUntil) && parseServerTs(el.armedUntil).getTime() > Date.now();
    const needsPassword = !el || el.mode === 'unarmed' || (el.mode === 'interactive' && !armed) || (el.mode === 'helper' && el.helperState !== 'ok');
    try {
      if (!needsPassword) return await fn(undefined);
      const creds = await this.promptSudo(title);
      if (!creds) return null;
      if (creds.remember) {
        await this.nas('tentaNasElevationArmRequest', { sudoPassword: creds.password, ttlSecs: 0 }, { timeoutMs: ADMIN_TIMEOUT_MS });
        this.refreshHeader(false);
        return await fn(undefined);
      }
      return await fn(creds.password);
    } catch (e) {
      toast(errMessage(e), 'error');
      return null;
    }
  },

  promptSudo(title, node = this.currentNode()) {
    const user = this.environment?.elevation?.coreUser || 'tentaflow';
    return new Promise((resolve) => {
      const win = document.createElement('tf-window');
      win.className = 'nas-modal';
      win.setAttribute('title', title || T('sudo.title', { node: node.nodeName }));
      win.setAttribute('icon', 'key');
      win.setAttribute('buttons', 'close');
      win.setAttribute('width', '520');
      win.setAttribute('initial-x', 'center');
      win.setAttribute('initial-y', 'center');
      win.innerHTML = `
        <div slot="body" class="stack">
          <div class="explain-box">${escapeHtml(node.isLocal ? T('sudo.explain_local') : T('sudo.explain_remote', { node: node.nodeName }))}</div>
          <tf-input id="nas-sudo-pass" type="password" autocomplete="current-password" autofocus label="${escapeAttr(T('sudo.password_label', { user, node: node.nodeName }))}"></tf-input>
          <div class="toggle-card">
            <div class="tc-text"><span>${escapeHtml(T('sudo.remember'))}</span><span class="tc-sub">${escapeHtml(T('sudo.remember_sub', { ttl: fmtDuration(this.environment?.elevation?.ttlSecs || 900) }))}</span></div>
            <tf-toggle id="nas-sudo-remember"></tf-toggle>
          </div>
        </div>
        <div slot="footer">
          <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
          <tf-button variant="primary" icon="unlock" data-action="confirm">${escapeHtml(T('sudo.confirm'))}</tf-button>
        </div>`;
      let settled = false;
      const finish = (value) => { if (!settled) { settled = true; resolve(value); } };
      win.addEventListener('action', (e) => {
        if (e.detail.action === 'confirm') {
          const password = win.querySelector('#nas-sudo-pass').value;
          if (!password) { win.querySelector('#nas-sudo-pass').setAttribute('error', T('sudo.password_required')); return; }
          finish({ password, remember: win.querySelector('#nas-sudo-remember').checked });
          win.close(true);
        } else if (e.detail.action === 'cancel') {
          finish(null);
          win.close(true);
        }
      });
      win.addEventListener('close-request', () => finish(null));
      win.querySelector('#nas-sudo-pass').addEventListener('keydown', (e) => { if (e.key === 'Enter') win.querySelector('[data-action="confirm"]').click(); });
      document.body.appendChild(win);
    });
  },

  // The three-step wizard is the addon install wizard 1:1 (same window
  // size, header, progress rail and footer) with TentaNas content: mode
  // choice → password (+ plan for the helper, TTL for interactive) → run.
  async openChannelWizard(presetMode = null) {
    if (this.openWindow) { this.openWindow.remove(); this.openWindow = null; }
    const node = this.currentNode();
    const env = this.environment;
    // The first run (no preset mode) ends with §5.8 "Odtwórz z kopii": once
    // the channel works, the admin may restore a desired-state export right
    // away. Re-arming or re-provisioning from the environment tab skips it.
    const restore = !presetMode;
    const state = { step: 0, mode: presetMode || 'helper', password: '', ttl: 0, plan: null, job: null, result: null, timer: null, restore: { json: null, plan: null } };
    const steps = [T('wizard.step_mode'), T('wizard.step_password'), T('wizard.step_run'), ...(restore ? [T('wizard.step_restore')] : [])];

    const win = document.createElement('tf-window');
    win.className = 'nas-modal';
    win.setAttribute('title', T('wizard.title', { node: node.nodeName }));
    win.setAttribute('icon', 'key');
    win.setAttribute('buttons', 'close');
    win.setAttribute('draggable', '');
    win.setAttribute('width', '820');
    win.setAttribute('min-width', '640');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    this.openWindow = win;

    const header = () => `
      <div class="install-header">
        <div class="big-ico">${sprite('key')}</div>
        <div class="install-header-meta">
          <h1>${escapeHtml(T('wizard.heading'))} <span class="version">${escapeHtml(T('wizard.node_tag', { node: node.nodeName }))}</span></h1>
          <div class="sub">${escapeHtml(T('wizard.sub', { user: env?.elevation?.coreUser || 'tentaflow', os: env?.osName || '' }))}</div>
        </div>
      </div>
      <div class="install-progress">${steps.map((s, i) => `<div class="install-step ${i === state.step ? 'active' : i < state.step ? 'done' : ''}"><span class="num">${i < state.step ? sprite('check') : i + 1}</span><span class="label">${escapeHtml(s)}</span></div>`).join('')}</div>`;

    const stepMode = () => `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard.mode_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard.mode_sub'))}</p>
      <tf-choice-group id="nas-wz-mode" value="${escapeAttr(state.mode)}" columns="2">
        <tf-choice-card value="helper" icon="shield" heading="${escapeAttr(T('wizard.helper_heading'))}" description="${escapeAttr(T('wizard.helper_desc'))}" pill="${escapeAttr(T('wizard.recommended'))}" pill-tone="ok"></tf-choice-card>
        <tf-choice-card value="interactive" icon="key" heading="${escapeAttr(T('wizard.interactive_heading'))}" description="${escapeAttr(T('wizard.interactive_desc'))}"></tf-choice-card>
      </tf-choice-group>
      <div class="wizard-warning info mt-md">${escapeHtml(T('wizard.mode_note'))}</div>`;

    const stepPassword = () => {
      const helper = state.mode === 'helper';
      const plan = state.plan;
      return `
        <h2 class="wizard-section-title">${escapeHtml(helper ? T('wizard.password_title_helper') : T('wizard.password_title_interactive'))}</h2>
        <p class="wizard-section-sub">${escapeHtml(node.isLocal ? T('sudo.explain_local') : T('sudo.explain_remote', { node: node.nodeName }))}</p>
        <div class="stack">
          <tf-input id="nas-wz-pass" type="password" autocomplete="current-password" autofocus label="${escapeAttr(T('sudo.password_label', { user: env?.elevation?.coreUser || 'tentaflow', node: node.nodeName }))}" value="${escapeAttr(state.password)}"></tf-input>
          ${helper ? (plan ? `
            ${plan.helperSourcePresent ? '' : `<div class="wizard-warning danger">${escapeHtml(T('wizard.helper_source_missing', { path: plan.helperSource }))}</div>`}
            <p class="wizard-section-sub">${escapeHtml(T('wizard.plan_intro'))}</p>
            <pre class="cmd mono">${escapeHtml(plan.commands.map((c) => c.join(' ')).join('\n'))}</pre>
            <div class="muted">${escapeHtml(T('wizard.plan_sudoers', { path: plan.sudoersPath }))} <span class="mono">${escapeHtml(plan.sudoersLine)}</span></div>
          ` : `<div class="muted">${escapeHtml(T('elevation.plan_loading'))}</div>`) : `
            <div class="form-grid-2">
              <tf-select id="nas-wz-ttl" label="${escapeAttr(T('wizard.ttl_label'))}"></tf-select>
              <div class="explain-box">${escapeHtml(T('wizard.ttl_explain'))}</div>
            </div>`}
        </div>`;
    };

    const stepRun = () => {
      if (state.result) {
        const ok = state.result.ok;
        return `<div class="result-box ${ok ? 'ok' : 'err'}">${sprite(ok ? 'check-circle' : 'alert')}<h3>${escapeHtml(ok ? T('wizard.done_title') : T('wizard.failed_title'))}</h3><p>${escapeHtml(state.result.detail || '')}</p></div>
          ${state.job ? `<pre class="job-log mono">${escapeHtml((state.job.log || []).join('\n'))}</pre>` : ''}`;
      }
      return `
        <h2 class="wizard-section-title">${escapeHtml(T('wizard.run_title'))}</h2>
        <p class="wizard-section-sub">${escapeHtml(state.mode === 'helper' ? T('wizard.run_sub_helper') : T('wizard.run_sub_interactive'))}</p>
        ${state.job ? `<tf-progress-bar value="${Number(state.job.progressPct) || 0}" tone="accent" label="${escapeAttr(T('jobs.status_' + state.job.status))}"></tf-progress-bar><pre class="job-log mono mt-sm">${escapeHtml((state.job.log || []).join('\n'))}</pre>` : `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`}`;
    };

    const stepRestore = () => `
      <h2 class="wizard-section-title">${escapeHtml(T('wizard.restore_title'))}</h2>
      <p class="wizard-section-sub">${escapeHtml(T('wizard.restore_sub'))}</p>
      <div class="explain-box">${escapeHtml(T('config.import_explain'))}</div>
      <div id="nas-wz-restore" class="mt-md"></div>`;

    const restoreReady = () => Boolean(state.restore.json && state.restore.plan?.items?.length && !planBlocked(state.restore.plan.items));

    const footer = () => {
      if (state.step === 3) {
        return `
          <tf-button variant="ghost" data-wizard-skip>${escapeHtml(T('wizard.restore_skip'))}</tf-button>
          <span class="spacer"></span>
          <tf-button variant="primary" icon="check" data-wizard-next ${restoreReady() ? '' : 'disabled'}>${escapeHtml(T('wizard.restore_apply'))}</tf-button>`;
      }
      const last = state.step === 2;
      const finished = last && state.result;
      const running = last && !state.result;
      const toRestore = finished && restore && state.result.ok;
      return `
        <tf-button variant="ghost" data-wizard-cancel ${running ? 'disabled' : ''}>${escapeHtml(I18n.t('common.cancel'))}</tf-button>
        <tf-button variant="ghost" icon="chevron-left" data-wizard-back ${state.step === 0 || last ? 'disabled' : ''}>${escapeHtml(I18n.t('common.back'))}</tf-button>
        <span class="spacer"></span>
        ${finished
          ? `<tf-button variant="primary" icon="${toRestore ? 'chevron-right' : 'check'}" data-wizard-next>${escapeHtml(toRestore ? I18n.t('common.next') : I18n.t('common.close'))}</tf-button>`
          : `<tf-button variant="primary" icon="${state.step === 1 ? 'check' : 'chevron-right'}" data-wizard-next ${running ? 'disabled' : ''}>${escapeHtml(state.step === 1 ? (state.mode === 'helper' ? T('wizard.provision') : T('wizard.arm')) : I18n.t('common.next'))}</tf-button>`}`;
    };

    const draw = () => {
      win.innerHTML = `
        <div slot="body">
          ${header()}
          <div class="install-step-body">${[stepMode, stepPassword, stepRun, stepRestore][state.step]()}</div>
        </div>
        <div slot="footer">${footer()}</div>`;
      wire();
    };

    const wire = () => {
      win.querySelector('#nas-wz-mode')?.addEventListener('change', (e) => { state.mode = e.detail.value; });
      const pass = win.querySelector('#nas-wz-pass');
      if (pass) {
        pass.addEventListener('input', () => { state.password = pass.value; });
        pass.addEventListener('change', () => { state.password = pass.value; });
        pass.addEventListener('keydown', (e) => { if (e.key === 'Enter') next(); });
      }
      const ttl = win.querySelector('#nas-wz-ttl');
      if (ttl) {
        const nodeDefault = env?.elevation?.ttlSecs || 900;
        ttl.setOptions([
          { value: '0', label: T('wizard.ttl_default', { d: fmtDuration(nodeDefault) }) },
          { value: '300', label: fmtDuration(300) },
          { value: '900', label: fmtDuration(900) },
          { value: '3600', label: fmtDuration(3600) },
          { value: '14400', label: fmtDuration(14400) },
          { value: '28800', label: fmtDuration(28800) },
        ], String(state.ttl));
        ttl.addEventListener('change', (e) => { state.ttl = Number(e.detail.value) || 0; });
      }
      const restoreHost = win.querySelector('#nas-wz-restore');
      if (restoreHost) {
        mountImportPicker(this, restoreHost, {
          onState: (r) => {
            state.restore = r;
            const btn = win.querySelector('[data-wizard-next]');
            if (!btn) return;
            if (restoreReady()) btn.removeAttribute('disabled'); else btn.setAttribute('disabled', '');
          },
        });
      }
      win.querySelector('[data-wizard-skip]')?.addEventListener('click', () => win.close());
      win.querySelector('[data-wizard-cancel]')?.addEventListener('click', () => win.close());
      win.querySelector('[data-wizard-back]')?.addEventListener('click', () => { if (state.step > 0 && state.step < 2) { state.step--; draw(); } });
      win.querySelector('[data-wizard-next]')?.addEventListener('click', next);
    };

    const next = async () => {
      if (state.step === 0) {
        state.step = 1;
        draw();
        if (state.mode === 'helper' && !state.plan) {
          try {
            const r = await this.nas('tentaNasElevationPlanRequest', {});
            state.plan = r.plan;
          } catch (e) {
            state.plan = { helperSource: '', helperSourcePresent: false, helperPath: '', sudoersPath: '', sudoersLine: '', coreUser: '', coreVersion: '', commands: [[errMessage(e)]] };
          }
          if (state.step === 1 && win.isConnected) draw();
        }
        return;
      }
      if (state.step === 1) {
        if (!state.password) { win.querySelector('#nas-wz-pass')?.setAttribute('error', T('sudo.password_required')); return; }
        state.step = 2;
        draw();
        await run();
        return;
      }
      if (state.step === 2 && restore && state.result?.ok) {
        state.step = 3;
        draw();
        return;
      }
      if (state.step === 3) {
        if (!restoreReady()) return;
        const btn = win.querySelector('[data-wizard-next]');
        btn?.setAttribute('disabled', '');
        const started = await applyImport(this, state.restore.json, () => { this.loadNodes(); if (!this.disposed) this.drawTab(); });
        if (started) win.close();
        else btn?.removeAttribute('disabled');
        return;
      }
      win.close();
    };

    const run = async () => {
      try {
        if (state.mode === 'helper') {
          const r = await this.nas('tentaNasElevationProvisionRequest', { sudoPassword: state.password }, { timeoutMs: ADMIN_TIMEOUT_MS });
          state.password = '';
          state.job = r.job;
          draw();
          await pollJob();
        } else {
          const r = await this.nas('tentaNasElevationArmRequest', { sudoPassword: state.password, ttlSecs: state.ttl }, { timeoutMs: ADMIN_TIMEOUT_MS });
          state.password = '';
          state.result = { ok: true, detail: T('wizard.armed_until', { t: fmtDate(r.elevation.armedUntil) }) };
          draw();
        }
      } catch (e) {
        state.password = '';
        state.result = { ok: false, detail: errMessage(e) };
        draw();
      }
      this.refreshHeader(false).then(() => { if (this.tab === 'environment' && !this.disposed) this.drawTab(); });
    };

    const pollJob = async () => {
      if (!win.isConnected || !state.job) return;
      try {
        const r = await this.nas('tentaNasJobGetRequest', { jobId: state.job.jobId });
        state.job = r.job;
      } catch (e) {
        state.result = { ok: false, detail: errMessage(e) };
        draw();
        return;
      }
      const s = state.job.status;
      if (s === 'running' || s === 'queued') {
        draw();
        state.timer = setTimeout(pollJob, POLL_JOB_MODAL_MS);
        return;
      }
      state.result = { ok: s === 'succeeded', detail: s === 'succeeded' ? T('wizard.provisioned') : (state.job.error || T('jobs.status_' + s)) };
      draw();
    };

    win.addEventListener('close-request', () => {
      if (state.timer) clearTimeout(state.timer);
      if (this.openWindow === win) this.openWindow = null;
    });
    draw();
    document.body.appendChild(win);
    if (presetMode) next();
  },

  // Job log viewer; polls while the job runs so a package install streams
  // its output line by line.
  openJobLog(jobId, onFinish = null) {
    const win = document.createElement('tf-window');
    win.className = 'nas-modal';
    win.setAttribute('title', T('jobs.log'));
    win.setAttribute('icon', 'file-text');
    win.setAttribute('buttons', 'close');
    win.setAttribute('draggable', '');
    win.setAttribute('width', '720');
    win.setAttribute('initial-x', 'center');
    win.setAttribute('initial-y', 'center');
    win.innerHTML = `<div slot="body" class="stack"><div id="nas-joblog-head" class="muted">${escapeHtml(I18n.t('common.loading'))}</div><pre class="job-log mono" id="nas-joblog"></pre></div>
      <div slot="footer"><tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.close'))}</tf-button></div>`;
    document.body.appendChild(win);
    let timer = null;
    let notified = false;
    const poll = async () => {
      if (!win.isConnected) return;
      try {
        const r = await this.nas('tentaNasJobGetRequest', { jobId });
        const j = r.job;
        const head = win.querySelector('#nas-joblog-head');
        const pre = win.querySelector('#nas-joblog');
        if (!head || !pre) return;
        head.innerHTML = `${escapeHtml(jobKindLabel(j.kind))} <span class="mono">${escapeHtml(j.subject)}</span> <tf-chip status="${jobTone(j.status)}" label="${escapeAttr(T('jobs.status_' + j.status))}"></tf-chip> · ${escapeHtml(T('jobs.started_by', { by: j.startedBy, t: fmtAgo(j.startedAt) }))}${j.error ? `<div class="num-err mt-sm">${escapeHtml(j.error)}</div>` : ''}`;
        const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 8;
        pre.textContent = (j.log || []).join('\n');
        if (atBottom) pre.scrollTop = pre.scrollHeight;
        if (j.status === 'running' || j.status === 'queued') timer = setTimeout(poll, POLL_JOB_MODAL_MS);
        else if (onFinish && !notified) { notified = true; onFinish(j); }
      } catch (e) {
        const head = win.querySelector('#nas-joblog-head');
        if (head) head.textContent = errMessage(e);
      }
    };
    win.addEventListener('action', (e) => { if (e.detail.action === 'cancel') win.close(); });
    win.addEventListener('close-request', () => { if (timer) clearTimeout(timer); });
    poll();
  },
};

// Where a fleet alert row takes the admin: the subject decides both the tab
// and the label, so "Uzbrój" never appears on a disk alert.
function alertTarget(alert) {
  if (!alert) return { act: 'details', tab: 'overview', extra: {} };
  if (alert.subjectKind === 'elevation') return { act: 'arm', tab: 'environment', extra: {} };
  if (alert.subjectKind === 'disk') {
    return alert.subjectId
      ? { act: 'details', tab: 'disks', extra: { disk: alert.subjectId } }
      : { act: 'disks', tab: 'disks', extra: {} };
  }
  if (alert.subjectKind === 'pool') return { act: 'pool', tab: 'pools', extra: { pool: alert.subjectId } };
  return { act: 'details', tab: 'overview', extra: {} };
}

function roleTone(role) {
  return role === 'free' ? 'info' : role === 'system' ? 'warn' : role === 'partitioned' ? 'info' : 'ok';
}

function trendHtml(now, weekAgo) {
  const a = Number(now) || 0;
  const b = Number(weekAgo) || 0;
  if (a === b) return `<span class="text-3">${escapeHtml(T('disk.trend_flat'))}</span>`;
  const up = a > b;
  return `<span class="${up ? 'num-warn' : ''}">${up ? '▲' : '▼'} ${escapeHtml(String(Math.abs(a - b)))}</span>`;
}

// A node that never published a summary has its counters absent; the card
// math wants zeros, not NaN.
function normalizeNode(n) {
  return {
    ...n,
    disksTotal: Number(n.disksTotal) || 0,
    disksWarning: Number(n.disksWarning) || 0,
    poolsTotal: Number(n.poolsTotal) || 0,
    sharesTotal: Number(n.sharesTotal) || 0,
    alertsActive: Number(n.alertsActive) || 0,
    capacityBytes: Number(n.capacityBytes) || 0,
    usedBytes: Number(n.usedBytes) || 0,
  };
}

export default TentaNasScreen;
