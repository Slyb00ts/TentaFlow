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
  T, sprite, channelMode, POLL_DISKS_MS, POLL_OVERVIEW_MS, IO_WINDOW_SECS, TEMP_WINDOW_SECS, POLL_FLEET_MS, POLL_JOB_MODAL_MS, ADMIN_TIMEOUT_MS,
  parseServerTs, fmtDate, fmtAgo, fmtDuration, fmtWindow, fmtBytes, fmtOptionalBytes, fmtMBps, pct, healthClass, healthChip, errMessage, jobTone, jobKindLabel,
  layoutLabel, stateChipHtml, stateTone, stateLabel, fmtSchedule, nodeLabel, jobAuthor, runDiskBatch, refusedBatchNames,
  firstDiskReasonWord, diskReasonsText, diskHealthChipLabel, replacementAdviceText, ADVICE_KINDS, alertText,
} from '/js/modules/tentanas/format.js';
import { setAttr, setText, patchHtml, patchKeyedList, paintStatCards, paintJobLog, setRowsIfChanged } from '/js/modules/tentanas/dom-patch.js';
import { nodeT, nodeHeadSub } from '/js/modules/tentanas/node-phrase.js';
import { isOpaqueId, isDiskIdShape, scrubIds } from '/js/modules/tentanas/machine-id.js';
import { drawPools, poolDescription } from '/js/modules/tentanas/pools.js';
import { drawPoolDetail, openReplaceWizard } from '/js/modules/tentanas/pool-detail.js';
import { openPoolWizard } from '/js/modules/tentanas/pool-wizard.js';
import { drawTasks, openSmartScheduleEditor, jobSubject, jobRowSkeleton, paintJobRow } from '/js/modules/tentanas/tasks.js';
import { drawShares, protocolChipHtml } from '/js/modules/tentanas/shares.js';
import { warningHtml } from '/js/modules/tentanas/dialogs.js';
import { openDiskWipeDialog } from '/js/modules/tentanas/disk-wipe.js';
import { exportConfig, mountImportPicker, applyImport, planBlocked } from '/js/modules/tentanas/config-transfer.js';
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
import { openTargetDetail } from '/js/modules/tentanas/targets.js';
import { drawElasticDetail, elasticState, elasticProtection, elasticCapacity } from '/js/modules/tentanas/elastic-detail.js';

// -----------------------------------------------------------------------------
// Screen-local helpers
// -----------------------------------------------------------------------------

// The in-place patching helpers (`setAttr`, `setText`, `patchHtml`,
// `paintStatCards`) live in modules/tentanas/dom-patch.js, imported above —
// the tab modules this file imports need them too, and a leaf module both
// sides import avoids the cycle that exporting them from here would create.

// The feature that backs SMART on a node. The telemetry banner may offer an
// install only when the environment probe reports THIS absent — the banner
// itself is never the evidence (a working smartctl can still fail on single
// disks, and offering "Doinstaluj" then names a cause that does not exist).
const SMART_FEATURE_ID = 'smartmontools';
const FEATURE_ABSENT = new Set(['missing_package', 'missing', 'missing_module']);

function sparklineSvg(points, cls = '', w = 90, h = 22) {
  const pts = (points || []).map(Number).filter((v) => Number.isFinite(v));
  if (pts.length < 2) return '<svg viewBox="0 0 90 22"></svg>';
  const max = Math.max(1, ...pts);
  const step = w / (pts.length - 1);
  const coords = pts.map((v, i) => `${(i * step).toFixed(1)},${(h - 1 - (v / max) * (h - 2)).toFixed(1)}`).join(' ');
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none"><polyline class="${escapeAttr(cls)}" points="${coords}"/></svg>`;
}

// "Services running" is a claim about EVERY service a node is expected to
// serve, never about any one of them. The chip used to go green as soon as one
// service on one node ran — a fleet with smbd up on one node and nfsd dead on
// another read as healthy. A service is expected when the node reports it
// installed, or when a share of that protocol exists there (a share whose
// service is not even installed is the worst case, not an exemption).
// `entries` are `{ node, answer }`, where `answer` is the node's
// SharesListResponse or null when the node did not answer.
// `named` prefixes each failing service with its node (the fleet chip); the
// node's own header leaves the node name out.
function servicesVerdict(entries, { named = true } = {}) {
  const down = [];
  const silent = [];
  let expected = 0;
  for (const { node, answer } of entries) {
    if (!answer) { silent.push(nodeLabel(node)); continue; }
    const used = new Set((answer.shares || []).map((s) => String(s.protocol || '').toLowerCase()));
    for (const svc of answer.services || []) {
      const proto = String(svc.protocol || '').toLowerCase();
      if (!svc.installed && !used.has(proto)) continue;
      expected += 1;
      if (!svc.running) down.push(named ? `${nodeLabel(node)}: ${proto.toUpperCase()}` : proto.toUpperCase());
    }
  }
  const unknown = silent.length ? T('fleet.chip_services_unknown', { nodes: silent.join(', ') }) : '';
  if (down.length) return { status: 'warn', label: T('fleet.chip_services_down_list', { list: down.join(', ') }), title: unknown };
  if (silent.length) return { status: 'warn', label: unknown, title: '' };
  if (!expected) return { status: 'neutral', label: T('fleet.chip_services_none'), title: '' };
  return { status: 'ok', label: T('fleet.chip_services'), title: '' };
}

function servicesChipHtml(v) {
  return `<tf-chip status="${v.status}" dot label="${escapeAttr(v.label)}"${v.title ? ` title="${escapeAttr(v.title)}"` : ''}></tf-chip>`;
}

// n02 ARC card: a stable skeleton (the donut ring plus the five stat rows)
// painted with `setText`/`setAttr`/`patchHtml`-per-slot instead of one string
// for the whole card. `arc.sizeBytes`, `hitRatio` and the MRU/MFU split move
// on almost every 5 s poll — rebuilding the whole card for that destroyed the
// donut and every row (BLOCKER 2, n01-n10 critic 2026-09-21).
function arcSkeletonHtml() {
  return `
    <div class="arc-flex">
      <div class="donut" data-role="donut">
        <div class="dn-center"><div class="dn-val" data-role="val"></div><div class="dn-lbl" data-role="lbl"></div></div>
      </div>
      <div class="stat-rows" style="flex:1">
        <div class="sr"><span class="k">${escapeHtml(T('arc.row_usage'))}</span><span class="v" data-role="usage"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('arc.row_split'))}</span><span class="v" data-role="split"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('arc.row_demand'))}</span><span class="v" data-role="demand"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('arc.row_slog'))}</span><span class="v" data-role="slog"></span></div>
        <div class="sr"><span class="k">${escapeHtml(T('arc.row_l2arc'))}</span><span class="v" data-role="l2arc"></span></div>
      </div>
    </div>`;
}

// n02 pool-mini row (ZFS pool): a stable skeleton keyed by pool name, painted
// per poll. `poolDescription(p)` can include a live scrub percentage/ETA, and
// the used/usable pair and fill bar move on every poll — none of that may
// rebuild the row itself (its click handler, via delegation on the host, does
// not care, but the node's identity across a poll does).
function poolMiniSkeleton(p) {
  return `
    <div class="pool-mini" data-pool="${escapeAttr(p.name)}">
      <div class="pm-ico">${sprite('layers')}</div>
      <div class="pm-main">
        <div class="pm-name"><span class="mono">${escapeHtml(p.name)}</span> <tf-chip data-role="state" dot></tf-chip></div>
        <div class="pm-sub" data-role="sub"></div>
        <tf-progress-bar data-role="bar" size="sm" tone="accent"></tf-progress-bar>
      </div>
      <div class="kv-inline"><span class="v" data-role="kv"></span></div>
    </div>`;
}

function paintPoolMini(row, p) {
  if (!row) return;
  const chip = row.querySelector('[data-role="state"]');
  setAttr(chip, 'status', stateTone(p.state));
  setAttr(chip, 'label', stateLabel(p.state));
  setText(row.querySelector('[data-role="sub"]'), poolDescription(p));
  setAttr(row.querySelector('[data-role="bar"]'), 'value', pct(p.usedBytes, p.usableBytes));
  setText(row.querySelector('[data-role="kv"]'), `${fmtBytes(p.usedBytes)} / ${fmtBytes(p.usableBytes)}`);
}

// One Elastic Array as a `.pool-mini` row of the node dashboard (n02:297) —
// the same shape a ZFS pool gets, so the dashboard shows every pool the Pools
// tab lists instead of only the ZFS half. Two chips carry the two questions an
// array raises: is it up, and is what it holds protected.
//
// The fill bar is rendered ONLY for a measured array: `pct` reads a missing
// capacity as 0, and a bar drawn at 0% claims an empty array where the node
// merely never measured one. The used/usable pair says "—" there instead. The
// bar's PRESENCE can flip between polls (a probe starts measuring), so it
// lives in its own small `patchHtml`-managed slot rather than in the outer
// skeleton the keyed list compares — the skeleton itself never changes for a
// given array, so the row keeps its identity across a poll regardless.
function arrayMiniSkeleton(a) {
  return `
      <div class="pool-mini" data-array="${escapeAttr(a.name)}">
        <div class="pm-ico">${sprite('cylinder')}</div>
        <div class="pm-main">
          <div class="pm-name"><span class="mono">${escapeHtml(a.name)}</span> <tf-chip data-role="state" dot></tf-chip> <tf-chip data-role="protection" size="sm"></tf-chip></div>
          <div class="pm-sub" data-role="sub"></div>
          <div data-role="bar-slot"></div>
        </div>
        <div class="kv-inline"><span class="v" data-role="kv"></span></div>
      </div>`;
}

function paintArrayMini(row, a) {
  if (!row) return;
  const state = elasticState(a);
  const protection = elasticProtection(a);
  setAttr(row.querySelector('[data-role="state"]'), 'status', state.tone);
  setAttr(row.querySelector('[data-role="state"]'), 'label', state.label);
  setAttr(row.querySelector('[data-role="protection"]'), 'status', protection.tone);
  setAttr(row.querySelector('[data-role="protection"]'), 'label', protection.label);
  const fs = a.filesystem ? String(a.filesystem).toUpperCase() : T('elastic.unknown');
  const topology = T('elastic.topology', { data: (a.dataDisks || []).length, parity: (a.parityDisks || []).length, fs });
  setText(row.querySelector('[data-role="sub"]'), `Elastic Array · ${topology}`);
  const barSlot = row.querySelector('[data-role="bar-slot"]');
  if (elasticCapacity(a).measured) {
    patchHtml(barSlot, '<tf-progress-bar size="sm" tone="accent"></tf-progress-bar>');
    setAttr(barSlot.querySelector('tf-progress-bar'), 'value', pct(a.usedBytes, a.usableBytes));
  } else {
    patchHtml(barSlot, '');
  }
  setText(row.querySelector('[data-role="kv"]'), `${fmtOptionalBytes(a.usedBytes)} / ${fmtOptionalBytes(a.usableBytes)}`);
}

// n02 overview alert row: a stable skeleton keyed by `alertId`. `severity`,
// `subject` and whether the alert is acked decide the row's SHAPE (its class,
// icon and which of Ack/acked-chip it offers) and stay fixed here — a change
// to any of those really is a different row. The title and detail are NOT
// fixed: `raise_coded_alert` refreshes the code, its parameters and the
// English on the node on every re-raise, and a cache-stuck alert's wait time
// moves every minute below 1 h and every hour above (elastic.rs
// `coarse_wait_secs`). So, like `fmtAgo(a.raisedAt)`, they are left as empty
// slots here and written into place by `paintAlertRow` — baking them into
// this markup, as the old code did, rebuilt the whole row (Ack button
// included) every time either one ticked, not just once a minute.
//
// Both are worded by `alertText` (format.js) from the alert's code; the
// node's English is the title's tooltip, never the text.
//
// The sub-line joins only what is there: a disk alert with one reason has
// it in the title and an EMPTY detail, so the detail's separator is a slot
// of its own, written by `paintAlertRow` with the detail and emptied without
// it. No part of the row is an id, not even as a tooltip (owner's rule): the
// subject is named by `alertSubjectName`, or by its kind alone.
function alertRowSkeleton(a) {
  const target = alertTarget(a);
  const subjectLabel = [subjectKindLabel(a.subjectKind), alertSubjectName(a.subjectId, a.subjectKind)].filter(Boolean).join(' ');
  const subject = subjectLabel ? `${escapeHtml(subjectLabel)} · ` : '';
  return `
      <div class="alert-row ${escapeAttr(a.severity)} ${a.ackedAt ? 'acked' : ''}" data-alert="${escapeAttr(a.alertId)}">
        ${sprite(a.severity === 'critical' ? 'alert' : a.severity === 'warning' ? 'alert' : 'info')}
        <div class="a-main">
          <div class="a-title" data-role="title"></div>
          <div class="a-sub"><span data-role="detail"></span><span data-role="detail-sep"></span>${subject}<span data-role="ago"></span></div>
          <details class="a-node" data-role="node" hidden><summary>${escapeHtml(T('alerts.node_text'))}</summary><div class="a-node-text" data-role="node-text"></div></details>
        </div>
        ${a.ackedAt ? `<tf-chip status="info" label="${escapeAttr(T('alerts.acked'))}"></tf-chip>` : `<tf-button size="sm" variant="ghost" icon="check" data-ack="${escapeAttr(a.alertId)}">${escapeHtml(T('alerts.ack'))}</tf-button>`}
        <tf-button size="sm" variant="secondary" icon="chevron-right" data-goto="${escapeAttr(a.alertId)}">${escapeHtml(T('fleet.act_' + target.act))}</tf-button>
      </div>`;
}

function paintAlertRow(row, a, nameOf) {
  if (!row) return;
  const text = alertText(a, { nameOf });
  const title = row.querySelector('[data-role="title"]');
  setText(title, text.title);
  setAttr(title, 'title', text.tooltip);
  setText(row.querySelector('[data-role="detail"]'), text.detail);
  setText(row.querySelector('[data-role="detail-sep"]'), text.detail ? ' · ' : '');
  setText(row.querySelector('[data-role="ago"]'), fmtAgo(a.raisedAt));
  // A detail that points at the node's text ("…w treści węzła") must reach
  // it without a hover too: the same text behind a tap (wave-4 critic minor
  // 13). Patched in place, so an opened one stays open across polls.
  setAttr(row.querySelector('[data-role="node"]'), 'hidden', !text.nodeText);
  setText(row.querySelector('[data-role="node-text"]'), text.nodeText ? text.tooltip : '');
}

// The n01 fleet table's alert cell, worded like the n02 row (`alertText`);
// the node's English is the cell's tooltip, and — when the detail points at
// it — also one tap away, for a reader with no hover.
function fleetAlertCellHtml(alert, nameOf) {
  const text = alertText(alert, { nameOf });
  const node = text.nodeText
    ? `<details class="l2"><summary>${escapeHtml(T('alerts.node_text'))}</summary>${escapeHtml(text.tooltip)}</details>`
    : '';
  return `<div class="cell-2" title="${escapeAttr(text.tooltip)}"><div class="l1">${escapeHtml(text.title)}</div><div class="l2">${escapeHtml(text.detail)}</div>${node}</div>`;
}

