// =============================================================================
// Plik: modules/profile-report.js
// Opis: Drill-down view raportu profilowania Nsight Systems. Topbar (back +
//       label + scope + akcje download/delete), header meta (hostname, czas,
//       nsys version, GPU targets), 6 KPI tile'ow, 7 tabsow (Overview, GPU
//       Kernels, CUDA APIs, Memory, CPU, NVTX, Timeline). Timeline rysowany
//       jako vanilla SVG line chart per GPU (SM%, Memory%, Power% znormalizowane).
//       UI wylacznie na komponentach tf-* (zero raw <button>/<input>).
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { nsightReport, nsightDelete, nsightDownload } from '/js/protocol/nsight.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-window.js';

// ---- Stan modulu ----------------------------------------------------------
//
// Trzymamy biezacy raport tylko podczas zycia widoku — Router/cleanup czysci
// wszystko przy odejsciu. Brak interwalow (raport nie odswieza sie w czasie).
let currentNodeId = null;
let currentSessionId = null;
let report = null;
let activeTab = 'overview';
let rootEl = null;
// Snapshot uzywany przez goBackToMesh po cleanup() — currentNodeId moze byc
// wtedy juz wyzerowane, a nadal potrzebujemy wiedziec do ktorego noda wracac.
let _backNodeId = null;

const ProfileReportScreen = {
  title: 'Profile Report',

  async show({ nodeId, sessionId } = {}) {
    if (!nodeId || !sessionId) {
      toast(I18n.t('nsight.report.error.load'), 'error');
      return;
    }
    currentNodeId = nodeId;
    currentSessionId = sessionId;
    _backNodeId = nodeId;
    report = null;
    activeTab = 'overview';

    const content = document.getElementById('main');
    if (!content) return;
    content.innerHTML = renderSkeleton();
    rootEl = content.querySelector('.profile-report');
    bindTopbar();

    try {
      const resp = await nsightReport({ nodeId, sessionId });
      report = resp?.report || null;
      if (!report) throw new Error('empty report');
      renderAll();
    } catch (err) {
      content.innerHTML = renderError(err);
      rootEl = content.querySelector('.profile-report');
      bindTopbar();
    }
  },

  cleanup() {
    currentNodeId = null;
    currentSessionId = null;
    report = null;
    activeTab = 'overview';
    rootEl = null;
  },
};

export default ProfileReportScreen;

// ---- Skeleton + error -----------------------------------------------------

function renderSkeleton() {
  return `
    <div class="profile-report">
      <div class="profile-report-topbar">
        <tf-button variant="ghost" size="sm" data-action="back">← ${escapeHtml(I18n.t('nsight.report.back'))}</tf-button>
        <div class="profile-report-title"><span class="skeleton" style="display:inline-block;width:240px;height:24px;"></span></div>
      </div>
      <div class="profile-report-skeleton">
        <div class="skeleton" style="width:100%;height:80px;"></div>
        <div class="skeleton" style="width:100%;height:120px;"></div>
        <div class="skeleton" style="width:100%;height:300px;"></div>
      </div>
    </div>
  `;
}

function renderError(err) {
  const msg = err?.message || String(err || '');
  return `
    <div class="profile-report">
      <div class="profile-report-topbar">
        <tf-button variant="ghost" size="sm" data-action="back">← ${escapeHtml(I18n.t('nsight.report.back'))}</tf-button>
      </div>
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(I18n.t('nsight.report.error.load'))}</div>
        <div class="muted" style="margin-top:8px;font-family:monospace;">${escapeHtml(msg)}</div>
      </div>
    </div>
  `;
}

// ---- Master render --------------------------------------------------------

function renderAll() {
  const content = document.getElementById('main');
  if (!content) return;
  content.innerHTML = `
    <div class="profile-report">
      ${renderTopbar()}
      ${renderHeaderMeta()}
      ${renderKpiGrid()}
      ${renderTabsBar()}
      <div class="profile-report-tab-body" id="profile-report-tab-body"></div>
    </div>
  `;
  rootEl = content.querySelector('.profile-report');
  bindTopbar();
  bindTabs();
  renderActiveTabBody();
}

// ---- Topbar ---------------------------------------------------------------