// One square per fleet node, in node order: the mount state of a share.
function mountDotsHtml(mounts, nodes) {
  return `<span class="mount-dots">${nodes.map((n) => {
    const m = (mounts || []).find((x) => x.nodeId === n.nodeId);
    const state = m ? m.state : 'na';
    const cls = state === 'mounted' || state === 'source' ? '' : state === 'pending' ? 'pending' : state === 'error' ? 'error' : 'na';
    return `<span class="md ${cls}" title="${escapeAttr(`${nodeLabel(n)}: ${m ? (m.detail || m.state) : T('fleet.mount_na')}`)}"></span>`;
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
    this.targetId = params.target || null;
    this.sharesFilter = 'all';
    this.sharesQuery = '';
    // Pools tab: the open pool, its inner tab and the dataset it focuses on
    // survive a reload through the hash (n06/n09).
    this.array = params.array || null;
    this.pool = this.array ? null : params.pool || null;
    if (this.array) this.tab = 'pools';
    this.poolTab = params.ptab || 'topology';
    this.dataset = this.array ? null : params.dataset || null;
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
    this.fleetVersions = null;
    // The last environment probe's error, so the tab body can state a FAILED
    // probe instead of painting a dashboard over a node that cannot answer.
    // An undefined `environment` means "not asked yet" — a different state
    // from "asked, and it failed".
    this.environmentError = null;
    // `?setup=1` — the admin has just installed TentaNas and the shell sent
    // them straight here (addons.js). Install is fleet-wide; the privilege
    // channel is per node, so this forces the setup step for ONE node.
    this.forceSetup = params.setup === '1' || params.setup === true;

    try {
      const me = await ApiBinary.one('authMeRequest');
      this.me = me;
      this.isAdmin = Boolean(me && (me.role === 'admin' || me.isAdmin));
    } catch (e) {
      this.root.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }

    await this.loadNodes();
    if (this.disposed) return;
    if (this.nodeId && !this.nodes.some((n) => n.nodeId === this.nodeId)) this.nodeId = null;
    // Straight after an install the route names no node, and the fleet list is
    // not where a channel gets configured: open the node the admin is on.
    if (this.forceSetup && !this.nodeId) {
      const local = this.nodes.find((n) => n.nodeId === this.localNodeId && n.instanceStatus === 'ready');
      if (local) this.nodeId = local.nodeId;
    }
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
    if (this.nodeId && this.tab === 'shares' && this.targetId) q.set('target', this.targetId);
    if (this.nodeId && this.tab === 'pools' && this.array) q.set('array', this.array);
    if (this.nodeId && this.tab === 'pools' && this.pool && !this.array) {
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
    if (nodeId !== this.nodeId) document.querySelector('tf-window.nas-elastic-wizard')?.close();
    this.nodeId = nodeId;
    this.diskId = extra.disk || null;
    this.targetId = null;
    this.sharesFilter = 'all';
    this.sharesQuery = '';
    this.array = extra.array || null;
    this.pool = this.array ? null : extra.pool || null;
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
    const counts = nodeTabCounts(n, this.fleet?.rows);
    const title = (t) => (t ? ` title="${escapeAttr(t)}"` : '');
    return `
      <tf-tabs variant="underline" value="${escapeAttr(active || '')}" id="nas-tabs">
        <tf-tab id="overview" icon="bar-chart">${escapeHtml(T('tabs.overview'))}</tf-tab>
        <tf-tab id="disks" icon="cylinder" count="${Number(n.disksTotal) || 0}">${escapeHtml(T('tabs.disks'))}</tf-tab>
        <tf-tab id="pools" icon="layers" count="${escapeAttr(counts.pools)}"${title(counts.poolsTitle)}>${escapeHtml(T('tabs.pools'))}</tf-tab>
        <tf-tab id="shares" icon="share" count="${escapeAttr(counts.shares)}"${title(counts.sharesTitle)}>${escapeHtml(T('tabs.shares'))}</tf-tab>
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
        // The fleet view passes a getter: the strip is wired once, and its
        // default node is whatever is ready at CLICK time.
        const target = typeof defaultNode === 'function' ? defaultNode() : defaultNode;
        if (target) this.selectNode(target.nodeId, value);
        return;
      }
      if (value === this.tab) return;
      this.tab = value;
      this.diskId = null;
      this.targetId = null;
      this.pool = null;
      this.array = null;
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
    // Through setAttr, never a raw setAttribute: this runs on the 5 s overview
    // poll, `count` is in TfTab.observedAttributes, and an identical value
    // still reaches attributeChangedCallback — which rebuilds the tab's button
    // wholesale. The badge blinked twelve times a minute for a number that had
    // not moved.
    setAttr(tab, 'count', this.runningJobs || null);
    if (this.runningJobs) setAttr(tab, 'count-tone', 'accent');
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
    // Asked of EVERY supported node, once per mount: the first node's answer
    // printed as "TentaNas 1.4.0" claimed a fleet-wide version nothing had
    // checked, and a fleet halfway through an upgrade is exactly when the
    // difference matters.
    if (this.fleetVersions == null && supported.length) {
      const versions = await Promise.all(supported.map((n) => this.nasOn(n, 'tentaNasEnvironmentRequest', { refresh: false })
        .then((r) => String(r?.environment?.elevation?.coreVersion || '').trim(), () => '')));
      this.fleetVersions = supported.map((n, i) => ({ node: n, version: versions[i] }));
    }
    this.fleet = { rows, at: new Date().toISOString() };
  },

  // The version line of the fleet header: one version when every supported
  // node answered with the same one, otherwise each version with the nodes
  // that run it — and the nodes that did not say, as "—". Null when no node
  // answered at all, so the header says nothing rather than something false.
  fleetVersionText() {
    const entries = this.fleetVersions || [];
    if (!entries.some((e) => e.version)) return null;
    const groups = new Map();
    for (const e of entries) {
      const key = e.version || '—';
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(nodeLabel(e.node));
    }
    if (groups.size === 1) return T('fleet.head_version', { v: entries[0].version });
    return T('fleet.head_versions', { list: [...groups].map(([v, names]) => `${v} (${names.join(', ')})`).join(' · ') });
  },

  fleetShares() {
    return (this.fleet?.rows || []).flatMap((r) => (typeof r.shares === 'string' ? [] : (r.shares.shares || []).map((s) => ({ share: s, node: r.node }))));
  },

  // Every reachable node's services, and every node that did not answer.
  fleetServicesChip() {
    return servicesVerdict((this.fleet?.rows || []).map((r) => ({ node: r.node, answer: typeof r.shares === 'string' ? null : r.shares })));
  },

  // Builds the fleet screen ONCE. Every value that a poll can move lives in a
  // host this leaves empty and `paintFleet` fills in — the screen is never
  // rebuilt from here on.
  drawFleet() {
    this.clearTimers();
    const nodes = this.nodes;
    const ready = nodes.filter((n) => n.instanceStatus === 'ready');

    // The fleet is the root level: its bar holds one current item and no
    // link, so there is nothing to wire. Every bar that links back is the
    // node view's, written by `setCrumbTail` and handled by `crumbAction`.
    this.root.innerHTML = `
      <tf-breadcrumb class="nas-crumbs"><tf-breadcrumb-item current>${escapeHtml(T('title'))}</tf-breadcrumb-item></tf-breadcrumb>
      <div class="tf-detail-header">
        <div class="big-ico">${sprite('cylinder')}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(T('title'))} <span id="nas-fleet-chips"></span></div>
          <div class="d-sub" id="nas-fleet-sub"></div>
          <div class="d-badges" id="nas-fleet-badges"></div>
        </div>
        <div class="d-actions">
          <tf-button variant="ghost" icon="download" data-act="export-config">${escapeHtml(T('config.export'))}</tf-button>
          <tf-button variant="ghost" icon="refresh" data-act="refresh">${escapeHtml(T('reprobe'))}</tf-button>
        </div>
      </div>
      ${this.tabsHtml(null, ready[0] || null)}
      <div class="kpi" id="nas-fleet-kpi"></div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('desktop')} ${escapeHtml(T('fleet.nodes_title'))}</div>
          <span class="hint">${escapeHtml(T('fleet.nodes_hint'))}</span>
        </div>
        <div class="node-grid" id="nas-node-grid"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('alert')} ${escapeHtml(T('fleet.alerts_title'))} <tf-chip size="sm" id="nas-fleet-alerts-count" status="neutral" label="0"></tf-chip></div>
          <div class="actions"><tf-button variant="ghost" size="sm" icon="clock" data-act="alert-history">${escapeHtml(T('alerts.history'))}</tf-button></div>
        </div>
        <tf-table id="nas-fleet-alerts" empty-message="${escapeAttr(I18n.t('common.loading'))}">
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
        <tf-table id="nas-fleet-res-table" empty-message="${escapeAttr(I18n.t('common.loading'))}">
          <tf-column key="resource" label="${escapeAttr(T('fleet.col_resource'))}" renderer="html" fill></tf-column>
          <tf-column key="protocol" label="${escapeAttr(T('fleet.col_protocol'))}" renderer="html" nowrap></tf-column>
          <tf-column key="source" label="${escapeAttr(T('fleet.col_source'))}" renderer="html"></tf-column>
          <tf-column key="mounts" label="${escapeAttr(T('fleet.col_mounts'))}" renderer="html" nowrap width="140"></tf-column>
          <tf-column key="sessions" label="${escapeAttr(T('fleet.col_sessions'))}" renderer="num" width="90"></tf-column>
        </tf-table>
      </div>`;

    this.root.querySelector('[data-act="refresh"]').addEventListener('click', () => this.refreshFleet());
    this.root.querySelector('[data-act="export-config"]').addEventListener('click', () => exportConfig(this));
    this.root.querySelector('[data-act="alert-history"]').addEventListener('click', () => {
      const target = this.fleetAlertRows().find((r) => r.node)?.node || ready[0];
      if (target) this.selectNode(target.nodeId, 'jobs');
    });
    this.wireTabs(this.root.querySelector('#nas-tabs'), () => this.nodes.find((n) => n.instanceStatus === 'ready') || null);
    this.root.querySelector('#nas-node-grid').addEventListener('click', (e) => {
      const card = e.target.closest('.node-card[data-node]');
      if (!card || card.classList.contains('unsupported')) return;
      this.selectNode(card.dataset.node);
    });
    // Row actions are a function OF THE ROW, so they are wired once, here.
    // Re-assigning them on every poll (which is what the paint methods used to
    // do) is not merely redundant: `set rowActions` calls the table's
    // `_render()`, so setting `.rows` and `.rowActions` together ran TWO full
    // render passes per poll over both fleet tables.
    this.root.querySelector('#nas-fleet-alerts').rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const { node, alert } = row._row;
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      const target = alertTarget(alert);
      wrap.innerHTML = `<tf-button size="sm" variant="secondary" icon="chevron-right" data-act="go">${escapeHtml(T('fleet.act_' + target.act))}</tf-button>`;
      wrap.querySelector('[data-act="go"]').addEventListener('click', (e) => {
        e.stopPropagation();
        const now = live()._row;
        const t = alertTarget(now.alert);
        this.selectNode(now.node.nodeId, t.tab, t.extra);
      });
      return wrap;
    };
    this.root.querySelector('#nas-fleet-res-table').rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      wrap.innerHTML = `<tf-button size="sm" variant="secondary" icon="chevron-right" data-act="go">${escapeHtml(T('fleet.act_manage'))}</tf-button>`;
      wrap.querySelector('[data-act="go"]').addEventListener('click', (e) => { e.stopPropagation(); this.selectNode(live()._node.nodeId, 'shares'); });
      return wrap;
    };
    this.paintFleet();

    if (!this.fleet) this.loadFleetData().then(() => { if (!this.disposed && !this.nodeId) this.paintFleet(); });
    this.later(() => this.refreshFleet(), POLL_FLEET_MS);
  },

  // Everything on n01 that a poll can move, written into the screen that is
  // already there: header chips, the KPI attributes, the node grid and the two
  // tables. The KPI tiles and the table rows keep their elements and take only
  // the values that moved. The chip strips are patched as ONE string each, so
  // an unchanged poll touches nothing; a single value crossing a rounding
  // boundary does rebuild a whole strip, which is bearable for a handful of
  // chips that change together.
  //
  // The node GRID is not bearable that way and is patched PER CARD, keyed by
  // node id: on a live fleet some node's uptime or used-bytes ticks on nearly
  // every 10 s poll, and a joined-string compare rebuilt every card on the
  // screen each time — the biggest blink surface this screen had.
  paintFleet() {
    const root = this.root;
    const grid = root.querySelector('#nas-node-grid');
    if (!grid) return;
    const nodes = this.nodes;
    const ready = nodes.filter((n) => n.instanceStatus === 'ready');
    // Warnings and failures are counted apart, because a node that has LOST a
    // disk must not reach this screen as a warning. `disksWarning` counts
    // warnings only (fleet.rs) and the worst state leads everywhere below.
    const warnNodes = nodes.filter((n) => n.disksWarning > 0);
    const warnDisks = nodes.reduce((a, n) => a + n.disksWarning, 0);
    const critNodes = nodes.filter((n) => n.disksCritical > 0);
    const critDisks = nodes.reduce((a, n) => a + n.disksCritical, 0);
    const cap = nodes.reduce((a, n) => a + n.capacityBytes, 0);
    const used = nodes.reduce((a, n) => a + n.usedBytes, 0);
    // An Elastic Array is storage the node serves, so it is counted beside the
    // ZFS pools the capacity spans — and `arraysUnmeasured` says how many
    // arrays that capacity is MISSING, because a node leaves an array it
    // could not measure out of both figures rather than half of one.
    const pools = nodes.reduce((a, n) => a + n.poolsTotal, 0);
    const arrays = nodes.reduce((a, n) => a + n.arraysTotal, 0);
    const unmeasured = nodes.reduce((a, n) => a + n.arraysUnmeasured, 0);
    // A NAS node is one that serves storage. On a remote row only the ZFS
    // pools are published, so a remote node with none is NOT a client on that
    // evidence: its Elastic Arrays were never counted, and the badge says so.
    const nasNodes = ready.filter((n) => n.poolsTotal + (perOrgCounted(n) ? n.arraysTotal : 0) > 0);
    const arraysUncounted = ready.filter((n) => !perOrgCounted(n) && n.poolsTotal === 0);
    const poolsOnly = ready.filter((n) => !perOrgCounted(n));
    const unarmed = ready.filter((n) => channelMode(n.elevationMode) === 'unarmed');
    const shares = this.fleetShares();
    const loaded = Boolean(this.fleet);
    const alertRows = this.fleetAlertRows();

    const channelParts = ['helper', 'interactive', 'unarmed']
      .map((mode) => ({ mode, n: ready.filter((n) => channelMode(n.elevationMode) === mode).length }))
      .filter((p) => p.n > 0)
      .map((p) => T('fleet.badge_channel_part', { n: p.n, mode: T('elevation.short_' + p.mode) }))
      .join(' · ');
    const protoCounts = [...new Set(shares.map((s) => s.share.protocol))]
      .map((p) => T('fleet.kpi_protocol', { n: shares.filter((s) => s.share.protocol === p).length, protocol: p.toUpperCase() }))
      .join(' · ');

    // The worst state leads: a failure is `err` and says "failures", and only
    // a fleet with no failure at all falls back to the warning wording.
    const diskChip = critDisks
      ? { status: 'err', label: `${critDisks} ${T('kpi.failures_suffix', { n: critDisks })} (${critNodes.map(nodeLabel).join(', ')})` }
      : { status: warnDisks ? 'warn' : 'ok', label: warnDisks
        ? T('fleet.chip_warnings', { n: warnDisks, nodes: warnNodes.map(nodeLabel).join(', ') })
        : T('fleet.chip_ok') };
    patchHtml(root.querySelector('#nas-fleet-chips'), [
      `<tf-chip status="${diskChip.status}" dot label="${escapeAttr(diskChip.label)}"></tf-chip>`,
      loaded ? servicesChipHtml(this.fleetServicesChip()) : '',
    ].join(''));

    setText(root.querySelector('#nas-fleet-sub'), [
      T('fleet.head_scope'),
      T('fleet.head_nodes', { n: nodes.length }),
      T('fleet.head_supported', { n: ready.length }),
      this.fleetVersionText(),
      this.fleet ? T('refreshed', { t: fmtAgo(this.fleet.at) }) : null,
    ].filter(Boolean).join(' · '));

    patchHtml(root.querySelector('#nas-fleet-badges'), [
      `<tf-chip status="accent" label="${escapeAttr([
        T('fleet.badge_nas', { n: nasNodes.length, nodes: nasNodes.map(nodeLabel).join(' · ') }),
        arraysUncounted.length ? T('fleet.badge_nas_uncounted', { nodes: arraysUncounted.map(nodeLabel).join(', ') }) : null,
      ].filter(Boolean).join(' · '))}"${arraysUncounted.length ? ` title="${escapeAttr(T('fleet.arrays_not_counted_hint'))}"` : ''}></tf-chip>`,
      `<tf-chip status="${unarmed.length ? 'warn' : 'ok'}" icon="shield" label="${escapeAttr(T('fleet.badge_channels', { parts: channelParts || '—' }))}"></tf-chip>`,
      `<tf-chip label="${escapeAttr([
        T('fleet.badge_pools', { n: pools + arrays, capacity: fmtBytes(cap) }),
        poolsOnly.length ? T('kpi.capacity_pools_only', { n: poolsOnly.length }) : null,
      ].filter(Boolean).join(' · '))}"${poolsOnly.length ? ` title="${escapeAttr(T('fleet.arrays_not_counted_hint'))}"` : ''}></tf-chip>`,
      `<tf-chip status="info" icon="network" label="${escapeAttr(T('fleet.badge_mesh', { n: nodes.length }))}"></tf-chip>`,
    ].join(''));

    const kpi = root.querySelector('#nas-fleet-kpi');
    const built = paintStatCards(kpi, [
      { key: 'capacity', attrs: {
        label: T('kpi.fleet_capacity'), value: fmtBytes(cap), icon: 'database',
        // An array the fleet could not measure is named, not folded in: the
        // total above is then knowingly short of that array's disks, and a
        // percentage the reader trusts has to say so.
        delta: [
          T('kpi.capacity_delta', { used: fmtBytes(used), pct: pct(used, cap), n: pools + arrays }),
          unmeasured ? T('kpi.capacity_unmeasured', { n: unmeasured }) : null,
          // Remote rows carry their ZFS pools only (see `perOrgCounted`).
          poolsOnly.length ? T('kpi.capacity_pools_only', { n: poolsOnly.length }) : null,
        ].filter(Boolean).join(' · '),
      } },
      { key: 'health', className: 'clickable', attrs: {
        // The worst state leads, as on the node's own disk tile: a failure
        // counts as a failure and colours the tile `danger`, never `warning`.
        id: 'nas-fleet-health', label: T('kpi.fleet_health'), value: String(critDisks || warnDisks),
        suffix: critDisks ? T('kpi.failures_suffix', { n: critDisks }) : T('kpi.warnings_suffix', { n: warnDisks }),
        icon: 'cylinder',
        accent: critDisks ? 'danger' : warnDisks ? 'warning' : null,
        delta: critDisks || warnDisks
          ? T('kpi.fleet_health_on', { nodes: [...new Set([...critNodes, ...warnNodes])].map(nodeLabel).join(', ') })
          : T('kpi.fleet_health_ok'),
        'delta-type': critDisks || warnDisks ? 'warn' : null,
      } },
      { key: 'resources', className: 'clickable', attrs: {
        id: 'nas-fleet-res', label: T('kpi.fleet_resources'), value: loaded ? String(shares.length) : '—', icon: 'share',
        delta: protoCounts || null,
      } },
      { key: 'nodes', attrs: {
        label: T('kpi.nodes'), value: String(ready.length), suffix: T('kpi.fleet_nodes_suffix', { total: nodes.length }), icon: 'network',
        delta: unarmed.length ? T('kpi.node_unarmed', { node: nodeLabel(unarmed[0]) }) : null,
        'delta-type': unarmed.length ? 'warn' : null,
      } },
    ]);
    if (built) {
      // Wired once, so both read the fleet at CLICK time — a later poll must
      // not leave a tile pointing at a node that has since changed.
      kpi.querySelector('[data-kpi="health"]').addEventListener('click', () => {
        // The failed node first: the tile that shows a failure must open the
        // node that HAS it, not whichever node merely warns.
        const target = this.nodes.find((n) => n.disksCritical > 0)
          || this.nodes.find((n) => n.disksWarning > 0)
          || this.nodes.find((n) => n.instanceStatus === 'ready');
        if (target) this.selectNode(target.nodeId, 'disks', { diskFilter: 'problems' });
      });
      kpi.querySelector('[data-kpi="resources"]').addEventListener('click', () => {
        const target = this.fleetShares()[0]?.node || this.nodes.find((n) => n.instanceStatus === 'ready');
        if (target) this.selectNode(target.nodeId, 'shares');
      });
    }

    // One card is rebuilt only when ITS node's markup changed; a node that
    // joins or leaves the fleet adds or removes exactly its own card. The
    // click handler is delegated on the grid itself (see drawFleet), so a
    // rebuilt card needs no re-wiring.
    patchKeyedList(grid, nodes.map((n) => ({ key: n.nodeId, html: this.nodeCardHtml(n) })));

    const count = root.querySelector('#nas-fleet-alerts-count');
    setAttr(count, 'label', String(alertRows.length));
    setAttr(count, 'status', alertRows.length ? 'err' : 'neutral');
    setAttr(root.querySelector('#nas-fleet-alerts'), 'empty-message', loaded ? T('fleet.alerts_none') : I18n.t('common.loading'));
    setAttr(root.querySelector('#nas-fleet-res-table'), 'empty-message', loaded ? T('fleet.resources_none') : I18n.t('common.loading'));

    const first = ready[0] || {};
    const counts = nodeTabCounts(first, this.fleet?.rows);
    setAttr(root.querySelector('#nas-tabs tf-tab#disks'), 'count', String(Number(first.disksTotal) || 0));
    const poolsTab = root.querySelector('#nas-tabs tf-tab#pools');
    setAttr(poolsTab, 'count', counts.pools);
    setAttr(poolsTab, 'title', counts.poolsTitle);
    const sharesTab = root.querySelector('#nas-tabs tf-tab#shares');
    setAttr(sharesTab, 'count', counts.shares);
    setAttr(sharesTab, 'title', counts.sharesTitle);

    this.paintFleetAlerts();
    this.paintFleetResources();
  },

  async refreshFleet() {
    await this.loadNodes();
    await this.loadFleetData();
    if (this.disposed || this.nodeId) return;
    // A poll patches; it never redraws. Only a screen that is not there yet
    // gets built ("nigdy pełne odświeżenie całości").
    if (!this.root.querySelector('#nas-node-grid')) { this.drawFleet(); return; }
    this.paintFleet();
    this.later(() => this.refreshFleet(), POLL_FLEET_MS);
  },

  // A node id -> the fleet's name for it ('' when none): how a node's own
  // text that carries a node id names it instead (`scrubIds`, machine-id.js).
  nodeNameOf() {
    const names = new Map((this.nodes || []).map((n) => [String(n.nodeId || '').toLowerCase(), String(n.nodeName || '').trim()]));
    return (id) => names.get(String(id || '').toLowerCase()) || '';
  },

  // One row per active alert plus one row per node that did not answer.
  fleetAlertRows() {
    return (this.fleet?.rows || []).flatMap((r) => (typeof r.alerts === 'string'
      ? [{ node: r.node, error: r.alerts }]
      : r.alerts.map((a) => ({ node: r.node, alert: a }))));
  },

  // The level column names an ALERT severity, not a disk/pool health grade —
  // `fleet.severity_*` keeps its own lowercase vocabulary (n01) so the disk
  // wording ("Uwaga"/"Awaria", n03/n04) can never leak into an info alert.
  paintFleetAlerts() {
    const table = this.root.querySelector('#nas-fleet-alerts');
    if (!table) return;
    // Only when the rows really changed: `rows =` rebuilds every cell, and
    // with it any node text a reader has just opened. The node is named, and
    // its id is not even the tooltip (owner's rule: no ids in the GUI).
    const nameOf = this.nodeNameOf();
    setRowsIfChanged(table, this.fleetAlertRows().map((r) => (r.error ? {
      _row: r,
      level: `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('fleet.node_offline'))}"></tf-chip>`,
      node: `<span class="mono">${escapeHtml(nodeLabel(r.node))}</span>`,
      alert: escapeHtml(T('fleet.node_unreachable', { error: scrubIds(r.error, T('alerts.id_hidden'), nameOf) })),
      since: '—',
    } : {
      _row: r,
      level: `<tf-chip size="sm" status="${r.alert.severity === 'critical' ? 'err' : r.alert.severity === 'warning' ? 'warn' : 'info'}" dot label="${escapeAttr(T('fleet.severity_' + (['critical', 'warning'].includes(r.alert.severity) ? r.alert.severity : 'info')))}"></tf-chip>`,
      node: `<span class="mono">${escapeHtml(nodeLabel(r.node))}</span>`,
      alert: fleetAlertCellHtml(r.alert, nameOf),
      since: fmtAgo(r.alert.raisedAt),
    })));
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
        resource: `<span class="mono">${escapeHtml(nodeLabel(r.node))}</span>`,
        protocol: `<tf-chip size="sm" status="warn" dot label="${escapeAttr(T('fleet.node_offline'))}"></tf-chip>`,
        source: escapeHtml(T('fleet.node_unreachable', { error: r.shares })),
        mounts: '—',
        // Unknown, not zero: the node did not answer, so nobody counted.
        sessions: '—',
      })),
    ];
  },

  // n01 node card: health dot + identity + status chip, the fill bar, four
  // key/value stats and a foot that pairs the permission channel with what
  // the node does for the fleet.
  nodeCardHtml(n) {
    const unsupported = n.instanceStatus !== 'ready';
    const cls = ['node-card', unsupported ? 'unsupported' : '', !n.online ? 'offline' : ''].filter(Boolean).join(' ');
    const usedPct = pct(n.usedBytes, n.capacityBytes);
    // Shares and arrays belong to organisations; a remote row does not carry
    // them (see `perOrgCounted`), so they read "—" with the reason, and the
    // capacity says it is the pools' alone.
    const counted = perOrgCounted(n);
    const shares = nodeShares(n, this.fleet?.rows);
    const health = nodeCardHealth(n, shares);
    const statusChip = unsupported
      ? `<tf-chip status="warn" label="${escapeAttr(T('instance.' + n.instanceStatus))}"></tf-chip>`
      : !n.online
        ? `<tf-chip status="info" label="${escapeAttr(T('offline'))}"></tf-chip>`
        : `<tf-chip status="${healthChip(health).status}" dot label="${escapeAttr(healthChip(health).label)}"></tf-chip>`;
    // n01:197 — "CachyOS · OpenZFS 2.3.1 · 128 GB RAM · uptime 41 dni". RAM
    // and uptime come from the node's own summary row, so a node that has not
    // published one yet simply drops them instead of showing zeros.
    const sub = [
      n.osName || '—',
      n.zfsVersion ? T('node.badge_zfs', { v: n.zfsVersion }) : null,
      n.ramBytes ? T('node.badge_ram', { v: fmtBytes(n.ramBytes) }) : null,
      n.uptimeSecs ? T('uptime', { d: fmtDuration(n.uptimeSecs) }) : null,
      n.isLocal ? T('this_node') : null,
    ].filter(Boolean).join(' · ');
    // A node whose only storage is an Elastic Array serves the fleet exactly
    // as a node with a ZFS pool does; reading `poolsTotal` alone called it a
    // client.
    // A remote row's zero arrays is "not counted", so with no ZFS pool either
    // the card names what it does not know instead of calling it a client.
    const role = unsupported ? T('fleet.role_unsupported')
      : n.poolsTotal + (counted ? n.arraysTotal : 0) ? T('fleet.role_nas')
        : counted ? T('fleet.role_client') : T('fleet.role_uncounted');
    const kv = (k, v) => `<span class="kv-inline"><span class="k">${escapeHtml(k)}</span><span class="v">${v}</span></span>`;
    return `
      <div class="${cls}" data-node="${escapeAttr(n.nodeId)}">
        <div class="nc-head">
          <span class="health-dot ${healthClass(unsupported || !n.online ? 'unknown' : health)}"></span>
          <div style="flex:1;min-width:0">
            <div class="nc-name">${escapeHtml(nodeLabel(n))}</div>
            <div class="nc-sub">${escapeHtml(sub)}</div>
          </div>
          ${statusChip}
        </div>
        <div class="split-bar" title="${escapeAttr([usedPct + '%', n.arraysUnmeasured ? T('kpi.capacity_unmeasured', { n: n.arraysUnmeasured }) : null, counted ? null : T('fleet.pools_only')].filter(Boolean).join(' · '))}"><span class="${usedPct > 90 ? 'err' : usedPct > 75 ? 'warn' : ''}" style="width:${usedPct}%"></span></div>
        <div class="nc-stats">
          ${kv(T('kpi.capacity_total'), `${escapeHtml(fmtBytes(n.usedBytes))} / ${escapeHtml(fmtBytes(n.capacityBytes))}${counted ? '' : ` <span class="text-3" data-pools-only title="${escapeAttr(T('fleet.arrays_not_counted_hint'))}">${escapeHtml(T('fleet.pools_only'))}</span>`}`)}
          ${kv(T('kpi.disks'), `${n.disksTotal}${n.disksCritical ? ` · <span class="num-err">${n.disksCritical}!</span>` : ''}${n.disksWarning ? ` · <span class="num-warn">${n.disksWarning}!</span>` : ''}`)}
          ${kv(T('kpi.pools'), String(n.poolsTotal))}
          ${!counted ? kv('Elastic Array', notCountedHtml(T('fleet.arrays_not_counted_hint'))) : n.arraysTotal ? kv('Elastic Array', String(n.arraysTotal)) : ''}
          ${kv(T('kpi.shares'), shares ? String(shares.total) : notCountedHtml(T('fleet.not_counted_hint')))}
        </div>
        <div class="nc-foot">
          <tf-chip size="sm" status="${channelMode(n.elevationMode) === 'unarmed' ? 'warn' : 'ok'}" icon="${channelMode(n.elevationMode) === 'unarmed' ? 'lock' : 'shield'}" label="${escapeAttr(T('elevation.short_' + channelMode(n.elevationMode)))}"></tf-chip>
          <span>${escapeHtml(role)}</span>
        </div>
      </div>`;
  },

  // ---------------------------------------------------------------------------
  // Node view (n02 header + tabs)
  // ---------------------------------------------------------------------------

  async drawNode() {
    const node = this.currentNode();
    this.root.innerHTML = `
      <tf-breadcrumb class="nas-crumbs" id="nas-crumbs"></tf-breadcrumb>
      <div class="tf-detail-header">
        <div class="big-ico">${sprite('cylinder')}</div>
        <div class="d-meta">
          <div class="d-name">${escapeHtml(T('title'))} <span id="nas-head-chips"></span></div>
          <div class="d-sub" id="nas-head-sub">${escapeHtml(nodeHeadSub(node))}</div>
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

    this.setCrumbTail([]);
    // Delegated once and resolved at CLICK time: a detail view rewrites the
    // items of this bar (setCrumbTail), so a map captured here would go stale.
    const bar = this.root.querySelector('#nas-crumbs');
    bar.addEventListener('click', (e) => {
      const link = e.target.closest('a.tf-breadcrumb-item');
      if (!link) return;
      e.preventDefault();
      const acts = [...bar.querySelectorAll('tf-breadcrumb-item')].filter((i) => !i.hasAttribute('current')).map((i) => i.dataset.crumb || '');
      this.crumbAction(acts[[...bar.querySelectorAll('a.tf-breadcrumb-item')].indexOf(link)]);
    });
    const sel = this.root.querySelector('#nas-node-select');
    sel.setOptions(this.nodes.map((n) => ({
      value: n.nodeId,
      label: nodeLabel(n) + (n.isLocal ? ` (${T('this_node')})` : '') + (n.instanceStatus !== 'ready' ? ` — ${T('instance.' + n.instanceStatus)}` : ''),
      disabled: n.instanceStatus !== 'ready',
    })), this.nodeId);
    sel.addEventListener('change', (e) => { if (e.detail.value !== this.nodeId) this.selectNode(e.detail.value); });
    this.root.querySelector('[data-act="reprobe"]').addEventListener('click', () => this.reprobe());
    this.root.querySelector('[data-act="export-config"]').addEventListener('click', () => exportConfig(this));
    this.wireTabs(this.root.querySelector('#nas-tabs'), node);

    // Awaited: the tab body below decides whether this node may show a
    // dashboard at all, and that decision needs the channel state. On a fresh
    // install `cached_or_probe` has no cache and runs a full live probe, so
    // the body says what it is waiting for instead of sitting empty under a
    // header that has already painted.
    this.drawProbePending(this.root.querySelector('#nas-tab-body'));
    await this.refreshHeader();
    if (this.disposed || !this.root.isConnected) return;
    this.refreshJobsBadge();
    this.drawTab();
  },

  // The ONE breadcrumb of the node view (m26, n04:125-134). It used to say
  // "TentaNas › node" while the disk and pool details stacked a second bar of
  // their own under it ("Dyski › sdd", "Pule › tank"); the mockup has a single
  // "TentaNas › helios › Pule › tank". A detail view passes its tail here —
  // `{ label, act, query }` for a level that links back, `{ label }` for the
  // current one — and the node becomes a link as soon as there is a tail.
  // Rewritten only when the items change, and only on navigation.
  setCrumbTail(tail = []) {
    const bar = this.root.querySelector('#nas-crumbs');
    if (!bar) return;
    const node = this.currentNode();
    const items = tail.length
      ? [{ label: T('title'), act: 'fleet' }, { label: nodeLabel(node), act: 'node', query: `node=${this.nodeId}` }, ...tail]
      : [{ label: T('title'), act: 'fleet' }, { label: nodeLabel(node) }];
    const html = items.map((it) => (it.act
      ? `<tf-breadcrumb-item href="${escapeAttr('#/tentanas' + (it.query ? '?' + it.query : ''))}" data-crumb="${escapeAttr(it.act)}">${escapeHtml(it.label)}</tf-breadcrumb-item>`
      : `<tf-breadcrumb-item current>${escapeHtml(it.label)}</tf-breadcrumb-item>`)).join('');
    if (bar.__tfCrumbs === html) return;
    bar.__tfCrumbs = html;
    bar.querySelectorAll('tf-breadcrumb-item').forEach((i) => i.remove());
    // In front of the component's own <nav>, in document order.
    const tpl = document.createElement('template');
    tpl.innerHTML = html;
    bar.insertBefore(tpl.content, bar.firstChild);
    // The component re-renders from a MutationObserver, i.e. a tick later;
    // rendering now keeps the bar and its items in step for the click map.
    if (bar._nav && typeof bar._render === 'function') bar._render();
  },

  crumbAction(act) {
    if (act === 'fleet') { this.nodeId = null; this.diskId = null; this.draw(); return; }
    if (act === 'node') { this.switchTab('overview'); return; }
    if (act === 'disks' || act === 'pools' || act === 'shares') this.switchTab(act);
  },

  async refreshJobsBadge() {
    const res = await this.nas('tentaNasJobsListRequest', { limit: 100 }).catch(() => null);
    if (this.disposed || !res) return;
    this.setJobsBadge(res.jobs || []);
  },

  async refreshHeader(refresh = false) {
    try {
      // The services chip reads what is RUNNING (the shares list's own
      // service rows), not which packages the probe found installed. That
      // read is secondary: its failure costs the chip its verdict, never the
      // header.
      const [res, sharesRes] = await Promise.all([
        this.nas('tentaNasEnvironmentRequest', { refresh }),
        this.nas('tentaNasSharesListRequest', {}).catch(() => null),
      ]);
      if (this.disposed) return;
      // Recorded before the header guard: the tab body's gate reads these two
      // and must not depend on whether the header happened to still be on
      // screen when the probe came back.
      this.environment = res.environment;
      this.environmentError = null;
      if (!this.root.querySelector('#nas-head-badges')) return;
      const env = res.environment;
      const node = this.currentNode();
      const zfs = (env.features || []).find((f) => f.id === 'zfs');
      this.root.querySelector('#nas-head-chips').innerHTML = [
        // A failed disk is a failure on this chip too: reading `disksWarning`
        // alone left a node that had lost a disk wearing an amber "warning".
        `<tf-chip status="${node.disksCritical ? 'err' : node.disksWarning ? 'warn' : 'ok'}" dot label="${escapeAttr(node.disksCritical
          ? `${node.disksCritical} ${T('kpi.failures_suffix', { n: node.disksCritical })}`
          : node.disksWarning ? T('node.chip_disks_warn', { n: node.disksWarning }) : T('node.chip_ok'))}"></tf-chip>`,
        servicesChipHtml(servicesVerdict([{ node, answer: sharesRes }], { named: false })),
      ].join('');
      const badges = [
        zfs && zfs.version ? `<tf-chip status="accent" label="${escapeAttr(T('node.badge_zfs', { v: zfs.version }))}"></tf-chip>` : `<tf-chip status="warn" label="${escapeAttr(T('env.no_zfs'))}"></tf-chip>`,
        `<tf-chip status="${channelMode(env.elevation.mode) === 'unarmed' ? 'warn' : 'ok'}" icon="${channelMode(env.elevation.mode) === 'unarmed' ? 'lock' : 'shield'}" label="${escapeAttr(T('node.badge_channel', { mode: T('elevation.short_' + channelMode(env.elevation.mode)) }))}"></tf-chip>`,
        `<tf-chip status="info" icon="network" label="${escapeAttr(T('fleet.badge_mesh', { n: this.nodes.length }))}"></tf-chip>`,
      ];
      this.root.querySelector('#nas-head-badges').innerHTML = badges.join('');
      const sub = [
        nodeHeadSub(node),
        T('uptime', { d: fmtDuration(env.uptimeSecs) }),
        env.elevation.coreVersion ? T('fleet.head_version', { v: env.elevation.coreVersion }) : null,
        T('refreshed', { t: fmtAgo(env.probedAt) }),
      ];
      this.root.querySelector('#nas-head-sub').textContent = sub.filter(Boolean).join(' · ');
      // The node's own share list, answered for this organisation, is the
      // count its Shares tab has; a remote row alone has none (`nodeShares`).
      if (Array.isArray(sharesRes?.shares)) {
        const tab = this.root.querySelector('#nas-tabs tf-tab#shares');
        setAttr(tab, 'count', String(sharesRes.shares.length));
        setAttr(tab, 'title', null);
      }
    } catch (e) {
      if (this.disposed) return;
      // A FAILED probe is a fact the tab body has to state. This used to only
      // toast: `environment` stayed undefined, `channelUnusable()` answered
      // false, and the full dashboard rendered anyway — every tile then
      // failing separately against the same node that had just gone silent.
      this.environmentError = errMessage(e);
      toast(T('env.failed', { error: this.environmentError }), 'error');
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
    // The one home for the rule, and the order is the rule. A probe that
    // FAILED is an error state with a retry, never a dashboard that would
    // fail tile by tile. A probe that has not answered yet is "not known
    // yet", not "broken". Only then does the channel state decide: a node
    // that cannot run a privileged command opens on the setup step, whichever
    // tab was asked for, because every tab below would otherwise render the
    // same refusal in its own shape.
    if (this.environmentError) return this.drawProbeFailed(body);
    if (!this.environment) return this.drawProbePending(body);
    if (this.forceSetup || this.channelUnusable()) return this.drawSetupStep(body);
    // The disk, ZFS pool, Elastic Array and block target details write their own tail into
    // the shell's breadcrumb, once, as they open; every other view is the
    // node's top level. Clearing it first for a detail rebuilt the bar twice
    // on every draw of that detail.
    if (!(this.tab === 'disks' && this.diskId) && !(this.tab === 'pools' && (this.pool || this.array))
      && !(this.tab === 'shares' && this.targetId)) this.setCrumbTail([]);
    switch (this.tab) {
      case 'disks': return this.diskId ? this.drawDiskDetail(body) : this.drawDisks(body);
      case 'pools': return this.array ? drawElasticDetail(this, body) : this.pool ? drawPoolDetail(this, body) : drawPools(this, body);
      case 'shares': return this.targetId ? openTargetDetail(this, this.targetId, { body }) : drawShares(this, body);
      case 'jobs': return drawTasks(this, body);
      case 'environment': return this.drawEnvironment(body);
      default: return this.drawOverview(body);
    }
  },

  // ---------------------------------------------------------------------------
  // Forced setup step (n16): installation carries no secret, so this is where
  // the node learns how to become root
  // ---------------------------------------------------------------------------

  // Whether the panel may show a dashboard at all. A channel that was never
  // configured, a helper that is not "ok", or a helper whose build the core's
  // catalog does not match all refuse every privileged command — a dashboard
  // over that is a grid of tiles reporting the same refusal.
  //
  // A node whose environment is not known YET is not unusable: that is its own
  // state, decided in `drawTab` before this is ever asked.
  channelUnusable() {
    const el = this.environment?.elevation;
    if (!el) return false;
    if (channelMode(el.mode) === 'unarmed') return true;
    if (el.mode === 'helper') return el.helperState !== 'ok' || el.coreCompatible === false;
    return false;
  },

  // The probe has not answered yet. On a fresh install it has no cache and
  // runs live, so this is the first thing the admin sees on the very node this
  // feature exists for.
  drawProbePending(body) {
    if (!body) return;
    body.innerHTML = `
      <div class="stack" id="nas-probe-pending">
        <div class="section-card"><div class="muted">${escapeHtml(T('setup.probing'))}</div></div>
      </div>`;
  },

  // The probe FAILED. Deliberately not a dashboard: every tile would ask the
  // same silent node and fail on its own, which is the symptom this change
  // set out to remove.
  drawProbeFailed(body) {
    body.innerHTML = `
      <div class="stack" id="nas-probe-failed">
        <tf-alert tone="danger" title="${escapeAttr(T('setup.probe_failed_title'))}" message="${escapeAttr(T('setup.probe_failed_msg', { error: this.environmentError }))}"></tf-alert>
        <div class="section-card">
          <p class="wizard-section-sub">${escapeHtml(T('setup.probe_failed_hint'))}</p>
          <tf-button variant="primary" icon="refresh" data-act="probe-retry">${escapeHtml(T('setup.probe_retry'))}</tf-button>
        </div>
      </div>`;
    body.querySelector('[data-act="probe-retry"]')?.addEventListener('click', async () => {
      this.drawProbePending(body);
      await this.refreshHeader(true);
      if (!this.disposed) this.drawTab();
    });
  },

  // States what is missing, shows the commands mode A would run verbatim (n16
  // promises exactly that), and offers both modes through the existing wizard
  // — there is no second password dialog. Cancelling returns here, never to a
  // half-rendered dashboard, because the channel is still not configured.
  async drawSetupStep(body) {
    const el = this.environment?.elevation || {};
    const admin = this.isAdmin;
    const node = this.currentNode();
    // Forced straight after an install, the channel may already be fine (a
    // reinstall, or a node someone configured earlier). Saying "the helper is
    // from a different version" there would be a lie, so the working case gets
    // its own sentence and a calmer tone.
    const unusable = this.channelUnusable();
    const missing = !unusable
      ? T('setup.configured_already', { mode: T('elevation.short_' + channelMode(el.mode)) })
      : channelMode(el.mode) === 'unarmed'
        ? T('setup.missing_unset')
        : el.helperState === 'ok'
          ? T('setup.missing_incompatible')
          : T('setup.missing_helper', { state: T('elevation.helper_' + el.helperState) });

    body.innerHTML = `
      <div class="stack" id="nas-setup">
        <tf-alert tone="${unusable ? 'warning' : 'info'}" title="${escapeAttr(this.forceSetup ? T('setup.post_install_title') : T('setup.title'))}" message="${escapeAttr(missing)}"></tf-alert>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('key')} ${escapeHtml(T('elevation.title'))}</div><span class="hint">${escapeHtml(T('elevation.hint'))}</span></div>
          <p class="wizard-section-sub" id="nas-setup-scope">${escapeHtml(nodeT('setup.node_scope', node))}</p>
          <p class="wizard-section-sub">${escapeHtml(T('setup.lead'))}</p>
          ${admin ? `
          <div class="grid-2 mt-md">
            <div class="explain-box" id="nas-setup-mode-a">
              <p><b>${escapeHtml(T('setup.mode_a_title'))}</b></p>
              <p>${escapeHtml(T('setup.mode_a_desc'))}</p>
              <p class="wizard-section-sub">${escapeHtml(T('setup.plan_intro'))}</p>
              <pre class="cmd mono" id="nas-setup-plan">${escapeHtml(T('elevation.plan_loading'))}</pre>
              <div id="nas-setup-mode-a-blocked"></div>
              <tf-button variant="primary" icon="shield" data-act="setup-mode-a" disabled>${escapeHtml(T('setup.mode_a_button'))}</tf-button>
            </div>
            <div class="explain-box" id="nas-setup-mode-b">
              <p><b>${escapeHtml(T('setup.mode_b_title'))}</b></p>
              <p>${escapeHtml(T('setup.mode_b_desc'))}</p>
              <tf-button variant="secondary" icon="key" data-act="setup-mode-b">${escapeHtml(T('setup.mode_b_button'))}</tf-button>
            </div>
          </div>
          <div class="row mt-md">
            <tf-button variant="ghost" size="sm" icon="refresh" data-act="setup-recheck">${escapeHtml(T('setup.recheck'))}</tf-button>
            ${this.forceSetup ? `<tf-button variant="ghost" size="sm" data-act="setup-dismiss">${escapeHtml(T('setup.dismiss'))}</tf-button>` : ''}
          </div>`
          : `
          <div id="nas-setup-viewer" class="mt-md">
            ${warningHtml('info', T('setup.viewer_msg'))}
            <div class="muted mt-md">${escapeHtml(T('elevation.admin_only'))}</div>
          </div>`}
        </div>
      </div>`;

    body.querySelector('[data-act="setup-mode-a"]')?.addEventListener('click', () => this.openChannelWizard('helper'));
    body.querySelector('[data-act="setup-mode-b"]')?.addEventListener('click', () => this.openChannelWizard('interactive'));
    body.querySelector('[data-act="setup-recheck"]')?.addEventListener('click', async () => {
      await this.refreshHeader(true);
      if (!this.disposed) this.drawTab();
    });
    // Dismissing is allowed — mode B is a deliberate downgrade and nobody is
    // trapped here. It drops only the post-install forcing: a node with no
    // channel still fails `channelUnusable()` and is held on this step, so it
    // goes on reading as not configured exactly as before.
    body.querySelector('[data-act="setup-dismiss"]')?.addEventListener('click', () => {
      this.forceSetup = false;
      if (!this.disposed) this.drawTab();
    });
    // A viewer can do nothing here and must not be shown a plan box that will
    // never fill: the plan request below is admin-only, so for anyone else the
    // box used to sit on "Pobieranie planu…" forever under a heading that
    // promised the commands.
    if (!admin) return;

    // Mode A copies a helper binary that ships next to the core. Measured on a
    // real install: it was not there (`helperSourcePresent: false`), and
    // provisioning would have died on its first command — so the step says
    // that instead of offering a button that cannot work.
    let plan = null;
    try {
      plan = (await this.nas('tentaNasElevationPlanRequest', {})).plan;
    } catch (e) {
      const host = body.querySelector('#nas-setup-plan');
      if (host && host.isConnected) host.textContent = T('setup.plan_failed', { error: errMessage(e) });
      return;
    }
    if (this.disposed || !body.isConnected) return;
    const pre = body.querySelector('#nas-setup-plan');
    if (pre) pre.textContent = (plan.commands || []).map((c) => c.join(' ')).join('\n');
    const modeA = body.querySelector('[data-act="setup-mode-a"]');
    if (plan.helperSourcePresent) {
      modeA?.removeAttribute('disabled');
    } else {
      modeA?.remove();
      const blocked = body.querySelector('#nas-setup-mode-a-blocked');
      if (blocked) blocked.innerHTML = warningHtml('danger', T('setup.helper_missing', { path: plan.helperSource }));
    }
  },

  // ---------------------------------------------------------------------------
  // Overview tab (n02)
  // ---------------------------------------------------------------------------

  async drawOverview(body) {
    body.innerHTML = `
      <div class="stack">
        <div id="nas-ov-error"></div>
        <div class="kpi" id="nas-ov-kpi"></div>
        <div id="nas-ov-telemetry"></div>
        <div class="grid-2" id="nas-ov-arc-row" data-single="1">
          <div class="section-card">
            <div class="section-card-head">
              <div class="title">${sprite('cpu')} ${escapeHtml(T('arc.title'))}</div>
              <div class="actions" id="nas-ov-arc-actions"></div>
            </div>
            <div id="nas-ov-arc"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
          </div>
          <!-- n02's "Tiering i cache zapisu", beside ARC as the mockup draws
               it. Hidden, and the row collapses to one column, on a node with
               no cache tier to describe: a permanently empty card next to a
               populated one reads as a broken panel. -->
          <div class="section-card" id="nas-ov-tier-card" hidden>
            <div class="section-card-head">
              <div class="title">${sprite('zap')} ${escapeHtml(T('tiering.title'))}</div>
            </div>
            <div id="nas-ov-tier"></div>
          </div>
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
    // Delegated ONCE on the container, exactly like the n15 Tasks tab's own
    // running-jobs list (tasks.js): `patchKeyedList` keeps a row's Cancel/Log
    // buttons as the very node a poll found them at, so re-attaching a
    // listener to them on every poll would either stack a second handler on
    // a survivor or silently do nothing for a row that moved.
    body.querySelector('#nas-ov-jobs').addEventListener('click', (e) => {
      const row = e.target.closest('.job-row');
      if (!row) return;
      const jobId = row.dataset.job;
      if (e.target.closest('[data-act="log"]')) { this.openJobLog(jobId); return; }
      if (e.target.closest('[data-act="cancel"]')) { e.stopPropagation(); this.cancelOverviewJob(jobId, body); }
    });
    body.querySelector('[data-act="create-pool"]').addEventListener('click', () => {
      const nodeId = this.currentNode()?.nodeId;
      const surface = body.querySelector('#nas-ov-pools');
      const isCurrent = () => !this.disposed && surface?.isConnected && this.currentNode()?.nodeId === nodeId;
      openPoolWizard(this, { freeDisks: this.overviewFreeDisks || [], pools: this.overviewPools || [], isCurrent,
        onDone: () => { if (isCurrent()) this.refreshOverview(body); },
        onCreated: ({ name, outcome }) => {
          if (!isCurrent()) return;
          if (outcome === 'approval') this.switchTab('jobs');
          else this.openArray(outcome === 'job' ? name : null);
        },
      });
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
      // Swatches once, numbers as text: the readout under the chart must not
      // be torn down and rebuilt on every sample either.
      const val = body.querySelector('#nas-ov-io-val');
      patchHtml(val, '<span class="sw primary"></span><span class="v-read"></span><span class="sw info"></span><span class="v-write"></span>');
      setText(val.querySelector('.v-read'), `${T('disk.legend_read')} ${fmtMBps(read)} MB/s  `);
      setText(val.querySelector('.v-write'), `${T('disk.legend_write')} ${fmtMBps(write)} MB/s`);
    }
    const temp = body.querySelector('#nas-ov-temp');
    if (temp) {
      const temps = disks.map((d) => d.temperatureC).filter((t) => t != null);
      const avg = temps.length ? temps.reduce((a, t) => a + t, 0) / temps.length : null;
      const sample = {};
      if (maxTemp != null) sample.max = maxTemp;
      if (avg != null) sample.avg = avg;
      temp.push(now, sample);
      const tval = body.querySelector('#nas-ov-temp-val');
      if (maxTemp == null) {
        patchHtml(tval, '<span class="v-max"></span>');
        setText(tval.querySelector('.v-max'), T('overview.temp_none'));
      } else {
        patchHtml(tval, '<span class="sw warning"></span><span class="v-max"></span><span class="sw info"></span><span class="v-avg"></span>');
        setText(tval.querySelector('.v-max'), `${T('overview.temp_max')} ${maxTemp}°C${hottest ? ` (${hottest})` : ''}  `);
        setText(tval.querySelector('.v-avg'), `${T('overview.temp_avg')} ${Math.round(avg)}°C`);
      }
    }
  },

  // A tab switch drops the subject of the tab it leaves; `disk` carries one
  // in (the alert drill-down opens the disk its alert is about).
  switchTab(tab, { disk = null, target = null } = {}) {
    this.tab = tab;
    this.diskId = disk;
    // Which block target the Sharing tab should put the admin on. Set by the
    // drift alert's own button, cleared by every other navigation, so a stale
    // name cannot follow the admin around the tabs.
    this.targetName = target;
    this.targetId = null;
    this.pool = null;
    this.array = null;
    this.dataset = null;
    this.clearTimers();
    this.setLocation();
    const tabs = this.root.querySelector('#nas-tabs');
    if (tabs) tabs.setAttribute('value', tab);
    this.drawTab();
  },

  openTarget(targetId) {
    this.targetId = targetId;
    this.targetName = null;
    this.tab = 'shares';
    this.clearTimers();
    this.setLocation();
    this.root.querySelector('#nas-tabs')?.setAttribute('value', 'shares');
    this.drawTab();
  },

  // Opens a pool inside the pools tab (from a card, an alert or a disk's
  // "member of" link); `poolTab` picks the inner tab, `dataset` focuses one
  // row of the datasets/snapshots tab.
  openPool(name, poolTab = 'topology', dataset = null) {
    this.array = null;
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

  openArray(name) {
    this.array = name || null;
    this.pool = null;
    this.dataset = null;
    this.diskId = null;
    this.targetId = null;
    this.tab = 'pools';
    this.clearTimers();
    this.setLocation();
    this.root.querySelector('#nas-tabs')?.setAttribute('value', 'pools');
    this.drawTab();
  },

  async refreshOverview(body) {
    let disksRes, jobsRes, alertsRes, poolsRes, arcRes, arraysRes;
    try {
      [disksRes, jobsRes, alertsRes, poolsRes, arcRes, arraysRes] = await Promise.all([
        this.nas('tentaNasDisksListRequest', {}),
        this.nas('tentaNasJobsListRequest', { limit: 20 }),
        this.nas('tentaNasAlertsListRequest', { includeAcked: false }),
        this.nas('tentaNasPoolsListRequest', {}),
        this.nas('tentaNasArcStatsRequest', {}).catch(() => ({ arc: null })),
        // An Elastic Array is a pool on this screen: the Pools tab lists both,
        // and a dashboard that showed only the ZFS half hid a whole pool, its
        // capacity and its protection state from the screen the admin lands
        // on. Degraded like ARC and for the same reason: a node without
        // Elastic support, or one whose array probe fails, must lose the array
        // rows and NOT the dashboard.
        this.nas('tentaNasElasticArraysListRequest', {}).catch(() => ({ arrays: [] })),
      ]);
    } catch (e) {
      if (this.disposed || !body.isConnected) return;
      // A failed poll is a transient fact about ONE request, not a reason to
      // throw the dashboard away: replacing the body destroyed the charts and
      // their accumulated history, and returning without re-arming the timer
      // stopped the overview refreshing for good — the tiles then sat on
      // whatever the last good poll left until the user changed tabs. Say
      // what failed in a banner above the dashboard, keep the dashboard, and
      // keep asking.
      patchHtml(body.querySelector('#nas-ov-error'),
        `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`);
      this.later(() => this.refreshOverview(body), POLL_OVERVIEW_MS);
      return;
    }
    if (this.disposed || !body.isConnected) return;
    // The node answered, so retract the banner the last failure raised.
    patchHtml(body.querySelector('#nas-ov-error'), '');

    const disks = disksRes.disks || [];
    // Split, because collapsing them is what made this screen lie: the disks
    // table and the fleet card both call nvme0n1 an "Awaria", while this tile
    // announced it as one of "6 ostrzeżeń". Two screens contradicting each
    // other about the same failing disk is worse than either wording alone.
    const critical = disks.filter((d) => d.health === 'critical');
    const warnings = disks.filter((d) => d.health === 'warning');
    // Failures first, so the delta line below names the worst disk first.
    const warned = critical.concat(warnings);
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
    // A malformed answer is not a reason to throw the dashboard away, but it is
    // also not an array list: anything that is not a list counts as none.
    const arrays = Array.isArray(arraysRes.arrays) ? arraysRes.arrays : [];
    this.overviewPools = pools;
    this.overviewArrays = arrays;
    this.overviewFreeDisks = poolsRes.freeDisks || [];
    // An array reports `usableBytes` / `usedBytes` in the same unit a pool
    // does, so the two belong in the same total — but only once the node has
    // actually measured them. `elasticCapacity().measured` is the single test
    // for that, and an unmeasured array contributes no bytes and is named
    // separately in the delta line instead of being silently read as zero.
    const measured = arrays.filter((a) => elasticCapacity(a).measured);
    const unmeasured = arrays.length - measured.length;
    // The total is what the pools and the MEASURED arrays offer, and nothing
    // else: the old fallback to the sum of raw disk sizes (system disk and
    // unused disks included) printed a capacity no storage on the node has.
    // With nothing measured the tile says "—" and why.
    const capKnown = pools.length + measured.length > 0;
    const cap = pools.reduce((a, p) => a + (Number(p.usableBytes) || 0), 0)
      + measured.reduce((a, x) => a + Number(x.usableBytes), 0);
    const used = pools.reduce((a, p) => a + (Number(p.usedBytes) || 0), 0)
      + measured.reduce((a, x) => a + Number(x.usedBytes), 0);
    this.setJobsBadge(jobs);

    const kpi = body.querySelector('#nas-ov-kpi');
    if (!kpi) return;
    // Read by the tile's click handler, which is wired once and must not
    // close over the disk list of whichever poll happened to build it.
    this.overviewWarned = warned.length;
    const built = paintStatCards(kpi, [
      { key: 'pools', className: 'clickable', attrs: {
        label: T('kpi.capacity_total'), value: capKnown ? fmtBytes(cap) : '—', icon: 'database',
        delta: [
          capKnown ? T('kpi.capacity_delta', { used: fmtBytes(used), pct: pct(used, cap), n: pools.length + arrays.length }) : null,
          !capKnown && !arrays.length ? T('kpi.capacity_no_pools') : null,
          unmeasured ? T('kpi.capacity_unmeasured', { n: unmeasured }) : null,
        ].filter(Boolean).join(' · '),
      } },
      { key: 'disks', className: 'clickable', attrs: {
        label: T('kpi.disk_health'), icon: 'cylinder',
        // The worst state leads. The rest stays reachable: the delta line below
        // names up to three problem disks with their reasons, and the tile is
        // clickable straight into the disks tab filtered to problems.
        value: String(critical.length || warnings.length),
        suffix: critical.length
          ? T('kpi.failures_suffix', { n: critical.length })
          : T('kpi.warnings_suffix', { n: warnings.length }),
        // `danger` is one of the FOUR accents tf-stat-card accepts
        // (success | danger | warning | info, tf-stat-card.js:10). Any other
        // word is silently dropped and the tile renders with no accent at all,
        // which is how a red state would quietly become an ordinary one.
        accent: critical.length ? 'danger' : warnings.length ? 'warning' : null,
        // Each disk's reason in the reader's language, from its codes
        // (`diskReasonsText`); the node's own sentences are the tooltip.
        delta: warned.length
          ? warned.slice(0, 3).map((d) => `${d.name}: ${diskReasonsText(d).text || T('health.' + d.health)}`).join(' · ')
          : T('kpi.disk_health_ok'),
        title: warned.length
          ? warned.slice(0, 3).filter((d) => d.healthReason).map((d) => `${d.name}: ${d.healthReason}`).join(' · ') || null
          : null,
        // Same trap one field down, and it had caught us: `delta-type` has its
        // OWN allowlist (up | down | warn | neutral, tf-stat-card.js:11) and
        // 'negative' is not in it, so the tile fell back to `neutral` — no ⚠,
        // no warn colour — on three of the eight KPI tiles.
        'delta-type': warned.length ? 'warn' : null,
      } },
      { key: 'iops', attrs: { label: T('kpi.iops'), value: String(iops), icon: 'trend', ...iopsBaseline(iops, disksRes.iopsHourAvg) } },
      { key: 'throughput', attrs: {
        label: T('kpi.throughput'), value: fmtMBps(read), suffix: T('kpi.throughput_suffix'), icon: 'zap',
        delta: T('kpi.throughput_delta', { w: fmtMBps(write), lat: latency.toFixed(1) }),
      } },
    ]);
    if (built) {
      kpi.querySelector('[data-kpi="pools"]').addEventListener('click', () => this.switchTab('pools'));
      kpi.querySelector('[data-kpi="disks"]').addEventListener('click', () => { this.diskFilter = this.overviewWarned ? 'problems' : 'all'; this.switchTab('disks'); });
    }

    this.pushOverviewSamples(body, disks, read, write, maxTemp, hottest);

    this.paintTelemetryAlert(body.querySelector('#nas-ov-telemetry'), disksRes.telemetry);

    this.paintArcCard(body, arcRes.arc);
    this.paintPoolsMini(body, pools, arrays);
    this.paintTiering(body, arrays);

    // tf-chip._update() empties its span and rebuilds it, so a raw setAttribute
    // with an unchanged value blinked this counter on every 5 s poll.
    const alertsCount = body.querySelector('#nas-ov-alerts-count');
    setAttr(alertsCount, 'label', String(alerts.length));
    setAttr(alertsCount, 'status', alerts.length ? 'err' : 'neutral');
    this.renderAlertList(body.querySelector('#nas-ov-alerts'), alerts, () => this.refreshOverview(body));

    // Keyed by jobId, exactly like the n15 Tasks tab's running-jobs list
    // (tasks.js), and painted with the very same `jobRowSkeleton`/
    // `paintJobRow`: a job whose progress or elapsed-time text moves keeps
    // its own row — Cancel button included — instead of the whole card being
    // rebuilt on every 5 s poll (BLOCKER 2, n01-n10 critic 2026-09-21).
    const jobsEl = body.querySelector('#nas-ov-jobs');
    patchKeyedList(jobsEl, running.map((j) => ({ key: j.jobId, html: jobRowSkeleton(j) })));
    running.forEach((j, i) => paintJobRow(jobsEl.children[i], j));

    this.later(() => this.refreshOverview(body), POLL_OVERVIEW_MS);
  },

  // n02 ARC card: a stable skeleton (donut + five stat rows), built once and
  // painted per poll with `setText`/`setAttr`/`patchHtml`-per-slot. Every
  // field here — `sizeBytes`, `hitRatio`, the MRU/MFU split — moves on almost
  // every 5 s poll, and rebuilding the card for that used to tear down the
  // donut and every row (BLOCKER 2, n01-n10 critic 2026-09-21).
  paintArcCard(body, arc) {
    const host = body.querySelector('#nas-ov-arc');
    const actions = body.querySelector('#nas-ov-arc-actions');
    if (!host || !actions) return;
    if (!arc) {
      patchHtml(host, `<div class="muted">${escapeHtml(T('arc.unavailable'))}</div>`);
      patchHtml(actions, '');
      host.__tfArcBuilt = false;
      actions.__tfArcBuilt = false;
      return;
    }
    if (!host.__tfArcBuilt) {
      host.__tfHtml = null;
      host.innerHTML = arcSkeletonHtml();
      host.__tfArcBuilt = true;
    }
    // Wired ONCE per host, on a flag of its own: `__tfArcBuilt` flips back to
    // false whenever ARC goes unavailable (above) so the skeleton is rebuilt
    // when it returns, but the delegated click listener lives on `host`
    // itself, which is never replaced — adding it again on every rebuild
    // stacked one more `click` handler per off/on cycle, so one l2arc click
    // called `openPool` once per cycle it had survived.
    if (!host.__tfArcWired) {
      host.__tfArcWired = true;
      host.addEventListener('click', (e) => {
        if (e.target.closest('[data-act="arc-l2arc"]')) this.openPool(this.overviewArcBiggestPool?.name);
      });
    }
    const ramPct = arc.ramBytes ? Math.round((Number(arc.maxBytes) || 0) / Number(arc.ramBytes) * 100) : 0;
    const mru = Number(arc.mruBytes) || 0;
    const mfu = Number(arc.mfuBytes) || 0;
    const demand = Number(arc.demandHits) || 0;
    const prefetch = Number(arc.prefetchHits) || 0;
    const l2 = (arc.l2arcPools || []);
    const biggest = (this.overviewPools || []).slice().sort((a, b) => (Number(b.sizeBytes) || 0) - (Number(a.sizeBytes) || 0))[0];
    this.overviewArcBiggestPool = biggest;
    const p = Math.max(0, Math.min(100, Number(arc.hitRatio) || 0));
    setAttr(host.querySelector('[data-role="donut"]'), 'style', `background: conic-gradient(var(--success) 0 ${p}%, var(--bg-3) ${p}% 100%);`);
    setText(host.querySelector('[data-role="val"]'), `${p.toFixed(1)}%`);
    setText(host.querySelector('[data-role="lbl"]'), T('arc.hit_ratio'));
    setText(host.querySelector('[data-role="usage"]'), `${fmtBytes(arc.sizeBytes)} / ${fmtBytes(arc.maxBytes)}`);
    setText(host.querySelector('[data-role="split"]'), `${pct(mru, mru + mfu)}% / ${pct(mfu, mru + mfu)}%`);
    setText(host.querySelector('[data-role="demand"]'), `${pct(demand, demand + prefetch)}% / ${pct(prefetch, demand + prefetch)}%`);
    patchHtml(host.querySelector('[data-role="slog"]'), (arc.slogPools || []).length
      ? `<span class="mono">${escapeHtml(arc.slogPools.join(', '))}</span>`
      : `<span class="text-3">${escapeHtml(T('arc.slog_none'))}</span>`);
    patchHtml(host.querySelector('[data-role="l2arc"]'), l2.length
      ? `<span class="mono">${escapeHtml(l2.join(', '))}</span>`
      : biggest
        ? `<span class="text-3">${escapeHtml(T('arc.l2arc_none'))} — <a data-act="arc-l2arc">${escapeHtml(T('arc.l2arc_add', { pool: biggest.name }))}</a></span>`
        : `<span class="text-3">${escapeHtml(T('arc.l2arc_none'))}</span>`);
    if (!actions.__tfArcBuilt) {
      actions.__tfHtml = null;
      actions.innerHTML = `<tf-button variant="ghost" size="sm" icon="settings" data-act="arc-limit"></tf-button>`;
      actions.__tfArcBuilt = true;
      actions.querySelector('[data-act="arc-limit"]').addEventListener('click', () => this.switchTab('environment'));
    }
    // `tf-button` is light-DOM: it owns an inner `<button>` it built itself,
    // and watches its own children for a caller overwriting them so it can
    // rebuild that `<button>` from scratch (see tf-button.js). `setText` here
    // used to write straight into the host's light DOM, which that observer
    // reads as exactly such an overwrite — so a ramPct% text change on every
    // ARC poll tore the button down and rebuilt it. `setAttr` on `label`
    // updates the same text through the attribute path the component
    // re-renders from in place, with no rebuild.
    setAttr(actions.querySelector('[data-act="arc-limit"]'), 'label', T('arc.change_limit', { pct: ramPct }));
  },

  // n02 pool mini-list: name + state, the one-line topology and the fill bar.
  // Both kinds of pool live here, ZFS first and then the Elastic Arrays, in
  // ONE patched string: an unchanged poll compares equal and writes nothing,
  // so every row on screen survives it as the same node.
  // n02's tiering card. Every figure here was already measured, already on
  // the wire and read by NOTHING: `cacheUnprotectedBytes` in particular is the
  // canonical "18 GiB na cache bez parity" the spec asks for in several
  // places and no screen has ever shown.
  //
  // What the mockup draws and this does NOT: a share spanning a fast ZFS
  // dataset and an archive pool, and a write-cache hit ratio. Neither exists
  // in the product — cross-pool tiering is a storage model, not a panel, and
  // nothing measures the hit ratio. Inventing either here would put a number
  // on the dashboard that no code stands behind.
  paintTiering(body, arrays = []) {
    const row = body.querySelector('#nas-ov-arc-row');
    const card = body.querySelector('#nas-ov-tier-card');
    const host = body.querySelector('#nas-ov-tier');
    if (!card || !host) return;
    // Only an array with a cache tier has tiering to describe.
    const tiered = arrays.filter((a) => a && Number(a.cacheSizeBytes) > 0);
    card.hidden = tiered.length === 0;
    setAttr(row, 'data-single', tiered.length ? null : '1');
    if (!tiered.length) {
      patchKeyedList(host, []);
      return;
    }
    patchKeyedList(host, tiered.map((a) => ({ key: a.name, html: this.tierBlockHtml(a) })));
  },

  tierBlockHtml(a) {
    // `Number(null)` is 0, not NaN, so an `Number.isFinite` guard alone turns
    // "not measured" into "zero used" — the one conflation this protocol has
    // its own test for. Absence is checked BEFORE the conversion.
    const num = (v) => (v == null || !Number.isFinite(Number(v)) ? null : Number(v));
    const cacheUsed = num(a.cacheUsedBytes);
    const dataUsed = num(a.usedBytes);
    // A bar drawn from a half-known split is worse than no bar: it looks like
    // a measurement. Both sides or neither, the rule this file already applies
    // to the capacity KPI.
    const bar = cacheUsed !== null && dataUsed !== null && cacheUsed + dataUsed > 0
      ? (() => {
        const total = cacheUsed + dataUsed;
        const cachePct = Math.round((cacheUsed / total) * 100);
        return `<div class="split-bar" title="${escapeAttr(T('tiering.bar_title', {
          cache: fmtBytes(cacheUsed), data: fmtBytes(dataUsed),
        }))}"><span style="width:${cachePct}%"></span><span class="warn" style="width:${100 - cachePct}%"></span></div>`;
      })()
      : '';
    const waiting = a.protection?.cacheUnprotectedBytes;
    // The recorded runs travel inside `mover`; the array has no top-level
    // history, and reading one here made every card say "no runs".
    const run = (a.mover?.history || [])[0] || null;
    const runText = run
      ? `${fmtAgo(run.startedAt)} · ${fmtBytes(Number(run.movedBytes) || 0)} · ${T('tiering.files', { n: Number(run.movedFiles) || 0 })}`
      : T('tiering.no_runs');
    const sr = (k, v, cls = '') => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v ${cls}">${escapeHtml(v)}</span></div>`;
    return `<div class="tier-block" data-array="${escapeAttr(a.name)}">
      <div class="text-3">${escapeHtml(T('tiering.subtitle', { name: a.name }))}</div>
      ${bar}
      <div class="stat-rows mt-sm">
        ${sr(T('tiering.cache_tier'), `${fmtOptionalBytes(a.cacheUsedBytes)} / ${fmtOptionalBytes(a.cacheSizeBytes)}`)}
        ${sr(T('tiering.data_tier'), `${fmtOptionalBytes(a.usedBytes)} / ${fmtOptionalBytes(a.usableBytes)}`)}
        ${sr(T('tiering.waiting'), fmtOptionalBytes(waiting))}
        ${sr(T('tiering.last_move'), runText)}
      </div>
      <div class="muted mt-sm">${escapeHtml(T('tiering.waiting_hint'))}</div>
    </div>`;
  },

  // n02 pool mini-list: name + state, the one-line topology and the fill bar.
  // Both kinds of pool live here, ZFS first and then the Elastic Arrays, keyed
  // by `pool:<name>`/`array:<name>` so a row keeps its identity (and its
  // click handler, wired ONCE via delegation below) across a poll even though
  // its description, fill bar and used/usable pair move on almost every one
  // (a live scrub percentage/ETA in particular) — BLOCKER 2, n01-n10 critic
  // 2026-09-21.
  paintPoolsMini(body, pools, arrays = []) {
    const host = body.querySelector('#nas-ov-pools');
    if (!host) return;
    if (!host.__tfPoolsWired) {
      host.__tfPoolsWired = true;
      host.addEventListener('click', (e) => {
        const row = e.target.closest('.pool-mini');
        if (!row) return;
        if (row.dataset.pool) this.openPool(row.dataset.pool);
        // `data-array`, so the click lands on the ARRAY detail (n11) and not
        // on a ZFS pool route that has no such pool to open.
        else if (row.dataset.array) this.openArray(row.dataset.array);
      });
    }
    if (!pools.length && !arrays.length) {
      patchHtml(host, `<div class="muted">${escapeHtml(T('pools.empty_title'))}</div>`);
      return;
    }
    const items = [
      ...pools.map((p) => ({ key: `pool:${p.name}`, html: poolMiniSkeleton(p), paint: (row) => paintPoolMini(row, p) })),
      ...arrays.map((a) => ({ key: `array:${a.name}`, html: arrayMiniSkeleton(a), paint: (row) => paintArrayMini(row, a) })),
    ];
    patchKeyedList(host, items.map(({ key, html }) => ({ key, html })));
    items.forEach((it, i) => it.paint(host.children[i]));
  },

  // What the disk-telemetry banner should say, or null when SMART is live and
  // there is nothing to say. Kept apart from the painting so a poll compares
  // STATES instead of comparing markup.
  telemetryAlertSpec(t) {
    // The wire says `pending` | `unarmed` | `ok` | `partial` (disks.rs:900,
    // :1104, :1142, :1145). This screen compared against 'live' and
    // 'stale_unarmed', which the server never emits, so the first test never
    // matched, the second was dead code, and every poll fell through to
    // "SMART niedostępny" — on a machine whose SMART was entirely healthy.
    if (!t) return null;
    // `ok` = every disk was read. `pending` = the first probe has not run yet,
    // so there is nothing to report and saying so would itself blink.
    if (t.smartState === 'ok' || t.smartState === 'pending') return null;
    if (t.smartState === 'unarmed') {
      return {
        kind: this.isAdmin ? 'stale:arm' : 'stale',
        tone: 'warning',
        title: T('telemetry.stale_title'),
        message: T('telemetry.stale_msg', { t: t.smartReadAt ? fmtAgo(t.smartReadAt) : T('never') }),
        action: this.isAdmin
          ? { variant: 'primary', icon: 'unlock', label: T('elevation.arm'), run: () => this.openChannelWizard() }
          : null,
      };
    }
    const missing = this.missingSmartFeature();
    return {
      kind: missing ? `unavailable:install:${missing.id}` : 'unavailable',
      tone: 'info',
      // `partial` means SMART read the rest of the disks and failed only on the
      // ones `detail` names — calling that "unavailable" overstates it, because
      // the tab is not blind, it is blind to part of the fleet. Any other value
      // is genuinely unknown to this screen and keeps the blunt title.
      // `kind` deliberately stays as it is: it drives the element rebuild, and
      // only the ACTION may force one. The title is written through setAttr,
      // which skips an unchanged value, so varying it costs no churn.
      title: T(t.smartState === 'partial' ? 'telemetry.partial_title' : 'telemetry.unavailable_title'),
      message: t.detail || '',
      // "Jak brakuje pakietu, to powinna być możliwość doinstalowania" — the
      // same route the Environment tab's per-feature button takes, so there is
      // one install path, not two.
      action: missing
        ? { variant: 'primary', icon: 'download', label: T('env.install_sudo'), run: () => this.installFeature(missing) }
        : null,
    };
  },

  // The SMART package, but only when the node's own probe reports it absent
  // AND the node has a package manager to install it with (n16). Anything
  // else — including SMART failing on part of the disks — is not a missing
  // package and gets no install button.
  missingSmartFeature() {
    if (!this.isAdmin || !this.environment?.packageManager) return null;
    return (this.environment.features || []).find((f) => f.id === SMART_FEATURE_ID
      && FEATURE_ABSENT.has(f.status)
      && (f.packages || []).length > 0) || null;
  },

  // The banner is ONE <tf-alert> that lives in `host`: an unchanged poll
  // writes nothing at all, and a changed one sets attributes on the element
  // already on screen. Rebuilding it every 5 s is what made it blink.
  paintTelemetryAlert(host, t) {
    if (!host) return;
    const spec = this.telemetryAlertSpec(t);
    if (!spec) {
      if (host.firstChild) { host.replaceChildren(); host.__tfAlertKind = null; }
      return;
    }
    let alert = host.firstElementChild;
    // tf-alert captures its slotted actions once, at build time, so only a
    // change of KIND (which decides the action) rebuilds the element.
    if (!alert || host.__tfAlertKind !== spec.kind) {
      host.__tfAlertKind = spec.kind;
      alert = document.createElement('tf-alert');
      if (spec.action) {
        const actions = document.createElement('div');
        actions.setAttribute('slot', 'actions');
        const btn = document.createElement('tf-button');
        btn.setAttribute('size', 'sm');
        btn.setAttribute('variant', spec.action.variant);
        btn.setAttribute('icon', spec.action.icon);
        btn.textContent = spec.action.label;
        btn.addEventListener('click', spec.action.run);
        actions.appendChild(btn);
        alert.appendChild(actions);
      }
      host.replaceChildren(alert);
    }
    setAttr(alert, 'tone', spec.tone);
    setAttr(alert, 'title', spec.title);
    setAttr(alert, 'message', spec.message);
  },

  // Every row ends with the same drill-down the fleet alert table offers
  // ("Szczegóły" → the disk, "Dyski", "Uzbrój" → the environment tab), with
  // "Potwierdź" staying the ghost action next to it (n02). Keyed by
  // `alertId`, with `fmtAgo(a.raisedAt)` written into its own element by
  // `paintAlertRow`: baking it into the row's markup — as the old single
  // `patchHtml` over the joined string did — rebuilt the whole list (Ack and
  // "Szczegóły" buttons included) about once a minute, for every alert still
  // inside its first minute (BLOCKER 2, n01-n10 critic 2026-09-21).
  renderAlertList(el, alerts, onChange) {
    if (!alerts.length) {
      patchHtml(el, `<div class="muted">${escapeHtml(T('alerts.none'))}</div>`);
      return;
    }
    if (!el.__tfAlertsWired) {
      el.__tfAlertsWired = true;
      el.addEventListener('click', async (e) => {
        const ackBtn = e.target.closest('[data-ack]');
        if (ackBtn) {
          try {
            await this.nas('tentaNasAlertAckRequest', { alertId: ackBtn.dataset.ack });
            el.__tfAlertsOnChange?.();
          } catch (err) {
            toast(errMessage(err), 'error');
          }
          return;
        }
        const gotoBtn = e.target.closest('[data-goto]');
        if (!gotoBtn) return;
        const a = el.__tfAlertsByKey?.get(gotoBtn.dataset.goto);
        if (!a) return;
        const target = alertTarget(a);
        if (target.extra.array) this.openArray(target.extra.array);
        else if (target.extra.pool) this.openPool(target.extra.pool);
        else this.switchTab(target.tab, { disk: target.extra.disk || null, target: target.extra.target || null });
      });
    }
    el.__tfAlertsOnChange = onChange;
    el.__tfAlertsByKey = new Map(alerts.map((a) => [String(a.alertId), a]));
    patchKeyedList(el, alerts.map((a) => ({ key: a.alertId, html: alertRowSkeleton(a) })));
    const nameOf = this.nodeNameOf();
    alerts.forEach((a, i) => paintAlertRow(el.children[i], a, nameOf));
  },

  // ---------------------------------------------------------------------------
  // Disks tab (n03)
  // ---------------------------------------------------------------------------

  async drawDisks(body) {
    this.diskSelection = this.diskSelection || new Set();
    body.innerHTML = `
      <div id="nas-disk-advice"></div>
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
          <tf-column key="rw" label="${escapeAttr(T('disks.col_rw'))}" renderer="text" nowrap hide-below="1024"></tf-column>
          <tf-column key="lat" label="${escapeAttr(T('disks.col_lat'))}" renderer="text" nowrap hide-below="1024"></tf-column>
          <tf-column key="wear" label="${escapeAttr(T('disks.col_wear'))}" renderer="html" nowrap hide-below="1180"></tf-column>
          <tf-column key="trend" label="${escapeAttr(T('disks.col_trend'))}" renderer="html" hide-below="1024"></tf-column>
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
    // The disk poll runs every 5 s and moves temperature, throughput and
    // latency — none of which the action buttons render or their handlers read.
    // This signature lists everything they DO depend on: `diskId` (the row's
    // identity, and what every handler sends to the core), `name` (the toasts),
    // `role` (which of the "use in pool" and "clear disk" buttons exists) and
    // `locateActive` (the locate icon). Nothing the builder below touches is
    // missing, so a kept element can never act on a stale disk.
    // The reason chip (n03: "3 realok." / "54°C") is part of the builder's
    // output too, but only its PRESENCE shapes the element: its label, tone
    // and tooltip move with the temperature, 1 °C at a time on a warm disk,
    // and no handler reads them. They are patched onto the kept chip by
    // `patchReasonChips` after every render, so a temperature step never
    // recreates the row's buttons (or drops the focus on one of them).
    table.rowActionsKey = (row) => {
      const d = row._disk;
      return `${d.diskId}|${d.name}|${d.role}|${d.locateActive ? 1 : 0}|${diskReasonChip(d) ? 1 : 0}`;
    };
    table.rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const d = row._disk;
      const reason = diskReasonChip(d);
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      wrap.dataset.disk = d.diskId;
      wrap.innerHTML = `
        ${reason ? `<tf-chip size="sm" status="${reason.status}" data-role="reason" label="${escapeAttr(reason.label)}" title="${escapeAttr(reason.title)}"></tf-chip>` : ''}
        <tf-button size="sm" variant="ghost" icon="${d.locateActive ? 'eye' : 'search'}" data-act="locate" title="${escapeAttr(T('disks.locate'))}"></tf-button>
        <tf-button size="sm" variant="ghost" icon="play" data-act="smart" title="${escapeAttr(T('disks.smart_test'))}"></tf-button>
        ${d.role === 'free' ? `<tf-button size="sm" variant="ghost" icon="layers" data-act="use" title="${escapeAttr(T('disks.use_in_pool'))}"></tf-button>` : ''}
        ${d.role === 'used' ? `<tf-button size="sm" variant="ghost" icon="trash" data-act="wipe" title="${escapeAttr(T('disks.wipe'))}"></tf-button>` : ''}
        <tf-button size="sm" variant="secondary" icon="chevron-right" data-act="details">${escapeHtml(T('disks.details'))}</tf-button>`;
      wrap.querySelector('[data-act="details"]').addEventListener('click', (e) => { e.stopPropagation(); this.openDisk(live()._disk.diskId); });
      wrap.querySelector('[data-act="locate"]').addEventListener('click', (e) => { e.stopPropagation(); const cur = live()._disk; this.locateDisk(cur, !cur.locateActive); });
      wrap.querySelector('[data-act="smart"]').addEventListener('click', (e) => { e.stopPropagation(); this.startSmartTest(live()._disk); });
      wrap.querySelector('[data-act="use"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.openPoolWizardForDisk(); });
      // `live()` and not `d`: the plan is read for the disk sitting in THIS
      // row at click time. A kept actions cell survives a sort or a filter,
      // and a wipe is the one action where acting on the row a poll ago would
      // erase the wrong device.
      wrap.querySelector('[data-act="wipe"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.wipeDisk(live()._disk); });
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

  // n03 row action for a disk whose role is the catch-all `used`: it carries
  // a filesystem signature, belongs to no pool and no array this node records,
  // and therefore cannot be offered to either until it is cleared.
  async wipeDisk(disk) {
    if (!disk?.diskId) return;
    try {
      // The Disks tab is repainted from a fresh list so the cleared disk
      // reads as free without waiting out a poll — but only while the tab is
      // still on screen, because `refreshDisks` reads the surface it paints.
      await openDiskWipeDialog(this, disk, () => {
        const surface = this.root.querySelector('#nas-tab-body')?.firstElementChild;
        if (surface?.isConnected) this.refreshDisks(surface);
      });
    } catch (e) {
      toast(T('wipe_disk.failed', { error: errMessage(e) }), 'error');
    }
  },

  async openPoolWizardForDisk() {
    const nodeId = this.currentNode()?.nodeId;
    const surface = this.root.querySelector('#nas-tab-body')?.firstElementChild;
    const isCurrent = () => !this.disposed && surface?.isConnected && this.currentNode()?.nodeId === nodeId;
    const res = await this.nas('tentaNasPoolsListRequest', {}).catch((e) => { toast(errMessage(e), 'error'); return null; });
    if (!isCurrent()) return;
    openPoolWizard(this, { freeDisks: res?.freeDisks || [], pools: res?.pools || [], isCurrent,
      onDone: () => { if (isCurrent()) this.drawTab(); },
      onCreated: ({ name, outcome }) => {
        if (!isCurrent()) return;
        if (outcome === 'approval') this.switchTab('jobs');
        else this.openArray(outcome === 'job' ? name : null);
      },
    });
  },

  paintSmartBulkButton() {
    const btn = this.root.querySelector('[data-act="smart-bulk"]');
    if (!btn) return;
    const n = this.diskSelection.size;
    // This runs on every disks poll (applyDiskRows), and both writes used to be
    // unconditional: `textContent =` replaces the text node, and an identical
    // `setAttribute` still fires attributeChangedCallback, so tf-button rebuilt
    // its insides every tick. Measured on the live page: the toolbar label
    // appeared under 6 different node identities in 21 s.
    setText(btn, T('disks.smart_selected', { n }));
    setAttr(btn, 'disabled', n ? null : true);
  },

  // One password for the whole batch: the prompt appears once and every
  // selected disk starts its short self-test with it. A disk-specific
  // refusal (busy, a test already runs, the disk rejects the command) does
  // not stop the batch; a privilege/credential error does, at once, instead
  // of replaying the same rejected password against sudo once per remaining
  // disk (`runDiskBatch` / `isBatchHaltError` in format.js — shared with the
  // "SMART all disks" schedule action in tasks.js).
  async startSmartTestBulk() {
    const targets = (this.disks || []).filter((d) => this.diskSelection.has(d.diskId));
    if (!targets.length) return;
    const outcome = await this.withSudo((sudoPassword) => runDiskBatch(targets, (d) => this.nas(
      'tentaNasDiskSmartTestRequest', { diskId: d.diskId, kind: 'short', sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS },
    )), T('disks.smart_selected_title', { n: targets.length }));
    // `outcome` is null both when the prompt was cancelled and when the
    // batch halted on a privilege/credential error — `withSudo`'s own catch
    // already toasted that one error.
    if (!outcome) return;
    if (outcome.started.length) toast(T('disks.smart_selected_done', { n: outcome.started.length }), 'success');
    if (outcome.refused.length) toast(T('jobs.smart_batch_refused', { n: outcome.refused.length, disks: refusedBatchNames(outcome.refused) }), 'warning');
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
      this.paintTelemetryAlert(body.querySelector('#nas-disks-telemetry'), res.telemetry);
      this.paintReplacementAdvice(body.querySelector('#nas-disk-advice'), res.advice || []);
      this.applyDiskRows();
    } catch (e) {
      if (this.disposed || !body.isConnected) return;
      toast(T('disks.failed', { error: errMessage(e) }), 'error');
    }
    this.later(() => this.refreshDisks(body), POLL_DISKS_MS);
  },

  /**
   * "Wymień, dopóki dysk jeszcze żyje" (§5.10, research R5). The node decides
   * WHICH disks are here — it has the history — so this only renders them, and
   * renders nothing at all on a healthy node. `urgent` is a disk whose counters
   * are moving; `advice` is one that has simply been unhealthy long enough.
   */
  paintReplacementAdvice(host, advice) {
    if (!host) return;
    if (!advice.length) { patchHtml(host, ''); return; }
    // The text is worded from the advice's codes (`replacementAdviceText`);
    // the node's English `reason` is only the tooltip.
    //
    // An Elastic Array member is never told to be replaced (C3): the node
    // sends it `spareAvailable: false` like a pool disk with no spare, but
    // replacing an array disk does not exist in this version, so "brak spare
    // w puli — przygotuj dysk zastępczy" was advice nobody could follow. Its
    // row says so instead — its chip says "obserwuj", not "zaplanuj", and a
    // reason this build cannot word is "a problem", never "the node
    // recommends replacing" (wave-4 critic minor 8) — and a card of array
    // disks only is not titled "replacement".
    //
    // Patched in place (owner's rule): the card is built once, its title,
    // count and hint are written into it, and the rows are keyed by disk, so
    // one disk's advice changing rebuilds that one row and nothing else.
    const forArray = advice.map((a) => adviceIsForArray(a, this.disks));
    const arrayOnly = forArray.every(Boolean);
    if (patchHtml(host, `
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('alert')} <span data-role="advice-title"></span> <tf-chip size="sm" status="warn" data-role="advice-count"></tf-chip></div>
          <span class="hint" data-role="advice-hint"></span>
        </div>
        <div class="stat-rows" data-role="advice-rows"></div>
      </div>`)) {
      host.querySelector('[data-role="advice-rows"]').addEventListener('click', (e) => {
        const open = e.target.closest('[data-act="advice-open"]');
        const row = open?.closest('[data-advice]');
        if (row) this.openDisk(row.dataset.advice);
      });
    }
    setText(host.querySelector('[data-role="advice-title"]'), T(arrayOnly ? 'replace_advice.array_title' : 'replace_advice.title'));
    setAttr(host.querySelector('[data-role="advice-count"]'), 'label', String(advice.length));
    setText(host.querySelector('[data-role="advice-hint"]'), T(arrayOnly ? 'replace_advice.array_hint' : 'replace_advice.hint'));
    patchKeyedList(host.querySelector('[data-role="advice-rows"]'), advice.map((a, i) => {
      const why = replacementAdviceText(a);
      const text = why.known ? why.text : T(forArray[i] ? 'replace_advice.array_reason_other' : 'replace_advice.reason_other');
      const spare = forArray[i] ? T('replace_advice.array_no_replace')
        : a.spareAvailable ? T('replace_advice.spare_ready') : T('replace_advice.no_spare');
      const kind = ADVICE_KINDS.has(a.severity) ? a.severity : 'other';
      const severity = T((forArray[i] ? 'replace_advice.array_severity_' : 'replace_advice.severity_') + kind);
      return {
        key: a.diskId,
        html: `
          <div class="sr" data-advice="${escapeAttr(a.diskId)}">
            <span class="k">
              <tf-chip size="sm" dot status="${a.severity === 'urgent' ? 'err' : 'warn'}" label="${escapeAttr(severity)}"></tf-chip>
              <span class="mono fw-700">${escapeHtml(a.name)}</span>${a.memberOf ? ` <span class="text-3">${escapeHtml(a.memberOf)}</span>` : ''}
            </span>
            <span class="v">
              <span data-role="advice-reason"${why.title ? ` title="${escapeAttr(why.title)}"` : ''}>${escapeHtml(text)}</span>
              <span class="text-3" data-role="advice-spare">${escapeHtml(spare)}</span>
              <tf-button size="sm" variant="secondary" icon="chevron-right" data-act="advice-open">${escapeHtml(T('disks.details'))}</tf-button>
            </span>
          </div>`,
      };
    }));
  },

  // The filter chips carry their own counts and the pool selector is built
  // from what the node actually reports, so an empty pool never gets a segment.
  //
  // This runs on EVERY disk poll (5 s), not once per entry: `applyDiskRows`
  // calls it, and the counts in the chip labels are exactly what a poll moves.
  // Neither half may rebuild its DOM on an unchanged poll — the pool selector
  // is guarded by `diskPoolSig` below, and the chip bar by tf-filter-chips
  // itself, which skips a render identical to the one already on screen.
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
    patchReasonChips(table, list);
    this.paintSmartBulkButton();
  },

  diskRow(d) {
    const locateActive = Boolean(this.locateState[d.diskId]);
    const hist = d.ioHistoryBps || [];
    return {
      _disk: { ...d, locateActive },
      _selected: this.diskSelection?.has(d.diskId) || false,
      // Styled in css/tentanas-cells.css: the <tr> lives in the table's
      // shadow root, where tentanas.css never reaches.
      _class: d.health === 'critical' ? 'row-danger' : d.health === 'warning' ? 'row-warn' : '',
      health: `<span class="health-cell"><span class="health-dot ${healthClass(d.health)}"></span>${escapeHtml(T('health.' + d.health))}</span>`,
      device: `<div class="cell-2"><div class="l1"><span class="mono">${escapeHtml(d.name)}</span><span class="disk-kind ${escapeAttr(d.kind)}">${escapeHtml(d.kind)}</span></div><div class="l2">${escapeHtml(d.transport || '')}${d.mountpoints && d.mountpoints.length ? ' · ' + escapeHtml(d.mountpoints.join(', ')) : ''}</div></div>`,
      model: `<div class="cell-2"><div class="l1">${escapeHtml(d.model || '—')}</div><div class="l2 mono">${escapeHtml(d.serial || '')}</div></div>`,
      size: fmtBytes(d.sizeBytes),
      role: { status: roleTone(d.role), label: roleChipLabel(d) },
      temp: d.temperatureC == null ? '<span class="text-3">—</span>' : `<span class="${d.temperatureC >= 55 ? 'num-err' : d.temperatureC >= 45 ? 'num-warn' : ''}">${d.temperatureC}°C</span>`,
      rw: `${fmtMBps(d.io?.readBps)} / ${fmtMBps(d.io?.writeBps)}`,
      lat: d.io ? (Number(d.io.awaitMs) || 0).toFixed(1) : '—',
      wear: d.wearPct == null ? '<span class="text-3">—</span>' : `<span class="${d.wearPct >= 90 ? 'num-err' : d.wearPct >= 70 ? 'num-warn' : ''}">${d.wearPct}%</span>`,
      trend: `<div class="trend">${sparklineSvg(hist)}</div>`,
    };
  },

  openDisk(diskId) {
    this.diskId = diskId;
    this.array = null;
    this.pool = null;
    this.dataset = null;
    this.targetId = null;
    this.tab = 'disks';
    this.clearTimers();
    this.setLocation();
    const tabs = this.root.querySelector('#nas-tabs');
    if (tabs) tabs.value = 'disks';
    this.drawTab();
  },

  async locateDisk(disk, enable) {
    try {
      const res = await this.nas('tentaNasDiskLocateRequest', { diskId: disk.diskId, enable });
      if (res.method === 'none') {
        toast(res.detail || T('disks.locate_unsupported'), 'warning');
        return;
      }
      // A failed `ledctl` is NOT an answer of "off": the node sends
      // `active:false` with the tool's own stderr in `detail`
      // (dispatch/tentanas.rs `disk_locate`). Reading `active` alone turned a
      // blink that never started into a green "LED off" toast. A success
      // carries an empty `detail`, so a non-empty one is the failure — and the
      // LED state this session knows stays what it was, because nothing
      // changed on the enclosure.
      if (String(res.detail || '').trim() || (enable && !res.active)) {
        toast(T('disks.locate_failed', { name: disk.name, error: String(res.detail || '').trim() || '—' }), 'error');
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
    if (!job) return false;
    toast(T('jobs.started', { kind: jobKindLabel('smart_test') }), 'success');
    this.refreshJobsBadge();
    if (this.tab === 'jobs') this.drawTab();
    return true;
  },

  // ---------------------------------------------------------------------------
  // Disk detail (n04)
  // ---------------------------------------------------------------------------

  // Builds n04 ONCE — the split drawDisks/refreshDisks already use. This method
  // owns the markup and the listeners; every value a poll can move lives in a
  // host it leaves empty for the paint to write. It is the screen the admin
  // watches SMART and temperature on, so it polls like every other live view
  // instead of standing still until the user navigates away and back.
  //
  // The SMART self-test cadence is read HERE and nowhere else: it is
  // configuration, it moves only through the schedule editor, and that editor
  // redraws the tab. Re-asking for it every five seconds buys nothing.
  async drawDiskDetail(body) {
    body.innerHTML = `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`;
    // Nothing is described yet, and the pool read kept for the disk left
    // behind must not be adopted by this one.
    this.diskDetail = null;
    // Until the disk answers, the bar leads back to the list; its name (never
    // the id this view was opened with) is added once it is known.
    const disksCrumb = { label: T('tabs.disks'), act: 'disks', query: `node=${this.nodeId}&tab=disks` };
    this.setCrumbTail([disksCrumb]);
    let res, schedRes;
    try {
      [res, schedRes] = await Promise.all([
        this.nas('tentaNasDiskGetRequest', { diskId: this.diskId }),
        // The cadence pills are an ornament: a node that cannot answer this
        // still gets the whole disk.
        this.nas('tentaNasSchedulesListRequest', {}).catch(() => null),
      ]);
    } catch (e) {
      if (this.disposed || !body.isConnected) return;
      body.innerHTML = `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`;
      return;
    }
    if (this.disposed || !body.isConnected) return;
    const d = res.disk;
    const smart = schedRes?.smart || null;

    // Labels are fixed, values are not: every field is a `k`/`v` pair whose
    // `v` is addressed by name and written by the paint, so one counter moving
    // cannot rebuild the grid it sits in.
    const field = (label, id) => `<div class="f"><div class="k">${escapeHtml(label)}</div><div class="v" data-f="${escapeAttr(id)}"></div></div>`;

    this.setCrumbTail([disksCrumb, { label: d.name }]);
    body.innerHTML = `
      <div class="stack">
        <div id="nas-dd-error"></div>
        <div class="section-card">
          <div class="section-card-head">
            <div class="title">${sprite('cylinder')} ${escapeHtml(T('disk.identification'))}</div>
            <tf-chip id="nas-dd-health" dot></tf-chip>
          </div>
          <div class="id-grid">
            <div class="id-badge">${sprite('cylinder')}<span class="k">${escapeHtml(d.kind)}</span></div>
            <div class="id-fields">
              ${field(T('disks.col_device'), 'device')}
              ${field(T('disk.serial'), 'serial')}
              ${field('WWN', 'wwn')}
              ${field(T('disk.model'), 'model')}
              ${field(T('disk.path'), 'path')}
              ${field(T('disk.firmware'), 'firmware')}
              ${field(T('disk.transport'), 'transport')}
              ${field(T('disks.col_role'), 'role')}
              ${field(T('disk.power_on'), 'power_on')}
              ${field(T('disk.mountpoints'), 'mountpoints')}
              ${field(T('disk.reallocated'), 'reallocated')}
              ${field(T('disk.pending'), 'pending')}
              ${field(T('disk.crc'), 'crc')}
              ${field(T('disk.media_errors'), 'media_errors')}
              ${field(T('disks.col_wear'), 'wear')}
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
            <div class="section-card-head"><div class="title">${sprite('info')} <span id="nas-dd-why-title"></span></div></div>
            <div class="explain-box" id="nas-dd-why"></div>
            <div id="nas-dd-advice"></div>
            <div class="row mt-md" id="nas-dd-acts"></div>
            <div id="nas-dd-jobs" class="mt-sm"></div>
          </div>
          <div class="section-card">
            <div class="section-card-head"><div class="title">${sprite('alert')} <span id="nas-dd-pool-title"></span></div></div>
            <div id="nas-dd-pool"></div>
          </div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('audit')} ${escapeHtml(T('disk.smart_attributes'))}</div>
            <span class="hint" id="nas-dd-smart-hint"></span></div>
          <div id="nas-dd-attrs"></div>
        </div>
        <div class="grid-2">
          <div class="section-card">
            <div class="chart-head">
              <div class="ch-title">${sprite('zap')} <span id="nas-dd-temp-title"></span></div>
              <div class="ch-val" id="nas-disk-temp-val"></div>
            </div>
            <div id="nas-disk-temp-chart"></div>
          </div>
          <div class="section-card">
            <div class="chart-head">
              <div class="ch-title">${sprite('alert')} <span id="nas-dd-realloc-title"></span></div>
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
          <div id="nas-dd-tests"></div>
        </div>
      </div>`;

    // Wired once, and every handler reads the disk the LATEST poll described —
    // never the one the button happened to be built with.
    const live = () => this.diskDetail?.res.disk || d;
    body.querySelector('[data-act="locate"]').addEventListener('click', () => { const cur = live(); this.locateDisk(cur, !this.locateState?.[cur.diskId]); });
    body.querySelector('[data-act="copy-serial"]').addEventListener('click', async () => {
      await navigator.clipboard?.writeText(live().serial || '');
      toast(T('disk.serial_copied'), 'success');
    });
    body.querySelector('[data-act="smart-schedule"]')?.addEventListener('click', () => openSmartScheduleEditor(this, smart, () => this.drawTab()));
    // Delegated once, like the dashboard's job list: the running-test row is
    // kept across polls, and so are its Log/Cancel buttons.
    body.querySelector('#nas-dd-jobs').addEventListener('click', (e) => {
      const row = e.target.closest('.job-row');
      if (!row) return;
      if (e.target.closest('[data-act="log"]')) { this.openJobLog(row.dataset.job); return; }
      if (e.target.closest('[data-act="cancel"]')) { e.stopPropagation(); this.confirmCancelJob(row.dataset.job); }
    });
    this.locateState = this.locateState || {};

    // `res` is the read this draw already made: opening a disk must not ask
    // the same question twice, so the first paint runs on it and only the
    // polls after it read for themselves.
    await this.refreshDiskDetail(body, res);
  },

  // The poll of n04. It re-reads the disk and — when the disk is in a pool —
  // the pool, because that is where its READ/WRITE/CKSUM counters live; the
  // SMART cadence read by the draw cannot move underneath it. Then it patches
  // and re-arms itself, at the cadence of the disks tab it belongs to.
  async refreshDiskDetail(body, seed = null) {
    // The screen this chain was armed for is gone (unmounted, or the body now
    // belongs to another view): there is nothing to patch and nothing to
    // re-arm. A poll patches; it never redraws what it did not build.
    if (this.disposed || !body.isConnected || !body.querySelector('#nas-dd-health')) return;
    let res = seed;
    if (!res) {
      try {
        res = await this.nas('tentaNasDiskGetRequest', { diskId: this.diskId });
      } catch (e) {
        if (this.disposed || !body.isConnected) return;
        // A failed poll is a transient fact about one request, not a reason to
        // throw a good screen away: the last temperature, health chip and
        // counters stay exactly where they are, the banner says what failed,
        // and the screen keeps asking.
        patchHtml(body.querySelector('#nas-dd-error'),
          `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(errMessage(e))}"></tf-alert>`);
        this.later(() => this.refreshDiskDetail(body), POLL_DISKS_MS);
        return;
      }
      if (this.disposed || !body.isConnected) return;
    }
    const d = res.disk;
    // An Elastic Array member's `memberOf` names its ARRAY, and there is no
    // ZFS pool of that name: asking for one failed on every 5 s poll. Such a
    // disk has no vdev counters to read. The running SMART jobs are read on
    // every poll instead, so the self-test card can show the one in flight and
    // refuse a second (n04:211-221).
    const [poolRes, jobsRes] = await Promise.all([
      d.memberOf && !isArrayMember(d)
        ? this.nas('tentaNasPoolGetRequest', { name: d.memberOf }).catch(() => null)
        : null,
      this.nas('tentaNasJobsListRequest', { limit: 50 }).catch(() => null),
    ]);
    if (this.disposed || !body.isConnected) return;
    // A pool read that failed keeps the counters of the last one that worked:
    // reading it as "not in a pool" would announce a lost membership that
    // nothing has reported. A disk that really left its pool has no `memberOf`
    // any more, and then the kept snapshot goes with it.
    const prev = this.diskDetail;
    let pool = poolRes?.pool || null;
    if (!pool && d.memberOf && prev?.pool?.name === d.memberOf) pool = prev.pool;
    const vdev = pool ? (pool.vdevs || []).find((v) => (v.disks || []).some((x) => x.diskId === d.diskId || x.name === d.name)) : null;
    const leaf = vdev ? (vdev.disks || []).find((x) => x.diskId === d.diskId || x.name === d.name) : null;
    // A jobs read that failed keeps the last known list, for the same reason
    // the pool read does: "no test running" is not what a failure says.
    const jobs = jobsRes ? smartJobsOf(jobsRes.jobs, d) : (prev?.jobs || []);
    this.diskDetail = { res, pool, vdev, leaf, jobs };
    patchHtml(body.querySelector('#nas-dd-error'), '');
    this.paintDiskDetail(body);
    this.later(() => this.refreshDiskDetail(body), POLL_DISKS_MS);
  },

  // Everything on n04 that a poll can move, written into the screen that is
  // already there. Values go through setText/setAttr; the two blocks whose
  // SHAPE depends on the answer (the pool card, the two tables) are patched as
  // one string each, so their skeleton is written once and only the values
  // inside it move afterwards.
  paintDiskDetail(body) {
    const { res, pool, vdev, leaf, jobs = [] } = this.diskDetail;
    const d = res.disk;
    const attrs = res.attributes || [];
    const tests = res.selfTests || [];
    // The replacement recommendation for THIS disk (§5.10), computed by the
    // node from its own history; `null` when there is nothing to recommend.
    const advice = res.advice || null;
    const setField = (id, value) => setText(body.querySelector(`[data-f="${id}"]`), value);

    // n04:175 names the symptom next to the status ("Uwaga: realokacje").
    // Only the first reason fits a chip; the whole list stays in the
    // "Dlaczego status…" box below. The chip is worded like the n03 row chip
    // (`diskHealthChipLabel`); a first reason this build has no word for
    // leaves the chip at the status alone, with the node's sentence in the
    // tooltip.
    const health = healthChip(d.health);
    const chip = body.querySelector('#nas-dd-health');
    const chipLabel = diskHealthChipLabel(d);
    setAttr(chip, 'status', health.status);
    setAttr(chip, 'label', chipLabel.label);
    setAttr(chip, 'title', chipLabel.title);

    setField('device', d.name);
    setField('serial', d.serial || '—');
    setField('wwn', d.wwn || '—');
    setField('model', `${d.model || '—'} · ${fmtBytes(d.sizeBytes)}`);
    setField('path', d.path);
    setField('firmware', d.firmware || '—');
    setField('transport', `${d.transport}${d.rotational ? ` · ${T('disk.rotational')}` : ''}${d.removable ? ` · ${T('disk.removable')}` : ''}`);
    setField('role', roleChipLabel(d));
    setField('power_on', d.powerOnHours == null ? '—' : fmtDuration(d.powerOnHours * 3600));
    setField('mountpoints', d.mountpoints && d.mountpoints.length ? d.mountpoints.join(', ') : '—');
    setField('reallocated', d.reallocatedSectors == null ? '—' : String(d.reallocatedSectors));
    setField('pending', d.pendingSectors == null ? '—' : String(d.pendingSectors));
    setField('crc', d.crcErrors == null ? '—' : String(d.crcErrors));
    setField('media_errors', d.mediaErrors == null ? '—' : String(d.mediaErrors));
    setField('wear', d.wearPct == null ? '—' : `${d.wearPct}%`);

    setText(body.querySelector('#nas-dd-why-title'), T('disk.why_title', { status: health.label }));
    // The whole list, reason by reason in the reader's language; the node's
    // own sentence is the tooltip (see `diskReasonsText`).
    const why = body.querySelector('#nas-dd-why');
    const whyText = diskReasonsText(d);
    setText(why, whyText.text || T('disk.why_ok'));
    setAttr(why, 'title', whyText.title || null);
    // The advice in the reader's language (`replacementAdviceText`); the
    // node's English sentence is only the tooltip.
    const adviceHost = body.querySelector('#nas-dd-advice');
    const adviceWhy = advice ? replacementAdviceText(advice) : null;
    // An Elastic Array member has no replacement to plan (C3, see
    // `paintReplacementAdvice`): its own sentence, with no spare in it.
    const adviceKey = (isArrayMember(d) ? 'replace_advice.array_' : 'replace_advice.disk_')
      + (adviceWhy?.known ? advice.severity : 'other');
    patchHtml(adviceHost, advice
      ? warningHtml(advice.severity === 'urgent' ? 'danger' : 'info', T(adviceKey, {
        reason: adviceWhy.text,
        spare: advice.spareAvailable ? T('replace_advice.spare_ready') : T('replace_advice.no_spare'),
      }))
      : '');
    if (adviceWhy) setAttr(adviceHost?.firstElementChild, 'title', adviceWhy.title || null);

    // "Wymień dysk…" exists only for a disk that is IN a ZFS pool. An Elastic
    // Array member gets none: replacing an array disk is withdrawn, and the
    // button only ever ended in a toast about a vdev that does not exist.
    // The membership card below says what applies instead.
    //
    // A SMART test already in flight — a job this node runs, or a self-test
    // the drive itself reports as running — disables both starters: the
    // drive runs one self-test at a time, and a second long test used to be
    // one click away. The row is patched as one string and re-wired exactly
    // when it was rebuilt.
    const arrayMember = isArrayMember(d);
    const busy = jobs.length > 0 || tests.some((t) => t.status === 'running')
      || Boolean(this.diskSmartStarting || this.diskDetail.startedPending);
    const busyAttr = busy ? ` disabled title="${escapeAttr(T('disk.smart_busy'))}"` : '';
    const acts = body.querySelector('#nas-dd-acts');
    if (patchHtml(acts, `
      ${d.memberOf && !arrayMember ? `<tf-button variant="danger" icon="refresh" data-act="replace">${escapeHtml(T('disk.replace'))}</tf-button>` : ''}
      <tf-button variant="secondary" icon="play" data-act="smart-short"${busyAttr}>${escapeHtml(T('disks.smart_short'))}</tf-button>
      <tf-button variant="secondary" icon="clock" data-act="smart-long"${busyAttr}>${escapeHtml(T('disks.smart_long'))}</tf-button>`)) {
      acts.querySelector('[data-act="replace"]')?.addEventListener('click', () => this.openReplaceForDisk(this.diskDetail.res.disk));
      acts.querySelector('[data-act="smart-short"]').addEventListener('click', () => this.startDiskDetailSmartTest(body, 'short'));
      acts.querySelector('[data-act="smart-long"]').addEventListener('click', () => this.startDiskDetailSmartTest(body, 'long'));
    }
    // The running test itself, drawn by the very row the Tasks tab and the
    // dashboard use (tasks.js), keyed by job so a moving progress bar patches
    // its own row.
    const jobsHost = body.querySelector('#nas-dd-jobs');
    patchKeyedList(jobsHost, jobs.map((j) => ({ key: j.jobId, html: jobRowSkeleton(j) })));
    jobs.forEach((j, i) => paintJobRow(jobsHost.children[i], j));

    if (arrayMember) {
      setText(body.querySelector('#nas-dd-pool-title'), d.role === 'other_org_array'
        ? T('disk.other_org_array_title')
        : T('disk.array_title', { array: d.memberOf || '—' }));
      this.paintDiskArrayMembership(body, d);
    } else {
      setText(body.querySelector('#nas-dd-pool-title'), T('disk.pool_errors_title', { pool: d.memberOf || '—' }));
      this.paintDiskPoolErrors(body, pool, vdev, leaf);
    }

    setText(body.querySelector('#nas-dd-smart-hint'), [
      d.smartReadAt ? T('disk.smart_read', { t: fmtAgo(d.smartReadAt) }) : T('disk.smart_never'),
      d.smartPassed === false ? T('disk.smart_failed') : null,
      T('disk.attr_hint'),
    ].filter(Boolean).join(' · '));

    // tf-table has no empty state of its own, so an empty list is a muted
    // line and the table is not on screen at all; the skeleton is a constant
    // string, which is why it is written once and the rows then flow into the
    // element that is already there.
    const attrsHost = body.querySelector('#nas-dd-attrs');
    patchHtml(attrsHost, attrs.length ? `<tf-table id="nas-attr-table">
      <tf-column key="id" label="ID" renderer="text" width="60"></tf-column>
      <tf-column key="name" label="${escapeAttr(T('disk.attr_name'))}" renderer="text" fill></tf-column>
      <tf-column key="value" label="${escapeAttr(T('disk.attr_value'))}" renderer="num"></tf-column>
      <tf-column key="raw" label="${escapeAttr(T('disk.attr_raw'))}" renderer="text" nowrap></tf-column>
      <tf-column key="trend" label="${escapeAttr(T('disk.attr_trend'))}" renderer="html" nowrap hide-below="1024"></tf-column>
      <tf-column key="status" label="${escapeAttr(T('disks.col_health'))}" renderer="chip"></tf-column>
    </tf-table>` : `<div class="muted">${escapeHtml(d.smartAvailable ? T('disk.smart_no_attrs') : T('disk.smart_unavailable'))}</div>`);
    setRowsIfChanged(attrsHost.querySelector('#nas-attr-table'), attrs.map((a) => ({
      id: String(a.id),
      name: a.name,
      value: a.value,
      raw: a.rawText || String(a.raw),
      trend: a.rawWeekAgo == null ? '<span class="text-3">—</span>' : trendHtml(a.raw, a.rawWeekAgo),
      status: { status: a.status === 'ok' ? 'ok' : a.status === 'critical' ? 'err' : a.status === 'warning' ? 'warn' : 'info', label: T('health.' + (['ok', 'warning', 'critical'].includes(a.status) ? a.status : 'unknown')), dot: true },
    })));

    const testsHost = body.querySelector('#nas-dd-tests');
    patchHtml(testsHost, tests.length ? `<tf-table id="nas-st-table">
      <tf-column key="date" label="${escapeAttr(T('disk.st_col_date'))}" renderer="text" nowrap width="170"></tf-column>
      <tf-column key="kind" label="${escapeAttr(T('disk.st_col_kind'))}" renderer="chip" width="100"></tf-column>
      <tf-column key="result" label="${escapeAttr(T('disk.st_col_result'))}" renderer="html" fill></tf-column>
      <tf-column key="hours" label="${escapeAttr(T('disk.st_col_hours'))}" renderer="text" nowrap width="150"></tf-column>
    </tf-table>` : `<div class="muted">${escapeHtml(T('disk.no_self_tests'))}</div>`);
    setRowsIfChanged(testsHost.querySelector('#nas-st-table'), tests.map((t) => ({
      date: t.startedAt ? fmtDate(t.startedAt) : '—',
      kind: { status: t.kind.toLowerCase().includes('extended') || t.kind.toLowerCase().includes('long') ? 'accent' : 'neutral', label: t.kind },
      result: `<tf-chip size="sm" status="${t.status === 'passed' ? 'ok' : t.status === 'running' ? 'info' : t.status === 'failed' ? 'err' : 'warn'}" dot label="${escapeAttr(T('disk.st_status_' + (['passed', 'failed', 'running'].includes(t.status) ? t.status : 'unknown')))}"></tf-chip> <span class="text-3">${escapeHtml(t.detail || '')}</span>`,
      // The SMART self-test log carries the disk's power-on counter at the
      // test, never the test's own duration — the column says so, and a log
      // row without the counter shows nothing instead of a bogus "0 h".
      hours: t.lifetimeHours ? `${t.lifetimeHours} h` : '—',
    })));

    const historyDays = Number(res.historyDays) || 0;
    setText(body.querySelector('#nas-dd-temp-title'), T('disk.temp_history', { d: historyDays }));
    setText(body.querySelector('#nas-dd-realloc-title'), T('disk.realloc_history', { d: historyDays }));
    this.drawDiskHistory(body, res.history || []);
  },

  // Starting a test from n04 greys both starters AT ONCE, before the request
  // goes out, so a double click cannot queue two. A start the node accepted
  // keeps them grey until the next poll, which then shows the job it finds
  // (`startedPending` lives on the poll's own state, so that poll clears it).
  // A start that did not happen (refused, cancelled prompt) gives them back.
  async startDiskDetailSmartTest(body, kind) {
    const disk = this.diskDetail?.res.disk;
    if (!disk || this.diskSmartStarting) return;
    this.diskSmartStarting = true;
    this.paintDiskDetail(body);
    let started = false;
    try {
      started = await this.startSmartTest(disk, kind);
    } finally {
      this.diskSmartStarting = false;
    }
    if (this.disposed || !body.isConnected || !this.diskDetail) return;
    if (started) this.diskDetail.startedPending = true;
    this.paintDiskDetail(body);
  },

  // The membership card of an Elastic Array disk (n04). Such a disk is a
  // branch of a union mount, not a vdev leaf, so it has no READ/WRITE/CKSUM
  // counters to show — the old card said "Błędy z warstwy puli" above "Dysk
  // nie należy do żadnej puli", contradicting itself. It names the array and
  // the part the disk plays, says that replacing an array disk is withdrawn
  // (the node refuses it: dispatch/tentanas.rs `elastic_replace_disk`), and
  // opens the array. A disk of ANOTHER organisation's array arrives with the
  // array name blanked (`hide_other_org_array`), and the card says only that.
  paintDiskArrayMembership(body, d) {
    const host = body.querySelector('#nas-dd-pool');
    if (d.role === 'other_org_array') {
      patchHtml(host, `<div class="muted">${escapeHtml(T('disk.other_org_array_info'))}</div>`);
      return;
    }
    const statRow = (key, label) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v" data-c="${key}"></span></div>`;
    if (patchHtml(host, `
      <div class="stat-rows">
        ${statRow('array', T('disk.array_name'))}
        ${statRow('part', T('disk.array_part'))}
      </div>
      ${warningHtml('info', T('disk.array_replace_withdrawn'))}
      <div class="row mt-md"><tf-button variant="ghost" size="sm" icon="layers" data-act="open-array"></tf-button></div>`)) {
      host.querySelector('[data-act="open-array"]').addEventListener('click', () => this.openArray(this.diskDetail.res.disk.memberOf));
    }
    const part = ARRAY_PART_KEYS[d.arrayRole];
    setText(host.querySelector('[data-c="array"]'), d.memberOf || '—');
    setText(host.querySelector('[data-c="part"]'), part ? T(part) : '—');
    setText(host.querySelector('[data-act="open-array"]'), T('disk.array_open', { array: d.memberOf || '—' }));
  },

  // The pool's own view of this disk (n04): its READ/WRITE/CKSUM counters, the
  // leaf state inside its vdev and the last scrub. The five rows are a shape
  // that depends only on whether the disk is in a pool at all, so the shape is
  // written once and each counter then moves on its own — an error that
  // appears must not rebuild the scrub line next to it.
  paintDiskPoolErrors(body, pool, vdev, leaf) {
    const host = body.querySelector('#nas-dd-pool');
    if (!pool || !vdev || !leaf) {
      patchHtml(host, `<div class="muted">${escapeHtml(T('disk.pool_none'))}</div>`);
      return;
    }
    const row = (key, label) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v" data-c="${key}"></span></div>`;
    if (patchHtml(host, `
      <div class="stat-rows">
        ${row('read', 'READ')}
        ${row('write', 'WRITE')}
        ${row('cksum', 'CKSUM')}
        ${row('state', T('disk.pool_vdev_state'))}
        ${row('scrub', T('disk.pool_last_scrub'))}
      </div>
      ${warningHtml('info', T('disk.pool_errors_info'))}
      <div class="row mt-md"><tf-button variant="ghost" size="sm" icon="layers" data-act="open-pool"></tf-button></div>`)) {
      host.querySelector('[data-act="open-pool"]').addEventListener('click', () => this.openPool(this.diskDetail.pool.name));
    }
    const cell = (key) => host.querySelector(`[data-c="${key}"]`);
    // A counter above zero is the whole point of this card, so the tone rides
    // on the cell itself instead of a span this would have to rebuild.
    const counter = (key, n) => {
      const el = cell(key);
      setText(el, String(Number(n) || 0));
      setAttr(el, 'class', `v ${Number(n) > 0 ? 'num-err' : 'num-ok'}`);
    };
    counter('read', leaf.readErrors);
    counter('write', leaf.writeErrors);
    counter('cksum', leaf.cksumErrors);
    patchHtml(cell('state'), `${stateChipHtml(leaf.state)} <span class="mono text-3">${escapeHtml(vdev.id)} · ${escapeHtml(layoutLabel(vdev.kind))}</span>`);
    setText(cell('scrub'), pool.lastScrubAt
      ? T('disk.pool_scrub_value', { t: fmtDate(pool.lastScrubAt), n: Number(pool.scan?.errors) || 0 })
      : T('disk.pool_no_scrub'));
    setText(host.querySelector('[data-act="open-pool"]'), T('disk.pool_open', { pool: pool.name }));
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
  //
  // These two are NOT live streams: the node samples the disk itself and
  // publishes days of history, so a poll that brings the same series has
  // nothing to draw. It must then leave both charts alone — mounting a new
  // <tf-line-chart> every five seconds restarts the line's draw animation and
  // drops whatever the pointer was hovering. The series the node appends to is
  // what moves them, so the signature is the sample count plus the ends of the
  // window: history only ever grows at the back.
  drawDiskHistory(body, history) {
    const samples = (history || [])
      .map((h) => ({ ...h, t: parseServerTs(h.at)?.getTime() }))
      .filter((h) => h.t != null)
      .sort((a, b) => a.t - b.t);
    const last = samples[samples.length - 1];
    const sig = samples.length
      ? `${samples.length}|${samples[0].t}|${last.t}|${last.temperatureC}|${last.reallocatedSectors}`
      : '0';
    // The signature is remembered on the chart HOST, not on the tab body: the
    // body outlives this view, and a key left on it would tell a freshly drawn
    // screen that it has already plotted a series it has never seen.
    const temp = body.querySelector('#nas-disk-temp-chart');
    if (!temp || temp.__tfHistory === sig) return;
    temp.__tfHistory = sig;
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
  // Job rows shared by the overview card and the tasks tab (n02/n15) — the
  // markup and painter (`jobRowSkeleton`/`paintJobRow`) live in
  // modules/tentanas/tasks.js and are imported above; this is only the
  // cancel action the overview's delegated click listener (drawOverview)
  // calls into, the same confirm-then-cancel flow n15 runs for its own list.
  // ---------------------------------------------------------------------------

  async cancelOverviewJob(jobId, body) {
    if (await this.confirmCancelJob(jobId)) this.refreshOverview(body);
  },

  // Asks, then cancels. True only when the node accepted the cancel; the
  // caller decides what to repaint (n04 simply lets its own poll show it).
  async confirmCancelJob(jobId) {
    const ok = await TfWindow.confirm({ title: T('jobs.cancel'), message: T('jobs.cancel_confirm'), confirmLabel: T('jobs.cancel'), cancelLabel: I18n.t('common.cancel'), danger: true });
    if (!ok) return false;
    try {
      await this.nas('tentaNasJobCancelRequest', { jobId });
      return true;
    } catch (e) {
      toast(errMessage(e), 'error');
      return false;
    }
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
      // n16:169 states the validation result, not the path — provisioning only
      // leaves the file in place when `visudo -c` accepted it, so a present
      // sudoers file IS the OK. The path lives in the <details> block below.
      [T('elevation.row_sudoers'), el.mode === 'helper' && el.helperState !== 'sudoers_missing' ? escapeHtml(T('elevation.sudoers_value')) : '—'],
      [T('elevation.row_provisioning'), el.provisionedAt ? escapeHtml(T('elevation.provisioning_value', { date: fmtDate(el.provisionedAt), user: el.provisionedBy || '—' })) : escapeHtml(T('elevation.provisioning_none'))],
      [T('elevation.row_audit'), `${escapeHtml(T('elevation.audit_value', { n: Number(el.auditEntries) || 0 }))} · <a data-act="audit-log">${escapeHtml(T('elevation.audit_link'))}</a>`],
      ...(el.mode === 'helper' ? [] : [
        [T('elevation.row_user'), `<span class="mono">${escapeHtml(el.coreUser)}</span>`],
        [T('elevation.row_ttl'), escapeHtml(fmtDuration(el.ttlSecs))],
      ]),
    ];

    const actions = [];
    if (admin) {
      if (channelMode(el.mode) === 'unarmed') {
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
    // n16:185-197 describes both modes side by side; the "(obecny)" marker
    // belongs only to the one this node actually runs.
    const modeACurrent = el.mode === 'helper' ? T('elevation.explain_current') : '';

    body.innerHTML = `
      <div class="stack">
        ${env.fullSupport ? '' : `<tf-alert tone="warning" title="${escapeAttr(T('env.partial_support'))}" message="${escapeAttr(T('env.partial_support_msg', { os: env.osName }))}"></tf-alert>`}
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('key')} ${escapeHtml(T('elevation.title'))}</div><span class="hint">${escapeHtml(T('elevation.hint'))}</span></div>
          <div class="grid-2">
            <div>
              ${channelChip}
              <div class="stat-rows mt-sm">${channelRows.map(([k, v]) => `<div class="sr"><span class="k">${escapeHtml(k)}</span><span class="v">${v}</span></div>`).join('')}</div>
              ${el.mode === 'helper' ? `<details class="sudoers"><summary>${escapeHtml(T('elevation.show_sudoers'))}</summary><pre class="cmd mono" id="nas-sudoers">${escapeHtml(T('elevation.plan_loading'))}</pre></details>` : ''}
              ${actions.length ? `<div class="row mt-md">${actions.join('')}</div>` : admin ? '' : `<div class="muted mt-md">${escapeHtml(T('elevation.admin_only'))}</div>`}
            </div>
            <div class="stack">
              <div class="explain-box">
                <p>${T('elevation.explain_helper_p', { current: modeACurrent })}</p>
              </div>
              <div class="explain-box">
                <p>${T('elevation.explain_interactive_p', { ttl: fmtDuration(el.ttlSecs) })}</p>
                <ul class="loss-list">
                  <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('elevation.explain_interactive_1'))}</span></li>
                  <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('elevation.explain_interactive_2'))}</span></li>
                  <li class="ll bad">${sprite('x')}<span>${escapeHtml(T('elevation.explain_interactive_3'))}</span></li>
                </ul>
              </div>
            </div>
          </div>
        </div>
        <div class="section-card">
          <div class="section-card-head">
            <div class="title">${sprite('cpu')} ${escapeHtml(T('arc.settings_title'))}</div>
            <span class="hint">${escapeHtml(nodeT('arc.settings_hint', this.currentNode(), { ram: fmtBytes(env.ramBytes) }))}</span>
          </div>
          <div id="nas-env-arc"><div class="muted">${escapeHtml(I18n.t('common.loading'))}</div></div>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('layers')} ${escapeHtml(T('env.features'))}</div>
            <div class="actions"><tf-button variant="secondary" size="sm" icon="refresh" data-act="reprobe">${escapeHtml(T('reprobe'))}</tf-button></div></div>
          <tf-table id="nas-feature-table" actions-label="${escapeAttr(I18n.t('common.actions'))}">
            <tf-column key="name" label="${escapeAttr(T('env.col_feature'))}" renderer="html" fill></tf-column>
            <tf-column key="status" label="${escapeAttr(T('env.col_status'))}" renderer="chip"></tf-column>
            <tf-column key="version" label="${escapeAttr(T('env.col_version_detail'))}" renderer="html"></tf-column>
          </tf-table>
        </div>
        <div class="section-card">
          <div class="section-card-head"><div class="title">${sprite('cluster')} ${escapeHtml(T('env.other_nodes'))}</div><span class="hint">${escapeHtml(T('env.other_nodes_hint'))}</span></div>
          <tf-table id="nas-others-table" actions-label="${escapeAttr(I18n.t('common.actions'))}" empty-message="${escapeAttr(T('env.no_other_nodes'))}">
            <tf-column key="name" label="${escapeAttr(T('env.col_node'))}" renderer="html" fill></tf-column>
            <tf-column key="platform" label="${escapeAttr(T('env.col_platform'))}" renderer="text"></tf-column>
            <tf-column key="channel" label="${escapeAttr(T('env.col_channel'))}" renderer="chip"></tf-column>
            <tf-column key="features" label="${escapeAttr(T('env.col_features'))}" renderer="text"></tf-column>
          </tf-table>
        </div>
      </div>`;

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
      // 'exposed' is a warning, not an absence: the node HAS the hardware and
      // the tools, and its RDMA interface also routes the world — a network
      // the admin can fix, which installing a package never would (§5.4b).
      status: { status: f.status === 'ok' ? 'ok' : f.status === 'broken' ? 'err' : ['outdated', 'version_too_low', 'exposed', 'unknown'].includes(f.status) ? 'warn' : f.optional ? 'info' : 'err', label: T('feature_status.' + f.status), dot: true },
      version: `<span class="mono">${escapeHtml([
        f.version ? `${f.version}${f.requiredVersion ? ` (≥ ${f.requiredVersion})` : ''}` : f.requiredVersion ? `≥ ${f.requiredVersion}` : '',
        f.detail || '',
      ].filter(Boolean).join(' · ') || '—')}</span>`,
    }));
    ftable.rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
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
      b.addEventListener('click', (e) => { e.stopPropagation(); this.installFeature(live()._feature); });
      return b;
    };

    const otable = body.querySelector('#nas-others-table');
    otable.rows = others.map((n) => ({
      _node: n,
      // The node id is NOT a name and the mockups never show one (n16 prints
      // `atlas`, `orion`) — not as text and not as a tooltip (owner's rule:
      // no ids anywhere in the GUI).
      name: `<div class="cell-2"><div class="l1">${escapeHtml(nodeLabel(n))}${n.isLocal ? ` <span class="text-3">(${escapeHtml(T('this_node'))})</span>` : ''}${n.online ? '' : ` <tf-chip status="info" label="${escapeAttr(T('offline'))}"></tf-chip>`}</div></div>`,
      platform: n.instanceStatus === 'ready' ? (n.osName || '—') : T('instance.' + n.instanceStatus),
      channel: { status: channelMode(n.elevationMode) === 'unarmed' ? 'warn' : 'ok', label: T('elevation.mode_' + channelMode(n.elevationMode)), dot: true },
      features: (n.features || []).join(' · ') || (n.instanceStatus === 'ready' ? T('env.features_unknown') : T('instance.' + n.instanceStatus)),
    }));
    otable.rowActions = (row, idx, currentRow) => {
      const live = () => currentRow?.() ?? row;
      const n = row._node;
      if (n.instanceStatus !== 'ready') return null;
      const wrap = document.createElement('div');
      wrap.className = 'row-actions';
      const unarmed = channelMode(n.elevationMode) === 'unarmed';
      wrap.innerHTML = unarmed && admin
        ? `<tf-button size="sm" variant="secondary" icon="unlock" data-act="arm-node">${escapeHtml(T('env.arm_node'))}</tf-button>`
        : `<tf-button size="sm" variant="ghost" icon="chevron-right" data-act="go">${escapeHtml(T('env.go_to_node'))}</tf-button>`;
      wrap.querySelector('[data-act="go"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.selectNode(live()._node.nodeId); });
      wrap.querySelector('[data-act="arm-node"]')?.addEventListener('click', (e) => { e.stopPropagation(); this.armNode(live()._node); });
      return wrap;
    };
    otable.addEventListener('row-click', (e) => { if (e.detail.row._node.instanceStatus === 'ready') this.selectNode(e.detail.row._node.nodeId); });

    this.paintArcSettings(body, env);
  },

  // n17b for a node other than the selected one: the password goes straight to
  // that node, the view stays where it is.
  async armNode(node) {
    const creds = await this.promptSudo(T('env.arm_node_title', { node: nodeLabel(node) }), node, T('sudo.arm_confirm'));
    if (!creds) return;
    try {
      await this.nasOn(node, 'tentaNasElevationArmRequest', { sudoPassword: creds.password, ttlSecs: 0 }, { timeoutMs: ADMIN_TIMEOUT_MS });
      toast(nodeT('env.armed_node', node), 'success');
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
    const current = Math.max(10, Math.min(75, Math.round((Number(arc.maxBytes) || 0) / ram * 100)));
    host.innerHTML = `
      <div class="slider-row">
        <div>
          <div class="sl-name">${escapeHtml(T('arc.slider_name'))}</div>
          <div class="sl-desc">${escapeHtml(T('arc.slider_desc'))}</div>
        </div>
        <tf-slider id="nas-arc-slider" min="10" max="75" step="1" value="${current}" ${this.isAdmin ? '' : 'disabled'}></tf-slider>
        <div class="sl-val" id="nas-arc-val">${escapeHtml(T('arc.slider_value', { pct: current, size: fmtBytes(ram * current / 100) }))}</div>
      </div>
      <div class="grid-2 mt-md">
        <div class="stat-rows">
          <div class="sr"><span class="k">${escapeHtml(T('arc.live_usage'))}</span><span class="v">${escapeHtml(fmtBytes(arc.sizeBytes))}</span></div>
          <div class="sr"><span class="k">${escapeHtml(T('arc.hit_ratio_24h'))}</span><span class="v num-ok">${(Number(arc.hitRatio) || 0).toFixed(1)}% · <a data-act="arc-details">${escapeHtml(T('arc.details'))}</a></span></div>
        </div>
        ${warningHtml('warning', T('arc.warning'))}
      </div>
      <div class="row mt-md" style="justify-content:flex-end">
        <tf-button variant="primary" icon="save" data-act="arc-apply" disabled>${escapeHtml(T('arc.apply'))}</tf-button>
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
  async withSudo(fn, title, isCurrent = () => true) {
    const sourceNodeId = this.nodeId;
    const checkContext = () => {
      if (this.disposed || this.nodeId !== sourceNodeId || !isCurrent()) throw new Error(T('targets.context_changed'));
    };
    try {
      checkContext();
      if (!this.environment) await this.refreshHeader(false);
      checkContext();
      const el = this.environment?.elevation;
      const armed = el && el.armedUntil && parseServerTs(el.armedUntil) && parseServerTs(el.armedUntil).getTime() > Date.now();
      const needsPassword = !el || channelMode(el.mode) === 'unarmed' || (el.mode === 'interactive' && !armed) || (el.mode === 'helper' && el.helperState !== 'ok');
      if (!needsPassword) return await fn(undefined);
      const creds = await this.promptSudo(title);
      if (!creds) return null;
      checkContext();
      if (creds.remember) {
        await this.nas('tentaNasElevationArmRequest', { sudoPassword: creds.password, ttlSecs: 0 }, { timeoutMs: ADMIN_TIMEOUT_MS });
        checkContext();
        this.refreshHeader(false);
        return await fn(undefined);
      }
      return await fn(creds.password);
    } catch (e) {
      toast(errMessage(e), 'error');
      return null;
    }
  },

  // `confirmLabel` names what the primary actually does: only the n17b arming
  // prompt (armNode) arms the channel, every other caller just runs one
  // privileged operation with a one-shot password.
  promptSudo(title, node = this.currentNode(), confirmLabel = T('sudo.confirm')) {
    const user = this.environment?.elevation?.coreUser || 'tentaflow';
    const ttl = fmtDuration(this.environment?.elevation?.ttlSecs || 900);
    return new Promise((resolve) => {
      const win = document.createElement('tf-window');
      win.className = 'nas-modal';
      win.setAttribute('title', title || T('sudo.title', { node: nodeLabel(node) }));
      win.setAttribute('icon', 'key');
      win.setAttribute('buttons', 'close');
      win.setAttribute('width', '520');
      win.setAttribute('initial-x', 'center');
      win.setAttribute('initial-y', 'center');
      win.innerHTML = `
        <div slot="body" class="stack">
          <div class="explain-box">${escapeHtml(node.isLocal ? T('sudo.explain_local') : nodeT('sudo.explain_remote', node))}</div>
          <tf-input id="nas-sudo-pass" type="password" autocomplete="current-password" autofocus label="${escapeAttr(nodeT('sudo.password_label', node, { user }))}"></tf-input>
          <div class="toggle-card">
            <div class="tc-text"><span>${escapeHtml(T('sudo.remember', { ttl }))}</span><span class="tc-sub">${escapeHtml(T('sudo.remember_sub', { ttl }))}</span></div>
            <tf-toggle id="nas-sudo-remember"></tf-toggle>
          </div>
          ${warningHtml('info', T('sudo.ttl_info', { ttl }))}
        </div>
        <div slot="footer">
          <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
          <tf-button variant="primary" icon="key" data-action="confirm">${escapeHtml(confirmLabel)}</tf-button>
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
    win.setAttribute('title', T('wizard.title', { node: nodeLabel(node) }));
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
          <h1>${escapeHtml(T('wizard.heading'))} <span class="version">${escapeHtml(T('wizard.node_tag', { node: nodeLabel(node) }))}</span></h1>
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
        <p class="wizard-section-sub">${escapeHtml(node.isLocal ? T('sudo.explain_local') : nodeT('sudo.explain_remote', node))}</p>
        <div class="stack">
          <tf-input id="nas-wz-pass" type="password" autocomplete="current-password" autofocus label="${escapeAttr(nodeT('sudo.password_label', node, { user: env?.elevation?.coreUser || 'tentaflow' }))}" value="${escapeAttr(state.password)}"></tf-input>
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
      // The post-install step has to retire itself. It is raised by `forceSetup`
      // and the wizard is the only thing that resolves it, so leaving it up
      // after a successful run tells an admin who just configured the node that
      // nothing happened — at the exact moment the feature should show it
      // worked. Judge on the REFRESHED environment, not on the wizard's own
      // result: a wizard can report success while the channel still fails
      // `channelUnusable()`, and in that case the step must stay.
      //
      // Redraw whenever the step was on screen, not only on the environment
      // tab: the post-install route lands on `overview`, so a tab-conditional
      // redraw left the stale "not configured" panel in place.
      const wasForced = this.forceSetup;
      this.refreshHeader(false).then(() => {
        if (this.disposed) return;
        if (this.forceSetup && !this.channelUnusable()) this.forceSetup = false;
        if (this.tab === 'environment' || wasForced) this.drawTab();
      });
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
    const sourceNodeId = this.nodeId;
    const sourceRoot = this.root;
    const isCurrent = () => !this.disposed && this.nodeId === sourceNodeId && this.root === sourceRoot && sourceRoot.isConnected;
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
      if (!isCurrent()) { win.close(true); return; }
      try {
        const r = await this.nas('tentaNasJobGetRequest', { jobId });
        if (!isCurrent() || !win.isConnected) return;
        const j = r.job;
        const author = jobAuthor(j.startedBy);
        const subject = jobSubject(j);
        const head = win.querySelector('#nas-joblog-head');
        const pre = win.querySelector('#nas-joblog');
        if (!head || !pre) return;
        patchHtml(head, `${escapeHtml(jobKindLabel(j.kind))} <span class="mono"${subject.title ? ` title="${escapeAttr(subject.title)}"` : ''}>${escapeHtml(subject.text)}</span> <tf-chip status="${jobTone(j.status)}" label="${escapeAttr(T('jobs.status_' + j.status))}"></tf-chip> · <span${author.title ? ` title="${escapeAttr(author.title)}"` : ''}>${escapeHtml(T('jobs.started_by', { by: author.label, t: fmtAgo(j.startedAt) }))}</span>${j.error ? `<div class="num-err mt-sm">${escapeHtml(j.error)}</div>` : ''}`);
        paintJobLog(pre, j.log);
        if (j.status === 'running' || j.status === 'queued') timer = setTimeout(poll, POLL_JOB_MODAL_MS);
        else if (onFinish && !notified) { notified = true; onFinish(j); }
      } catch (e) {
        if (!isCurrent() || !win.isConnected) return;
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
/// The subject kind, in the reader's language. It reaches the UI as a raw
/// enum string (`pool`, `disk`, `target`, …) and used to be printed that way
/// in the alert subline — one English word in the middle of a Polish sentence.
/// An unknown kind falls back to itself: a new alert source is still readable,
/// just untranslated, which is better than an empty subline.
function subjectKindLabel(kind) {
  const key = `alerts.subject.${kind}`;
  const label = T(key);
  // `T` answers a missing key with the FULL path, prefix included.
  return label === `tentanas.${key}` ? kind : label;
}

// An alert's `subjectId` is a NAME for most kinds — the node passes `spec.name`
// for an array, `row.name` for a target — and printing it is the point: "target
// vm-store" is what an admin recognises. For two kinds it is a machine id
// instead: a disk raises on `wwn-5000cca27dc7a4c6` (disks.rs builds it as
// `wwn-<hex>` / `sn-<serial>`) and an approval on the request UUID. Those say
// nothing the title has not already said, so the subline drops them rather
// than deciding per kind — a kind added later must not silently start
// printing a GUID.
// `dev-<name>` is the third shape `disks.rs` gives a disk: a virtual disk with
// neither a WWN nor a serial is keyed by its kernel name behind that prefix,
// and the title already names it. Only a `disk` subject can take that shape;
// every other kind's subject is a pool/target/array/dataset name that must
// not be hidden just because it starts with the same prefix (a pool named
// `usb-backup`, a target `pci-store`) or is all digits (a pool `2024`) — so
// only `disk` uses the disk rule, everything else (including `approval`,
// whose subject is a request UUID) uses the narrower opaque rule.
// The shapes live in machine-id.js, shared with the job rows.
function alertSubjectName(subjectId, subjectKind) {
  const value = String(subjectId || '');
  const isId = subjectKind === 'disk' ? isDiskIdShape(value) : isOpaqueId(value);
  return isId ? '' : value;
}

function alertTarget(alert) {
  if (!alert) return { act: 'details', tab: 'overview', extra: {} };
  if (alert.subjectKind === 'elevation') return { act: 'arm', tab: 'environment', extra: {} };
  if (alert.subjectKind === 'disk') {
    return alert.subjectId
      ? { act: 'details', tab: 'disks', extra: { disk: alert.subjectId } }
      : { act: 'disks', tab: 'disks', extra: {} };
  }
  if (alert.subjectKind === 'pool') return { act: 'pool', tab: 'pools', extra: { pool: alert.subjectId } };
  // An Elastic Array raises on its NAME (`elastic.rs`, `scheduler.rs`), and
  // what the alert asks for — a restore, a repair, a settled mover — is done
  // on that array's own pane. Without this branch the button led to the
  // Overview the admin was already on.
  if (alert.subjectKind === 'elastic-array' && alert.subjectId) {
    return { act: 'array', tab: 'pools', extra: { array: alert.subjectId } };
  }
  // A four-eyes request (§5.10) is answered where its queue is, not on the
  // overview: the admin who followed the alert came to decide on it.
  if (alert.subjectKind === 'approval') return { act: 'manage', tab: 'jobs', extra: {} };
  // A block target's alert (today: a portal whose address drifted, §5.5) lands
  // on the Sharing tab, where the targets table and its state chip are. Not
  // the target's own window: re-picking the interface is the wizard's job, and
  // the row is where the admin sees whether other targets drifted with it.
  // The button is named after the SURFACE it lands on, the way the disk rows
  // are ("Disks"): "Details" is this app's word for a detail window, and this
  // one goes to a list. The target's name rides along so the Sharing tab can
  // put the admin on the right row instead of at the top of the table.
  if (alert.subjectKind === 'target') {
    return { act: 'shares', tab: 'shares', extra: { target: alert.subjectId } };
  }
  // The node-wide reconcile alert (`targets:reconcile`): "this node cannot
  // reach the state it decided on". It is about the block targets as a whole,
  // so it lands on the same tab as they do — and its button is named after
  // that tab, not "Details", which is this app's word for a detail screen.
  // Without this branch it fell through to the Dashboard, which is where the
  // admin already was.
  if (alert.subjectKind === 'node' && alert.subjectId === 'targets') {
    return { act: 'shares', tab: 'shares', extra: {} };
  }
  return { act: 'details', tab: 'overview', extra: {} };
}

// n02 IOPS tile: the value now against the node's own mean over the last
// hour ("+12% vs śr. godzinowa"). A sampler that has not built a baseline
// yet — or a node that was idle for the whole hour — has no percentage to
// show, so the tile names the mean instead of dividing by zero.
function iopsBaseline(now, hourAvg) {
  const avg = Number(hourAvg) || 0;
  if (avg <= 0) return { delta: T('kpi.iops_avg', { n: Math.round(avg) }), 'delta-type': null };
  const change = Math.round((now - avg) / avg * 100);
  return {
    delta: T('kpi.iops_delta', { pct: `${change > 0 ? '+' : ''}${change}` }),
    'delta-type': change > 0 ? 'up' : change < 0 ? 'down' : 'neutral',
  };
}

function roleTone(role) {
  return role === 'free' ? 'info' : role === 'system' ? 'warn' : role === 'partitioned' ? 'info' : 'ok';
}

// n03 role chip: pool first, then the concrete layout or group role
// ("tank · RAIDZ2", "tank · Special"). The inventory carries the owning vdev
// on the disk row, so the chip needs no pool topology of its own.
// The parts of an Elastic Array this build can name, `NasDisk.arrayRole` →
// locale key.
const ARRAY_PART_KEYS = {
  data: 'role.array_data',
  cache: 'role.array_cache',
  parity: 'role.array_parity',
};

// A disk the node counts as a branch of an Elastic Array — this
// organisation's (`array_member`) or another's (`other_org_array`, name
// blanked). Its `memberOf` is an ARRAY name, never a ZFS pool.
function isArrayMember(disk) {
  return disk?.role === 'array_member' || disk?.role === 'other_org_array';
}

// Whether a replacement advice is about an Elastic Array member. The advice
// carries no role of its own, so the disk is looked up in the list the same
// poll brought (`refreshDisks`); a disk not in it is taken as a pool disk,
// the advice's own wording.
function adviceIsForArray(advice, disks) {
  return isArrayMember((disks || []).find((d) => d.diskId === advice.diskId));
}

// The SMART jobs of THIS disk that are still in flight. The node names a
// SMART job by the disk's kernel name (`disks::disk_name`), falling back to
// the disk id when it has none, so both are matched.
function smartJobsOf(jobs, disk) {
  return (jobs || []).filter((j) => j.kind === 'smart_test'
    && (j.status === 'running' || j.status === 'queued')
    && (j.subject === disk.name || j.subject === disk.diskId));
}

function roleChipLabel(disk) {
  // "used" is the catch-all `role_of` falls back to: not system, not in a pool
  // or array, not mounted, but carrying a filesystem signature. On a real
  // machine it covers 23 of 29 disks, and on its own ("Zajęty") it tells the
  // reader nothing they can act on — so name what occupies the disk whenever
  // the inventory knows it.
  if (!disk.memberOf) {
    const base = T('role.' + disk.role);
    return disk.role === 'used' && disk.fsType ? `${base} · ${disk.fsType}` : base;
  }
  // An Elastic Array is not a pool: it is a union mount over per-disk
  // filesystems, so it has no vdev and no RAID layout. `vdevRole`/`vdevKind`
  // therefore may NOT carry its members — `vdevRole === 'data'` would send the
  // label through `layoutLabel(vdevKind)` and print a RAID layout for a union
  // branch. The array carries its own field, and the second half names the
  // PART the disk plays, because that is what differs in what the admin does
  // next: data, cache (unprotected bytes) or parity (no data at all).
  if (disk.arrayRole) {
    // Listed, not derived: a part this build has no word for must stay
    // readable as the plain membership ("produkt · W macierzy"), and `T` of a
    // missing key answers with the key itself, which is not a label.
    const part = ARRAY_PART_KEYS[disk.arrayRole];
    return `${disk.memberOf} · ${T(part || ('role.' + disk.role))}`;
  }
  if (!disk.vdevRole) return `${disk.memberOf} · ${T('role.' + disk.role)}`;
  return `${disk.memberOf} · ${disk.vdevRole === 'data' ? layoutLabel(disk.vdevKind) : T('pool.role_' + disk.vdevRole)}`;
}

// Brings the reason chip of every KEPT row-actions element up to date. The
// element survives a poll whenever `rowActionsKey` still matches, and that key
// leaves the chip's words out on purpose (they move with every degree), so
// this is the only writer of those words after the first build. Each element
// names its disk: a kept element sits in the slot of the disk it was built
// for, because the disk id is part of the key.
function patchReasonChips(table, disks) {
  const byId = new Map((disks || []).map((d) => [d.diskId, d]));
  for (const wrap of table.shadowRoot?.querySelectorAll('.row-actions[data-disk]') || []) {
    const d = byId.get(wrap.dataset.disk);
    const chip = wrap.querySelector('[data-role="reason"]');
    const reason = d ? diskReasonChip(d) : null;
    if (!chip || !reason) continue;
    setAttr(chip, 'status', reason.status);
    setAttr(chip, 'label', reason.label);
    setAttr(chip, 'title', reason.title);
  }
}

// The n03 row's reason chip: only for a warning or a failure, and only when
// the node gave a reason (codes, or at least its sentence). The label is the
// first reason in the reader's language (`firstDiskReasonWord`), or the
// grade when this build has no word for it; the sentence is the tooltip.
function diskReasonChip(d) {
  if (d?.health !== 'warning' && d?.health !== 'critical') return null;
  const full = String(d.healthReason || '').trim();
  const coded = Array.isArray(d.healthReasons) && d.healthReasons.length > 0;
  if (!coded && !full) return null;
  return {
    status: d.health === 'critical' ? 'err' : 'warn',
    label: firstDiskReasonWord(d) ?? T('health.' + d.health),
    title: full,
  };
}

// Writes a tf-table's rows, but only when the rendered rows really differ.
// tf-table recycles its <tr> elements, yet `set rows` still re-renders every
// cell — and `_writeCell` rebuilds a chip span unconditionally. The tables on
// a disk (SMART attributes, the self-test log) move only when the node
// re-reads SMART, minutes apart, so an unchanged poll must not pay for a
// full render pass over them.
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
    disksCritical: Number(n.disksCritical) || 0,
    poolsTotal: Number(n.poolsTotal) || 0,
    arraysTotal: Number(n.arraysTotal) || 0,
    arraysUnmeasured: Number(n.arraysUnmeasured) || 0,
    sharesTotal: Number(n.sharesTotal) || 0,
    alertsActive: Number(n.alertsActive) || 0,
    capacityBytes: Number(n.capacityBytes) || 0,
    usedBytes: Number(n.usedBytes) || 0,
    ramBytes: Number(n.ramBytes) || 0,
    uptimeSecs: Number(n.uptimeSecs) || 0,
  };
}

// Per-organisation figures (shares, Elastic Arrays and the arrays' bytes) are
// NOT in the summary a node publishes: every tenant of every node reads that
// row, so fleet.rs publishes them as 0 and fills them in only on the row of
// the node that answered the list, with the asking organisation's own
// figures — and says so with `perOrgCounted` (`scope_local_org_figures`),
// which is false when either scoped read failed. `isLocal` alone could not
// tell a real 0 from a read that gave up. Wherever it is not set, those
// zeros mean "not counted", never "none" — the local row included.
function perOrgCounted(n) {
  return Boolean(n && n.isLocal && n.perOrgCounted === true);
}

// The organisation's shares on one node, as far as this screen knows them:
// the local row's scoped figure (its share errors are already in its
// `health`), or, for a remote node, the share list that node answered itself
// for this organisation on the fleet poll. Null when nobody counted them.
function nodeShares(n, fleetRows) {
  if (!n) return null;
  if (perOrgCounted(n)) return { total: n.sharesTotal, errors: 0 };
  const row = (fleetRows || []).find((r) => r.node && r.node.nodeId === n.nodeId);
  const list = row && row.shares && typeof row.shares === 'object' ? row.shares.shares : null;
  if (!Array.isArray(list)) return null;
  return { total: list.length, errors: list.filter((s) => s.state === 'error').length };
}

// The node card's grade: a remote node's own broken share makes it a warning,
// as fleet.rs does for the local row (`add_own_shares`).
function nodeCardHealth(n, shares) {
  return shares && shares.errors > 0 && n.health === 'ok' ? 'warning' : n.health;
}

// "—" for a figure nobody counted, with the reason as its tooltip.
function notCountedHtml(hint) {
  return `<span class="text-3" data-not-counted title="${escapeAttr(hint)}">—</span>`;
}

// The Pools and Shares badges of the tab strip for one node. The Pools count
// is ZFS pools plus Elastic Arrays, so on a remote row (arrays not counted)
// any number would be the ZFS half passed off as the whole.
function nodeTabCounts(n, fleetRows) {
  const shares = nodeShares(n, fleetRows);
  const counted = perOrgCounted(n);
  return {
    pools: counted ? String((Number(n.poolsTotal) || 0) + (Number(n.arraysTotal) || 0)) : '—',
    poolsTitle: counted ? null : T('fleet.arrays_not_counted_hint'),
    shares: shares ? String(shares.total) : '—',
    sharesTitle: shares ? null : T('fleet.not_counted_hint'),
  };
}

export default TentaNasScreen;