function renderTopbar() {
  const meta = report.meta || {};
  const scopeLabel = formatScopeForDisplay(meta.scope);
  return `
    <div class="profile-report-topbar">
      <tf-button variant="ghost" size="sm" data-action="back">← ${escapeHtml(I18n.t('nsight.report.back'))}</tf-button>
      <h1 class="profile-report-title">${escapeHtml(meta.label || '—')}</h1>
      <tf-chip status="info">${escapeHtml(scopeLabel)}</tf-chip>
      <span class="profile-report-actions">
        <tf-button size="sm" variant="ghost" data-action="download">
          ${escapeHtml(I18n.t('nsight.report.action.download'))}
        </tf-button>
        <tf-button size="sm" variant="danger" data-action="delete">
          ${escapeHtml(I18n.t('nsight.report.action.delete'))}
        </tf-button>
      </span>
    </div>
  `;
}

function bindTopbar() {
  if (!rootEl || rootEl.__topbarBound) return;
  rootEl.__topbarBound = true;
  rootEl.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn || btn.hasAttribute('disabled')) return;
    const action = btn.dataset.action;
    if (action === 'back') {
      goBackToMesh();
      return;
    }
    if (action === 'download') {
      await handleDownload();
      return;
    }
    if (action === 'delete') {
      await handleDelete();
      return;
    }
  });
}

async function goBackToMesh() {
  // Powrot do widoku noda mesh — uzywamy bezposredniego importu jak istniejacy
  // wzorzec w mesh-detail.js (mesh-detail nie jest zarejestrowany w Routerze).
  ProfileReportScreen.cleanup();
  const { default: MeshDetailScreen } = await import('/js/modules/mesh-detail.js');
  await MeshDetailScreen.show(currentNodeIdSnapshot());
}

let _backNodeId = null;
function currentNodeIdSnapshot() {
  // currentNodeId moze byc juz wyzerowane przez cleanup() — bierz snapshot.
  return _backNodeId;
}

async function handleDownload() {
  try {
    const resp = await nsightDownload({ nodeId: currentNodeId, sessionId: currentSessionId });
    const bytes = resp?.bytes;
    const filename = resp?.filename || `${currentSessionId}.nsys-rep`;
    if (!bytes || !(bytes.byteLength || bytes.length)) {
      throw new Error('empty payload');
    }
    downloadBytes(bytes, filename);
  } catch (err) {
    toast(`${I18n.t('nsight.report.error.download')}: ${err.message || err}`, 'error');
  }
}

async function handleDelete() {
  const ok = await TfWindow.confirm({
    title: I18n.t('nsight.report.action.delete'),
    message: I18n.t('nsight.report.confirm_delete'),
    confirmLabel: I18n.t('nsight.report.action.delete'),
    cancelLabel: I18n.t('nsight.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    await nsightDelete({ nodeId: currentNodeId, sessionId: currentSessionId });
    goBackToMesh();
  } catch (err) {
    toast(`${I18n.t('nsight.report.error.delete')}: ${err.message || err}`, 'error');
  }
}

// Inline downloader — projekt nie ma wspolnego helpera (audit/meeting tworza
// Blob+URL ad-hoc), wiec trzymamy implementacje lokalnie zamiast dodawac
// niepotrzebne API w utils.js.
function downloadBytes(bytes, filename) {
  const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const blob = new Blob([u8], { type: 'application/octet-stream' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // setTimeout daje przegladarce szanse rozpoczac pobieranie zanim revoke'niemy URL.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// ---- Header meta ----------------------------------------------------------

function renderHeaderMeta() {
  const meta = report.meta || {};
  const items = [];
  if (meta.hostname) {
    items.push(metaItem(I18n.t('nsight.report.meta.hostname'), meta.hostname));
  }
  if (meta.startedAtMs) {
    items.push(metaItem(I18n.t('nsight.report.meta.started'), formatDateTime(meta.startedAtMs)));
  }
  if (typeof meta.durationMs === 'number' && meta.durationMs > 0) {
    items.push(metaItem(I18n.t('nsight.report.meta.duration'), formatMillis(meta.durationMs)));
  }
  if (meta.nsysVersion) {
    items.push(metaItem(I18n.t('nsight.report.meta.nsys_version'), meta.nsysVersion));
  }
  const targets = Array.isArray(meta.gpuTargets) ? meta.gpuTargets : [];
  if (targets.length > 0) {
    const txt = targets.map((t) => `GPU ${t.idx}: ${t.name || ''}`.trim()).join(', ');
    items.push(metaItem(I18n.t('nsight.report.meta.gpu_targets'), txt));
  }
  return `
    <div class="profile-report-meta">
      ${items.join('')}
    </div>
  `;
}

function metaItem(label, value) {
  return `
    <div class="profile-report-meta-item">
      <span class="profile-report-meta-label">${escapeHtml(label)}</span>
      <span class="profile-report-meta-value">${escapeHtml(value)}</span>
    </div>
  `;
}

// ---- KPI grid -------------------------------------------------------------

function renderKpiGrid() {
  const k = report.kpi || {};
  const tiles = [
    kpiTile(I18n.t('nsight.report.kpi.gpu_active'), formatMillisAsSec(k.totalGpuActiveMs)),
    kpiTile(I18n.t('nsight.report.kpi.cpu_active'), formatMillisAsSec(k.totalCpuActiveMs)),
    kpiTile(I18n.t('nsight.report.kpi.kernel_count'), formatInt(k.kernelCount)),
    kpiTile(I18n.t('nsight.report.kpi.cuda_api_count'), formatInt(k.cudaApiCount)),
    kpiTile(I18n.t('nsight.report.kpi.peak_vram'), formatVramMb(k.peakVramMb)),
    kpiTile(I18n.t('nsight.report.kpi.samples'), formatInt(k.samplesCollected)),
  ];
  return `<div class="profile-report-kpi-grid">${tiles.join('')}</div>`;
}

function kpiTile(label, value) {
  return `
    <div class="kpi-tile">
      <div class="kpi-label">${escapeHtml(label)}</div>
      <div class="kpi-value">${escapeHtml(value)}</div>
    </div>
  `;
}

// ---- Tabs -----------------------------------------------------------------

function renderTabsBar() {
  const tabs = [
    { id: 'overview', label: I18n.t('nsight.report.tab.overview') },
    { id: 'kernels', label: I18n.t('nsight.report.tab.kernels'), count: lengthOf(report.gpuKernelsTop) },
    { id: 'cuda', label: I18n.t('nsight.report.tab.cuda'), count: lengthOf(report.cudaApiTop) },
    { id: 'memory', label: I18n.t('nsight.report.tab.memory'), count: lengthOf(report.gpuMemOps) },
    { id: 'cpu', label: I18n.t('nsight.report.tab.cpu'), count: lengthOf(report.cpuSamplesTop) },
    { id: 'nvtx', label: I18n.t('nsight.report.tab.nvtx'), count: lengthOf(report.nvtxRangesTop) },
    { id: 'timeline', label: I18n.t('nsight.report.tab.timeline'), count: lengthOf(report.gpuUtilTimeline) },
  ];
  return `
    <tf-tabs variant="underline" value="${escapeAttr(activeTab)}" id="profile-report-tabs">
      ${tabs.map((t) => {
        const countAttr = (t.count != null && t.count > 0) ? ` count="${escapeAttr(t.count)}"` : '';
        return `<tf-tab id="${escapeAttr(t.id)}"${countAttr}>${escapeHtml(t.label)}</tf-tab>`;
      }).join('')}
    </tf-tabs>
  `;
}

function bindTabs() {
  const tabsEl = rootEl?.querySelector('#profile-report-tabs');
  if (!tabsEl) return;
  tabsEl.addEventListener('change', (e) => {
    const id = e.detail?.value;
    if (!id || id === activeTab) return;
    activeTab = id;
    renderActiveTabBody();
  });
}

function renderActiveTabBody() {
  const host = document.getElementById('profile-report-tab-body');
  if (!host) return;
  let html = '';
  switch (activeTab) {
    case 'overview': html = renderOverviewTab(); break;
    case 'kernels': html = renderTopRowsTabHtml(report.gpuKernelsTop); break;
    case 'cuda':    html = renderTopRowsTabHtml(report.cudaApiTop); break;
    case 'memory':  html = renderTopRowsTabHtml(report.gpuMemOps); break;
    case 'cpu':     html = renderTopRowsTabHtml(report.cpuSamplesTop, I18n.t('nsight.report.empty.cpu_samples')); break;
    case 'nvtx':    html = renderTopRowsTabHtml(report.nvtxRangesTop, I18n.t('nsight.report.empty.nvtx')); break;
    case 'timeline': html = renderTimelineTab(); break;
    default: html = '';
  }
  host.innerHTML = html;
  if (activeTab === 'kernels' || activeTab === 'cuda' || activeTab === 'memory' || activeTab === 'cpu' || activeTab === 'nvtx') {
    hydrateTopRowsTable(host);
  }
}

// ---- Overview tab ---------------------------------------------------------

function renderOverviewTab() {
  const sections = [
    { title: I18n.t('nsight.report.tab.kernels'), rows: report.gpuKernelsTop },
    { title: I18n.t('nsight.report.tab.cuda'), rows: report.cudaApiTop },
    { title: I18n.t('nsight.report.tab.memory'), rows: report.gpuMemOps },
    { title: I18n.t('nsight.report.tab.cpu'), rows: report.cpuSamplesTop },
    { title: I18n.t('nsight.report.tab.nvtx'), rows: report.nvtxRangesTop },
  ];
  const cards = sections.map((s) => renderOverviewCard(s.title, s.rows)).join('');
  return `<div class="profile-report-overview-grid">${cards}</div>`;
}

function renderOverviewCard(title, rowsRaw) {
  const rows = Array.isArray(rowsRaw) ? rowsRaw.slice(0, 5) : [];
  if (rows.length === 0) {
    return `
      <div class="profile-report-overview-card">
        <h3>${escapeHtml(title)}</h3>
        <div class="empty muted">—</div>
      </div>
    `;
  }
  const items = rows.map((r) => `
    <div class="overview-row">
      <span class="overview-row-name" title="${escapeAttr(r.name || '')}">${escapeHtml(r.name || '—')}</span>
      <span class="overview-row-pct">${formatPct(r.pct)}</span>
    </div>
  `).join('');
  return `
    <div class="profile-report-overview-card">
      <h3>${escapeHtml(title)}</h3>
      ${items}
    </div>
  `;
}

// ---- Top rows table tab ---------------------------------------------------

function renderTopRowsTabHtml(rowsRaw, emptyText) {
  const rows = Array.isArray(rowsRaw) ? rowsRaw : [];
  if (rows.length === 0) {
    return `<div class="empty-state"><div class="empty-state-text">${escapeHtml(emptyText || '—')}</div></div>`;
  }
  return `<tf-table sortable id="profile-report-top-rows">
    <tf-column key="name" label="${escapeAttr(I18n.t('nsight.report.col.name'))}" sortable></tf-column>
    <tf-column key="totalMs" label="${escapeAttr(I18n.t('nsight.report.col.total_ms'))}" renderer="num" sortable></tf-column>
    <tf-column key="calls" label="${escapeAttr(I18n.t('nsight.report.col.calls'))}" renderer="num" sortable></tf-column>
    <tf-column key="avgMs" label="${escapeAttr(I18n.t('nsight.report.col.avg_ms'))}" renderer="num" sortable></tf-column>
    <tf-column key="pctHtml" label="${escapeAttr(I18n.t('nsight.report.col.pct'))}" renderer="html" sortable></tf-column>
  </tf-table>`;
}

function hydrateTopRowsTable(host) {
  const table = host.querySelector('tf-table#profile-report-top-rows');
  if (!table) return;
  const sourceRaw = topRowsSourceForActiveTab();
  const source = Array.isArray(sourceRaw) ? sourceRaw : [];
  const rows = source.map((r) => ({
    name: r.name || '—',
    totalMs: roundTo(r.totalMs, 3),
    calls: Number(r.calls) || 0,
    avgMs: roundTo(r.avgMs, 3),
    pctHtml: pctBarHtml(r.pct),
  }));
  // tf-table buduje sie w connectedCallback; jesli host wlasnie wstawiony, czekamy na frame.
  if (table._tbody) table.rows = rows;
  else requestAnimationFrame(() => { table.rows = rows; });
}

function topRowsSourceForActiveTab() {
  switch (activeTab) {
    case 'kernels': return report.gpuKernelsTop;
    case 'cuda':    return report.cudaApiTop;
    case 'memory':  return report.gpuMemOps;
    case 'cpu':     return report.cpuSamplesTop;
    case 'nvtx':    return report.nvtxRangesTop;
    default: return [];
  }
}

function pctBarHtml(pct) {
  const v = Number.isFinite(pct) ? Math.max(0, Math.min(100, pct)) : 0;
  return `<div class="bar"><div class="bar-fill" style="width:${v.toFixed(1)}%"></div><span class="bar-text">${v.toFixed(1)}%</span></div>`;
}

// ---- Timeline tab (vanilla SVG) -------------------------------------------

function renderTimelineTab() {
  const series = Array.isArray(report.gpuUtilTimeline) ? report.gpuUtilTimeline : [];
  if (series.length === 0) {
    return `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('nsight.report.empty.timeline'))}</div></div>`;
  }
  return `<div class="profile-report-timeline">
    ${series.map((s) => renderTimelineCard(s)).join('')}
  </div>`;
}

function renderTimelineCard(s) {
  const samples = Array.isArray(s.samples) ? s.samples : [];
  const idx = Number.isFinite(s.gpuIdx) ? s.gpuIdx : 0;
  const powerLimit = Number.isFinite(s.powerLimitW) ? s.powerLimitW : 0;
  const subtitle = powerLimit > 0
    ? `Power limit: ${powerLimit.toFixed(0)} W`
    : '';
  if (samples.length < 2) {
    return `
      <div class="timeline-card">
        <div class="timeline-card-head">
          <h3>GPU ${idx}</h3>
          ${subtitle ? `<span class="muted">${escapeHtml(subtitle)}</span>` : ''}
        </div>
        <div class="empty muted" style="padding:24px;">${escapeHtml(I18n.t('nsight.report.empty.timeline'))}</div>
      </div>
    `;
  }
  const svg = renderTimelineSvg(samples, powerLimit);
  return `
    <div class="timeline-card">
      <div class="timeline-card-head">
        <h3>GPU ${idx}</h3>
        ${subtitle ? `<span class="muted">${escapeHtml(subtitle)}</span>` : ''}
      </div>
      <div class="timeline-legend">
        <span class="legend-item"><span class="legend-swatch sm"></span>${escapeHtml(I18n.t('nsight.report.timeline.legend.sm'))}</span>
        <span class="legend-item"><span class="legend-swatch mem"></span>${escapeHtml(I18n.t('nsight.report.timeline.legend.mem'))}</span>
        <span class="legend-item"><span class="legend-swatch power"></span>${escapeHtml(I18n.t('nsight.report.timeline.legend.power'))}</span>
      </div>
      ${svg}
    </div>
  `;
}

// Generuje SVG line chart. Wszystkie 3 serie znormalizowane do 0..100% ->
// jeden wspolny Y axis. Power% liczone jako (power_w / power_limit_w) * 100;
// jesli power_limit_w == 0, pomijamy serie power.
function renderTimelineSvg(samples, powerLimitW) {
  const W = 1000;
  const H = 220;
  const padL = 44;
  const padR = 16;
  const padT = 12;
  const padB = 28;
  const innerW = W - padL - padR;
  const innerH = H - padT - padB;

  const t0 = samples[0].tMs;
  const tN = samples[samples.length - 1].tMs;
  const dt = Math.max(1, tN - t0);
  const x = (tMs) => padL + ((tMs - t0) / dt) * innerW;
  const y = (pct) => padT + (1 - Math.max(0, Math.min(100, pct)) / 100) * innerH;

  // Gridlines + Y ticks (0, 25, 50, 75, 100).
  const grid = [0, 25, 50, 75, 100].map((p) => {
    const yy = y(p);
    return `<line class="grid" x1="${padL}" y1="${yy}" x2="${W - padR}" y2="${yy}"/><text class="axis-label" x="${padL - 8}" y="${yy + 4}" text-anchor="end">${p}%</text>`;
  }).join('');

  // X ticks — sekundy. Maksymalnie ~6 etykiet zeby nie zaspamowac.
  const totalSec = dt / 1000;
  const desired = 6;
  const stepSec = niceStep(totalSec / desired);
  const xTicks = [];
  for (let s = 0; s <= totalSec + 1e-6; s += stepSec) {
    const tMs = t0 + s * 1000;
    const xx = x(tMs);
    xTicks.push(`<line class="grid" x1="${xx}" y1="${padT}" x2="${xx}" y2="${H - padB}" stroke-opacity="0.25"/><text class="axis-label" x="${xx}" y="${H - padB + 16}" text-anchor="middle">${s.toFixed(stepSec < 1 ? 1 : 0)}s</text>`);
  }

  // Polylines. Power tylko gdy mamy power_limit_w > 0.
  const smPoints = samples.map((s) => `${x(s.tMs).toFixed(1)},${y(s.smPct).toFixed(1)}`).join(' ');
  const memPoints = samples.map((s) => `${x(s.tMs).toFixed(1)},${y(s.memPct).toFixed(1)}`).join(' ');
  let powerLine = '';
  if (powerLimitW > 0) {
    const powerPoints = samples.map((s) => {
      const pct = (s.powerW / powerLimitW) * 100;
      return `${x(s.tMs).toFixed(1)},${y(pct).toFixed(1)}`;
    }).join(' ');
    powerLine = `<polyline class="line line-power" points="${powerPoints}" fill="none"/>`;
  }

  return `
    <svg class="timeline-svg" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" role="img">
      <rect class="plot-bg" x="${padL}" y="${padT}" width="${innerW}" height="${innerH}"/>
      ${grid}
      ${xTicks.join('')}
      <polyline class="line line-sm" points="${smPoints}" fill="none"/>
      <polyline class="line line-mem" points="${memPoints}" fill="none"/>
      ${powerLine}
    </svg>
  `;
}

// ---- Helpers --------------------------------------------------------------

function lengthOf(arr) {
  return Array.isArray(arr) ? arr.length : 0;
}

function formatScopeForDisplay(scope) {
  if (typeof scope === 'string') {
    if (scope === 'Cpu') return 'CPU';
    if (scope === 'GpuAll') return 'GPU all';
    if (scope === 'BothAll') return 'CPU + GPU all';
    return scope;
  }
  if (scope && typeof scope === 'object') {
    if (scope.kind === 'gpu_index' || scope.kind === 'GpuIndex') return `GPU ${scope.idx}`;
    if (scope.kind === 'both_index' || scope.kind === 'BothIndex') return `CPU + GPU ${scope.idx}`;
  }
  return '—';
}

function formatDateTime(epochMs) {
  if (!epochMs) return '—';
  const d = new Date(Number(epochMs));
  if (Number.isNaN(d.getTime())) return '—';
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function formatMillis(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(2)} s`;
  const m = Math.floor(sec / 60);
  const s = Math.round(sec % 60);
  return `${m}m ${s}s`;
}

function formatMillisAsSec(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '0 s';
  return `${(ms / 1000).toFixed(2)} s`;
}

function formatInt(n) {
  if (!Number.isFinite(n) || n < 0) return '0';
  return Math.round(n).toLocaleString('en-US');
}

function formatVramMb(mb) {
  if (!Number.isFinite(mb) || mb <= 0) return '0 MB';
  if (mb >= 1024) return `${(mb / 1024).toFixed(2)} GB`;
  return `${Math.round(mb)} MB`;
}

function formatPct(pct) {
  if (!Number.isFinite(pct)) return '0.0%';
  return `${pct.toFixed(1)}%`;
}

function roundTo(v, decimals) {
  if (!Number.isFinite(v)) return 0;
  const m = Math.pow(10, decimals);
  return Math.round(v * m) / m;
}

// Wybiera "ladne" odstepy do osi X (1, 2, 5, 10, 20, 50 itp.).
function niceStep(raw) {
  if (!Number.isFinite(raw) || raw <= 0) return 1;
  const exp = Math.floor(Math.log10(raw));
  const base = Math.pow(10, exp);
  const norm = raw / base;
  let nice;
  if (norm < 1.5) nice = 1;
  else if (norm < 3) nice = 2;
  else if (norm < 7) nice = 5;
  else nice = 10;
  return nice * base;
}

