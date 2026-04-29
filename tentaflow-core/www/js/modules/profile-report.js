// =============================================================================
// Plik: modules/profile-report.js
// Opis: Ekran raportu profilowania (multi-source). Renderuje raport
//       sesji z dynamicznymi zakladkami (Overview / Unified Timeline /
//       CPU Flamegraph / GPU per-vendor / Memory / Disk / Power / Sources).
//       Zakladki sa ukrywane gdy ich kategorii zdarzen nie ma w raporcie.
// =============================================================================

import {
  expandCompactSeries,
  groupEventsByCategory,
  eventsForCategory,
  eventsForDevice,
  uniqueDevices,
  unwrapPayload,
  computeAllKpis,
  buildQuickFindings,
  buildTimeSeries,
  renderLineChart,
  renderAreaChart,
  renderStackedArea,
  renderRidgelinePreview,
  formatBytes,
  formatBytesPerSec,
  formatNs,
  formatDurationNs,
  formatPower,
  formatPct,
  formatInt,
  formatDateTime,
  relativeTime,
  escape,
  detectVendor,
  vendorBadge,
  normalizeCollectorStatus,
  countUsedCollectors,
} from '/js/modules/profile-report-helpers.js';
import {
  profilingReport,
  profilingDownload,
} from '/js/protocol/profiling.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-window.js';

// Krotki helper i18n z fallbackiem.
function t(key, vars, fallback) {
  const v = I18n.t(key, vars || null);
  return v === key && fallback != null ? fallback : v;
}

const FIXTURE_PATH = '/js/modules/__fixtures__/profile-report.json';

// wasm-bindgen / decode helpery zwracaja klucze w camelCase (schemaVersion,
// sessionId, ...), a reszta tego pliku byla pisana pod snake_case zgodny z
// rust polami. Konwertujemy raz przy boundary, zeby nie przepisywac 39 odwolan.
function camelToSnake(name) {
  return name.replace(/[A-Z]/g, (m) => '_' + m.toLowerCase());
}
function deepSnakeKeys(value) {
  if (Array.isArray(value)) return value.map(deepSnakeKeys);
  if (value && typeof value === 'object' && value.constructor === Object) {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      out[camelToSnake(k)] = deepSnakeKeys(v);
    }
    return out;
  }
  return value;
}

function fixtureMode() {
  return typeof window !== 'undefined' && window.__TF_PROFILING_FIXTURE === true;
}

// Loads the report. In fixture mode reads the static JSON from the asset path;
// otherwise hits the dashboard API. The single call site is `loadReport` so a
// future swap to streaming/cached delivery is local.
async function loadReport({ nodeId, sessionId }) {
  if (fixtureMode()) {
    const resp = await fetch(FIXTURE_PATH, { headers: { accept: 'application/json' } });
    if (!resp.ok) throw new Error(`fixture load failed: HTTP ${resp.status}`);
    return await resp.json();
  }
  return profilingReport({ nodeId: nodeId || '', sessionId });
}

// =============================================================================
// Public API.
// =============================================================================

export class ProfileReportView {
  static async render(container, { sessionId, nodeId } = {}) {
    if (!container) throw new Error('container is required');
    if (!sessionId) throw new Error('sessionId is required');
    container.innerHTML = renderSkeleton();

    let raw;
    try {
      raw = await loadReport({ nodeId, sessionId });
    } catch (err) {
      container.innerHTML = renderError(err);
      bindBackHandler(container);
      return;
    }

    // Wyodrebnienie raportu z rkyv envelope. Binary protocol zwraca enum
    // jako { V2: {...} }; fixture JSON moze nosic { envelope: { kind, report } }.
    // Legacy V1 nie jest juz wspierany — zwracamy explicit error zamiast renderowac.
    let report = raw;
    let envelopeIsV2 = false;
    if (raw && typeof raw === 'object') {
      if (raw.envelope && typeof raw.envelope === 'object') {
        if (raw.envelope.kind === 'v2' && raw.envelope.report) {
          envelopeIsV2 = true;
          report = raw.envelope.report;
        }
      } else if ('V2' in raw && raw.V2) {
        envelopeIsV2 = true;
        report = raw.V2;
      } else if (raw.envelope && Array.isArray(raw.envelope)) {
        // Niektore deserializery zwracaja enum jako tagged tuple [tag, payload].
        const [tag, payload] = raw.envelope;
        if (tag === 'V2' && payload) { envelopeIsV2 = true; report = payload; }
      }
    }
    if (!report) {
      container.innerHTML = renderError(new Error(t('profiling.report.err_empty_report', null, 'Empty report received from backend')));
      bindBackHandler(container);
      return;
    }
    report = deepSnakeKeys(report);
    if (report.schema_version !== 2) {
      const ver = report.schema_version || 'brak';
      const msg = envelopeIsV2
        ? t('profiling.report.err_unknown_schema_v2', { ver }, `Backend returned a V2 envelope with schema_version=${ver} which the dashboard does not understand. Update the web client.`)
        : t('profiling.report.err_unknown_schema', { ver }, `Unknown report format (schema_version=${ver}). Check tentaflow versions.`);
      container.innerHTML = renderError(new Error(msg));
      bindBackHandler(container);
      return;
    }

    const expanded = expandCompactSeries(report);
    const ctx = buildContext(expanded);
    container.innerHTML = renderShell(ctx);
    bindShell(container, ctx);
    renderTab(container, ctx, ctx.defaultTab);
  }
}

export default ProfileReportView;

// =============================================================================
// Context — derived once per render, carries grouped events + tab visibility.
// =============================================================================

function buildContext(report) {
  const events = report.events || [];
  const grouped = groupEventsByCategory(events);

  // Devices come from any payload that carries device_id (GpuUtilSample first,
  // then GpuKernel as fallback).
  let deviceIds = uniqueDevices(eventsForCategory(events, 'GpuUtilSample'));
  if (deviceIds.length === 0) deviceIds = uniqueDevices(eventsForCategory(events, 'GpuKernel'));
  // Hydrate device info from compact series when present (vendor / name / version).
  const deviceMeta = new Map();
  const cs = report._compact_series || {};
  for (const d of cs.gpu_devices || []) {
    deviceMeta.set(d.device_id, {
      device_id: d.device_id,
      vendor: (d.vendor || '').toLowerCase() || detectVendor(d.name, d.collector),
      name: d.name || `GPU ${d.device_id}`,
      version: d.version || '',
      collector: d.collector || '',
      limited: !!d.limited,
      memTotalBytes: d.mem_total_bytes || 0,
      transfers: d.transfers || null,
      hasKernels: Array.isArray(d.kernels) && d.kernels.length > 0,
      hasApis: Array.isArray(d.apis) && d.apis.length > 0,
    });
  }
  for (const id of deviceIds) {
    if (!deviceMeta.has(id)) {
      deviceMeta.set(id, { device_id: id, vendor: 'nvidia', name: `GPU ${id}`, version: '', collector: '', limited: false, memTotalBytes: 0, transfers: null, hasKernels: false, hasApis: false });
    }
  }
  const devices = Array.from(deviceMeta.values()).sort((a, b) => a.device_id - b.device_id);

  const hasGpu = grouped.has('GpuUtilSample') || grouped.has('GpuKernel');
  const hasDisk = grouped.has('DiskIoBurst');
  const hasPower = grouped.has('PowerSample');
  const hasMemory = grouped.has('RamSample');
  const hasNetwork = grouped.has('NetworkSample');

  // Mockup #04: 9-tab layout. CPU Detail (#07) wstawiony miedzy Flamegraph
  // a GPU; widoczny zawsze gdy mamy CpuUtil events (a te zbiera kazdy
  // backend cpu_util collector niezalezne od OS).
  const hasCpuUtil = (grouped.get('CpuUtil') || []).length > 0;
  // Sub-tab icons match mockup #04 (linie 1107-1115). Komplet symboli zywie
  // w www/index.html jako <symbol id="i-...">.
  const tabs = [
    { id: 'overview',   label: t('profiling.report.tab_overview', null, 'Overview'),                 icon: 'grid-2x2',   visible: true },
    { id: 'timeline',   label: t('profiling.report.tab_timeline', null, 'Unified Timeline'),         icon: 'line-chart', visible: true },
    { id: 'flame',      label: t('profiling.report.tab_flame', null, 'CPU Flamegraph'),              icon: 'bar-chart',  visible: true },
    { id: 'cpu_detail', label: t('profiling.report.tab_cpu_detail', null, 'CPU Detail'),             icon: 'cpu',        visible: hasCpuUtil },
    { id: 'gpu',        label: t('profiling.report.tab_gpu', null, 'GPU'),                            icon: 'globe-grid', visible: hasGpu, count: devices.length || undefined },
    { id: 'memory',     label: t('profiling.report.tab_memory', null, 'Memory'),                      icon: 'ram',        visible: hasMemory },
    { id: 'disk',       label: t('profiling.report.tab_disk', null, 'Disk IO'),                       icon: 'cylinder',   visible: hasDisk },
    { id: 'power',      label: t('profiling.report.tab_power', null, 'Power'),                        icon: 'shield',     visible: hasPower },
    { id: 'sources',    label: t('profiling.report.tab_sources', null, 'Sources'),                    icon: 'list',       visible: true },
  ].filter((tt) => tt.visible);

  // Single-pass aggregation across all events. Powers Overview + GPU tabs
  // without re-walking events for each KPI.
  const kpis = computeAllKpis(events, report.names || [], report.duration_ns);

  return {
    report,
    events,
    grouped,
    devices,
    tabs,
    defaultTab: 'overview',
    counts: countUsedCollectors(report.collectors || []),
    hasGpu, hasDisk, hasPower, hasMemory, hasNetwork,
    kpis,
    // Lazy cache for rendered tab HTML — see renderTab. Most tabs are pure
    // functions of ctx so we can cache the string and skip the work on revisit.
    _tabHtml: new Map(),
  };
}

// =============================================================================
// Shell (header + tabs strip + body container).
// =============================================================================

function renderShell(ctx) {
  const { report, counts } = ctx;
  const startedAt = formatDateTime(report.t0_wallclock_unix_ns);
  const dur = formatDurationNs(report.duration_ns);
  const scopeLabel = escape(report.scope?.label || '—');
  const sessionShort = escape((report.session_id || '').slice(0, 8));
  const node = escape(report.node_id || '—');
  const sourcesPill = t('profiling.report.header_sources_used', { used: counts.used, total: counts.total }, `${counts.used}/${counts.total} sources used`);
  const sourcesPillCls = counts.skipped > 0 || counts.failed > 0 ? 'warn' : 'ok';

  // Mockup #04: jedna linia mono z czterema segmentami. Bez relative time
  // (redundantny z absolute) i bez prefixu "started" — sam timestamp wystarcza.
  const headerActions = `
    <span class="pr-status-pill ${sourcesPillCls}" role="status">
      ${counts.skipped > 0 || counts.failed > 0
        ? `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 2L2 22h20L12 2z"/><path d="M12 9v6M12 18h.01"/></svg>`
        : `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M5 13l4 4L19 7"/></svg>`}
      ${escape(sourcesPill)}
    </span>
    <tf-button variant="ghost" size="sm" data-action="download" aria-label="${escape(t('profiling.report.header_aria_download', null, 'Download report'))}">${escape(t('profiling.report.header_action_download', null, 'Download'))}</tf-button>
    <tf-button variant="ghost" size="sm" data-action="compare" aria-label="${escape(t('profiling.report.header_aria_compare', null, 'Compare with another session'))}">${escape(t('profiling.report.header_action_compare', null, 'Compare'))}</tf-button>
  `;

  const tabsHtml = ctx.tabs.map((t) => {
    const countAttr = t.count ? ` count="${t.count}"` : '';
    const iconAttr = t.icon ? ` icon="${escape(t.icon)}"` : '';
    return `<tf-tab id="${escape(t.id)}"${iconAttr}${countAttr}>${escape(t.label)}</tf-tab>`;
  }).join('');

  return `
    <div class="profile-report">
      <header class="pr-header">
        <div class="pr-header-main">
          <h1 class="pr-title">${scopeLabel}</h1>
          <div class="pr-meta mono">
            ${escape(t('profiling.report.header_meta', { sid: sessionShort, started: startedAt, dur, node }, `session ${sessionShort} · ${startedAt} · ${dur} duration · ${node}`))}
          </div>
        </div>
        <div class="pr-header-actions">
          ${headerActions}
        </div>
      </header>

      <div class="pr-tabs-wrap">
        <tf-tabs variant="underline" value="${escape(ctx.defaultTab)}" id="pr-tabs">
          ${tabsHtml}
        </tf-tabs>
      </div>

      <main class="pr-body" id="pr-body" role="region" aria-label="${escape(t('profiling.report.header_aria_body', null, 'Report body'))}"></main>
    </div>
  `;
}

function renderSkeleton() {
  return `
    <div class="profile-report pr-loading">
      <div class="pr-skeleton" style="width:60%; height:24px;"></div>
      <div class="pr-skeleton" style="width:100%; height:80px; margin-top:14px;"></div>
      <div class="pr-skeleton" style="width:100%; height:200px; margin-top:14px;"></div>
    </div>
  `;
}

function renderError(err) {
  const msg = err?.message || String(err || 'Unknown error');
  // Specific case: NotFound dla summary.bin -> session crashed/in-progress.
  // Mowimy user'owi dokladnie co sie stalo, nie surowy error code.
  const isMissingSummary = /NotFound.*summary\.bin/i.test(msg);
  const friendlyHeader = isMissingSummary
    ? t('profiling.report.err_unavailable_title', null, 'Session report unavailable')
    : t('profiling.report.err_failed_title', null, 'Failed to load report');
  const friendlyBody = isMissingSummary
    ? `<p>${escape(t('profiling.report.err_missing_summary_intro', null, 'This session has no report saved on disk. Possible reasons:'))}</p>
       <ul style="margin:8px 0 8px 22px; line-height:1.6;">
         <li>${t('profiling.report.err_missing_summary_in_progress', null, 'Session is <strong>still being collected</strong> (the report is created only after Stop)')}</li>
         <li>${t('profiling.report.err_missing_summary_killed', null, 'Tentaflow process was <strong>killed before completion</strong> (kill / OOM / restart)')}</li>
         <li>${t('profiling.report.err_missing_summary_stop_failed', null, 'Stop failed with an error (check tentaflow logs)')}</li>
       </ul>
       <p style="font-size:12px; color:var(--tf-text-3, #6a7196);">${escape(t('profiling.report.err_missing_summary_hidden', null, 'After pull and restart, this session will be automatically hidden from the list (storage_v2 list_sessions filters by summary.bin).'))}</p>`
    : `<pre class="mono" style="white-space:pre-wrap; word-break:break-word;">${escape(msg)}</pre>`;
  return `
    <div class="profile-report pr-error">
      <div class="pr-error-card">
        <h2>${escape(friendlyHeader)}</h2>
        ${friendlyBody}
        <tf-button variant="ghost" size="sm" data-action="back-mesh">${escape(t('profiling.report.err_back_btn', null, 'Back to Mesh'))}</tf-button>
      </div>
    </div>
  `;
}

function bindShell(container, ctx) {
  // Header / breadcrumb actions.
  container.addEventListener('click', (e) => {
    const target = e.target.closest('[data-action]');
    if (!target) return;
    const action = target.dataset.action;
    if (action === 'back-mesh') {
      e.preventDefault();
      navigateBack();
    } else if (action === 'download') {
      handleDownload(ctx);
    } else if (action === 'compare') {
      handleCompare(ctx);
    } else if (action === 'open-timeline') {
      const tabs = container.querySelector('#pr-tabs');
      if (tabs) tabs.value = 'timeline';
    } else if (action === 'rerun') {
      handleRerun(ctx);
    }
  });

  // Tab switching.
  const tabsEl = container.querySelector('#pr-tabs');
  if (tabsEl) {
    tabsEl.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id) renderTab(container, ctx, id);
    });
  }
}

function bindBackHandler(container) {
  container.addEventListener('click', (e) => {
    const t = e.target.closest('[data-action="back-mesh"]');
    if (!t) return;
    e.preventDefault();
    navigateBack();
  });
}

function navigateBack() {
  // Use the global Router if available; otherwise fall back to history.
  if (window.Router && typeof window.Router.navigate === 'function') {
    window.Router.navigate('mesh');
    return;
  }
  history.back();
}

async function handleDownload(ctx) {
  if (fixtureMode()) {
    const blob = new Blob([JSON.stringify(ctx.report, null, 2)], { type: 'application/json' });
    triggerDownload(blob, `profile-${ctx.report.session_id}.json`);
    return;
  }
  try {
    const resp = await profilingDownload({
      nodeId: ctx.report.node_id || '',
      sessionId: ctx.report.session_id,
    });
    const bytes = resp.tarballBytes instanceof Uint8Array
      ? resp.tarballBytes
      : new Uint8Array(resp.tarballBytes || []);
    const filename = resp.filename || `profile-${ctx.report.session_id}.tar.gz`;
    const blob = new Blob([bytes], { type: 'application/gzip' });
    triggerDownload(blob, filename);
  } catch (err) {
    console.error('download failed', err);
  }
}

function triggerDownload(blob, filename) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// Compare ouverture: Router pokazuje modal wyboru drugiej sesji i po
// potwierdzeniu nawiguje na osobny widok diff. Tu tylko delegujemy.
function handleCompare(ctx) {
  if (window.Router && typeof window.Router.navigate === 'function') {
    window.Router.navigate('profile-compare', {
      nodeId: ctx.report.node_id,
      sessionA: ctx.report.session_id,
    });
  }
}

async function handleRerun(ctx) {
  // Hand off to the launch modal with skipped collectors pre-selected.
  try {
    const { ProfilingLaunchModal } = await import('/js/modules/profiling-launch.js');
    const skipped = (ctx.report.collectors || [])
      .filter((c) => normalizeCollectorStatus(c.status).kind === 'skipped')
      .map((c) => ({ id: c.id, label: c.id, description: 'Re-run with elevation', status: 'needs_sudo' }));
    if (skipped.length === 0) return;
    await ProfilingLaunchModal.open({
      nodeId: ctx.report.node_id,
      availableSources: skipped,
      onLaunched: () => navigateBack(),
    });
  } catch (err) {
    console.error('re-run failed', err);
  }
}

// =============================================================================
// Tab dispatcher.
// =============================================================================

function renderTab(container, ctx, tabId) {
  const body = container.querySelector('#pr-body');
  if (!body) return;
  // Lazy/dynamic tabs (timeline, flame) need a fresh mount because they
  // attach Canvas/SVG and event listeners; we never cache their HTML.
  if (tabId === 'timeline') { renderLazyTab(body, '/js/modules/profile-timeline.js', ctx, 'TimelineView'); return; }
  if (tabId === 'flame')    { renderLazyTab(body, '/js/modules/profile-flamegraph.js', ctx, 'FlamegraphView'); return; }
  // Pure-HTML tabs: cache the rendered string keyed by tabId. KPIs and
  // device data on `ctx` are immutable for the lifetime of a report, so
  // re-rendering on tab switch only repaints; the HTML itself is identical.
  let html = ctx._tabHtml.get(tabId);
  if (html === undefined) {
    switch (tabId) {
      case 'overview':   html = renderOverviewTab(ctx); break;
      case 'cpu_detail': html = renderCpuDetailTab(ctx); break;
      case 'gpu':        html = renderGpuTab(ctx); break;
      case 'memory':     html = renderMemoryTab(ctx); break;
      case 'disk':       html = renderDiskTab(ctx); break;
      case 'power':      html = renderPowerTab(ctx); break;
      case 'sources':    html = renderSourcesTab(ctx); break;
      default:         html = '';
    }
    ctx._tabHtml.set(tabId, html);
  }
  body.innerHTML = html;
  if (tabId === 'gpu') bindGpuTab(body, ctx);
  if (tabId === 'cpu_detail') bindCpuDetailTab(body);
}

// Lazy-load tabs (Timeline + Flamegraph) implemented by sibling agents. While
// loading we show a spinner; if the module is missing we render an info
// banner so the rest of the report stays usable.
async function renderLazyTab(host, modulePath, ctx, exportName) {
  host.innerHTML = `<div class="pr-card pr-loading-card"><div class="pr-skeleton" style="height:24px;width:50%;"></div><div class="pr-skeleton" style="height:200px;margin-top:14px;"></div></div>`;
  try {
    const mod = await import(modulePath);
    const ViewClass = mod[exportName] || mod.default;
    if (ViewClass && typeof ViewClass.render === 'function') {
      await ViewClass.render(host, ctx);
      return;
    }
    host.innerHTML = pendingModuleBanner(modulePath, t('profiling.report.tab_pending_export_missing', null, 'export missing'));
  } catch (err) {
    host.innerHTML = pendingModuleBanner(modulePath, err?.message || t('profiling.report.tab_pending_module_unavailable', null, 'module unavailable'));
  }
}

function pendingModuleBanner(modulePath, reason) {
  return `
    <div class="pr-card">
      <div class="pr-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>
          <strong>${escape(t('profiling.report.tab_pending', null, 'Tab not yet available.'))}</strong>
          ${t('profiling.report.tab_pending_body', { path: escape(modulePath), reason: escape(reason) }, `The module <code class="mono">${escape(modulePath)}</code> is being implemented separately (${escape(reason)}). Other tabs of this report are fully functional.`)}
        </div>
      </div>
    </div>
  `;
}

// =============================================================================
// Tab: Overview.
// =============================================================================

function renderOverviewTab(ctx) {
  const { events, devices, report, hasMemory, hasDisk, hasPower, hasNetwork, kpis } = ctx;
  const names = report.names || [];
  const cpu = kpis.cpu;
  const ram = kpis.ram;
  const disk = kpis.disk;
  const power = kpis.power;
  const net = kpis.net;
  const gpuKpiOf = (id) => kpis.gpu.get(id) || { peakCompute: 0, peakMem: 0, memUsedBytes: 0, topKernel: null, topPct: 0, avgW: 0, peakW: 0, kernels: [], apis: [] };

  // Mockup #04: max 4 GPU w grid 4x2. NVIDIA pokazuje "Peak SM · Top kernel ...",
  // AMD "Peak compute · Top ...", Intel/Apple bez detali kernel ("No kernel detail")
  // bo nie ma per-kernel sourcing z ich runtime'ow w aktualnej generacji.
  const gpuTiles = devices.slice(0, 4).map((d) => {
    const k = gpuKpiOf(d.device_id);
    const badge = vendorBadge(d.vendor);
    return `
      <div class="pr-kpi-tile ${badge.cls}">
        <div class="pr-kpi-head">
          <span class="pr-kpi-ico"><span class="pr-vendor-badge ${badge.cls}">${escape(badge.label)}</span></span>
          <span class="pr-kpi-label">${escape(t('profiling.report.kpi_gpu_label', { id: d.device_id, name: d.name }, `GPU ${d.device_id} — ${d.name}`))}</span>
        </div>
        <div class="pr-kpi-value">${formatPct(k.peakCompute, 0)}</div>
        <div class="pr-kpi-sub">${vendorPeakLabel(badge.cls, k)}</div>
      </div>
    `;
  }).join('');

  const cpuTile = `
    <div class="pr-kpi-tile">
      <div class="pr-kpi-head"><span class="pr-kpi-ico">${iconCpu()}</span><span class="pr-kpi-label">${escape(t('profiling.report.kpi_cpu', null, 'CPU'))}</span></div>
      <div class="pr-kpi-value">${formatPct(cpu.avgUtil, 0)}</div>
      <div class="pr-kpi-sub">${t('profiling.report.kpi_avg_util', { peak: formatPct(cpu.peakUtil, 0), top: escape(cpu.topSymbol) }, `Avg util · Peak <strong>${formatPct(cpu.peakUtil, 0)}</strong> · Top: <strong>${escape(cpu.topSymbol)}</strong>`)}</div>
    </div>
  `;
  const ramTile = hasMemory ? `
    <div class="pr-kpi-tile">
      <div class="pr-kpi-head"><span class="pr-kpi-ico">${iconRam()}</span><span class="pr-kpi-label">${escape(t('profiling.report.kpi_ram', null, 'RAM'))}</span></div>
      <div class="pr-kpi-value">${formatBytes(ram.peakUsedBytes)}</div>
      <div class="pr-kpi-sub">${t('profiling.report.kpi_ram_sub', { bw: formatBytesPerSec(ram.peakBwBps) }, `Peak used · BW peak <strong>${formatBytesPerSec(ram.peakBwBps)}</strong>`)}</div>
    </div>
  ` : '';
  const diskTile = hasDisk ? `
    <div class="pr-kpi-tile">
      <div class="pr-kpi-head"><span class="pr-kpi-ico">${iconDisk()}</span><span class="pr-kpi-label">${escape(t('profiling.report.kpi_disk', null, 'Disk'))}</span></div>
      <div class="pr-kpi-value">${formatBytesPerSec(disk.peakReadBps)}</div>
      <div class="pr-kpi-sub">${t('profiling.report.kpi_disk_sub', { w: formatBytesPerSec(disk.peakWriteBps), p99: disk.p99AwaitMs.toFixed(1) }, `Peak R · W <strong>${formatBytesPerSec(disk.peakWriteBps)}</strong> · p99 await <strong>${disk.p99AwaitMs.toFixed(1)} ms</strong>`)}</div>
    </div>
  ` : '';
  const powerTile = hasPower ? `
    <div class="pr-kpi-tile">
      <div class="pr-kpi-head"><span class="pr-kpi-ico">${iconPower()}</span><span class="pr-kpi-label">${escape(t('profiling.report.kpi_power', null, 'Power'))}</span></div>
      <div class="pr-kpi-value">${formatPower(power.avgW)}</div>
      <div class="pr-kpi-sub">${t('profiling.report.kpi_power_sub', { peak: formatPower(power.peakW), total: power.totalKj.toFixed(1) }, `Avg · Peak <strong>${formatPower(power.peakW)}</strong> · Total <strong>${power.totalKj.toFixed(1)} kJ</strong>`)}</div>
    </div>
  ` : '';
  const netTile = hasNetwork ? `
    <div class="pr-kpi-tile">
      <div class="pr-kpi-head"><span class="pr-kpi-ico">${iconNet()}</span><span class="pr-kpi-label">${escape(t('profiling.report.kpi_network', null, 'Network'))}</span></div>
      <div class="pr-kpi-value">${formatBytesPerSec(net.peakRxBps)}</div>
      <div class="pr-kpi-sub">${t('profiling.report.kpi_net_sub', { tx: formatBytesPerSec(net.peakTxBps) }, `Peak in · Peak out <strong>${formatBytesPerSec(net.peakTxBps)}</strong>`)}</div>
    </div>
  ` : '';
  // Mockup #04 trzyma siatke 4x2 w stalej kolejnosci: CPU + (do 4 GPU) + RAM
  // + Disk + Power + Network. Wallclock i CPU Counters byly extras V2 i
  // wypadly z headline metrics (zostaja w innych zakladkach).
  const findings = buildQuickFindings(events, devices, report.duration_ns, names);
  const findingsHtml = findings.length === 0 ? `<div class="muted">${escape(t('profiling.report.no_findings', null, 'No notable findings.'))}</div>` : findings.map((f) => `
    <div class="pr-finding-card ${escape(f.kind)}">
      <span class="f-ico">${iconFinding(f.kind)}</span>
      <div class="f-body">
        <div class="f-title">${escape(f.title)}</div>
        <div class="f-detail">${escape(f.detail)}</div>
      </div>
    </div>
  `).join('');

  const lanes = [];
  lanes.push({ label: t('profiling.report.lane_cpu', null, 'CPU'), color: '#a78bfa', bg: 'rgba(167,139,250,0.05)', points: buildTimeSeries(events, 'CpuUtil', null, 'util_pct') });
  for (const d of devices) {
    const badge = vendorBadge(d.vendor);
    const colors = { nv: '#76b900', amd: '#ed1c24', intel: '#0071c5', apple: '#d4d4d8' };
    lanes.push({
      label: t('profiling.report.lane_gpu', { id: d.device_id }, `GPU${d.device_id}`),
      color: colors[badge.cls] || '#a78bfa',
      bg: 'rgba(255,255,255,0.02)',
      points: buildTimeSeries(events, 'GpuUtilSample', d.device_id, 'compute_pct'),
    });
  }
  if (hasPower) {
    const totals = new Map();
    for (const e of eventsForCategory(events, 'PowerSample')) {
      const p = unwrapPayload(e.payload);
      if (!p) continue;
      totals.set(e.t_start_ns, (totals.get(e.t_start_ns) || 0) + p.watts);
    }
    const arr = Array.from(totals.entries()).map(([tt, v]) => [tt, v]).sort((a, b) => a[0] - b[0]);
    lanes.push({ label: t('profiling.report.lane_pwr', null, 'PWR'), color: '#f59e0b', bg: 'rgba(245,158,11,0.04)', points: arr });
  }

  return `
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.section_headline', null, 'Headline metrics'))}</h2>
      <div class="pr-kpi-grid">
        ${cpuTile}
        ${gpuTiles}
        ${ramTile}
        ${diskTile}
        ${powerTile}
        ${netTile}
      </div>
    </section>

    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.section_findings', null, 'Quick findings'))}</h2>
      <div class="pr-findings-stack">${findingsHtml}</div>
    </section>

    <section class="pr-card">
      <h2 class="pr-card-title">
        ${escape(t('profiling.report.section_timeline_preview', null, 'Timeline preview'))}
        <span class="pr-card-actions"><tf-button variant="ghost" size="sm" data-action="open-timeline">${escape(t('profiling.report.open_unified_timeline', null, 'Open Unified Timeline →'))}</tf-button></span>
      </h2>
      <div class="pr-timeline-preview">${renderRidgelinePreview(lanes, { width: 920, height: 140 })}</div>
    </section>
  `;
}

// =============================================================================
// Tab: CPU Detail (mockup #07) — per-core grid + PMU counters + top symbols
// + hot threads. Source: linux.proc.cpu_util (CpuUtil events) — zawsze
// dostepne. CpuSample / CpuCounter — opcjonalnie (perf record + perf stat),
// banner-degraded gdy brak.
// =============================================================================

function renderCpuDetailTab(ctx) {
  const { events, report } = ctx;
  const utilEvents = eventsForCategory(events, 'CpuUtil');
  const sampleEvents = eventsForCategory(events, 'CpuSample');
  const counterEvents = eventsForCategory(events, 'CpuCounter');
  const names = report.names || [];
  const frames = report.frames || [];
  const stacks = report.stacks || [];

  if (utilEvents.length === 0) {
    return noDataCard(t('profiling.report.cpu_no_data', null, 'No CPU utilization data collected for this session.'));
  }

  // -- Per-core grid ----------------------------------------------------------
  // Mockup pokazuje 16 kafelkow; my renderujemy ile cores faktycznie
  // raportowalo (4/8/16/32+). Klik kafla emituje CustomEvent
  // 'pr2:cpu-filter-by-core' (nasluchiwacze podswietlaja core w timeline).
  const perCore = new Map();
  for (const e of utilEvents) {
    const p = unwrapPayload(e.payload);
    if (!p || typeof p.core !== 'number') continue;
    if (!perCore.has(p.core)) perCore.set(p.core, []);
    perCore.get(p.core).push({ t: e.t_start_ns, util: p.util_pct, freq: p.freq_mhz });
  }
  const cores = Array.from(perCore.keys()).sort((a, b) => a - b);
  const totalThreadsHint = cores.length > 0
    ? t('profiling.report.cpu_cores_hint', { cores: cores.length, samples: utilEvents.length.toLocaleString() }, `${cores.length} core${cores.length === 1 ? '' : 's'} · ${utilEvents.length.toLocaleString()} samples`)
    : `${utilEvents.length} samples`;

  const coreCells = cores.map((core) => {
    const points = perCore.get(core).sort((a, b) => a.t - b.t);
    const peak = points.reduce((m, x) => Math.max(m, x.util), 0);
    const last = points.length > 0 ? points[points.length - 1].util : 0;
    const xs = points.length;
    const sampled = xs <= 24 ? points : points.filter((_, i) => i % Math.max(1, Math.ceil(xs / 24)) === 0);
    const path = sampled.length === 0 ? 'M0 14 L100 14' : sampled
      .map((p, i, arr) => {
        const x = (i / Math.max(1, arr.length - 1)) * 100;
        const y = 28 - (p.util / 100) * 24 - 2;
        return `${i === 0 ? 'M' : 'L'} ${x.toFixed(1)} ${y.toFixed(1)}`;
      })
      .join(' ');
    // Kolor wedlug peak (mockup: red >80, amber >60, violet baseline).
    const color = peak > 80 ? '#ef4444' : peak > 60 ? '#f59e0b' : '#a78bfa';
    return `
      <button type="button" class="pr-core-mini" data-core="${core}" aria-label="Filter timeline by CPU ${core}">
        <div class="cm-head">
          <span class="cm-name">CPU ${core}</span>
          <span class="cm-val">${last.toFixed(0)}%</span>
        </div>
        <svg viewBox="0 0 100 28" preserveAspectRatio="none" aria-hidden="true">
          <path d="${path}" stroke="${color}" stroke-width="1.4" fill="none"/>
        </svg>
      </button>
    `;
  }).join('');

  // -- PMU counters -----------------------------------------------------------
  // Kazde CpuCounter event ma { kind, value }. kind to enum rkyv: unit warianty
  // (Ipc/CacheMissL3/BranchMiss/ContextSwitches/...) serializuja sie jako string;
  // Custom(...) jako { Custom: "..." }.
  const counterKindLabel = (k) => {
    if (typeof k === 'string') return k;
    if (k && typeof k === 'object' && 'Custom' in k) return String(k.Custom);
    return '?';
  };

  const counterSeries = new Map(); // label -> [{t, v}]
  for (const e of counterEvents) {
    const p = unwrapPayload(e.payload) || e.payload;
    if (!p || typeof p !== 'object') continue;
    const kind = counterKindLabel(p.kind);
    if (typeof p.value !== 'number') continue;
    if (!counterSeries.has(kind)) counterSeries.set(kind, []);
    counterSeries.get(kind).push({ t: e.t_start_ns, v: p.value });
  }

  // -- Context switches (osobna karta z trendu CounterKind=ContextSwitches) ---
  const ctxSwitchPts = (counterSeries.get('ContextSwitches') || []).slice().sort((a, b) => a.t - b.t);
  const ctxSwitchTotal = ctxSwitchPts.reduce((s, x) => s + x.v, 0);
  let ctxSwitchSection;
  if (ctxSwitchPts.length > 0) {
    const max = Math.max(...ctxSwitchPts.map((p) => p.v), 1);
    const path = ctxSwitchPts.map((p, i, arr) => {
      const x = (i / Math.max(1, arr.length - 1)) * 920;
      const y = 60 - (p.v / max) * 50 - 4;
      return `${i === 0 ? 'M' : 'L'} ${x.toFixed(1)} ${y.toFixed(1)}`;
    }).join(' ');
    const avg = ctxSwitchTotal / ctxSwitchPts.length;
    ctxSwitchSection = `
      <section class="pr-card">
        <h2 class="pr-card-title">
          ${escape(t('profiling.report.cpu_ctx_switch_title', null, 'Context switches /s'))}
          <span class="pr-card-actions"><span class="muted" style="font-size:11px;">${escape(t('profiling.report.cpu_ctx_switch_meta', { avg: Math.round(avg).toLocaleString(), peak: Math.round(max).toLocaleString() }, `avg ${Math.round(avg).toLocaleString()} · peak ${Math.round(max).toLocaleString()}`))}</span></span>
        </h2>
        <div class="pr-pmu-chart">
          <svg viewBox="0 0 920 60" preserveAspectRatio="none" style="width:100%; height:60px;">
            <path d="${path}" stroke="#60a5fa" stroke-width="1.4" fill="none"/>
          </svg>
        </div>
      </section>
    `;
  } else {
    ctxSwitchSection = '';
  }

  // -- PMU table + line chart -------------------------------------------------
  let pmuSection;
  if (counterSeries.size > 0) {
    // Lookup totalow do liczenia "Per-instr": instructions jako baseline.
    const totals = new Map();
    for (const [kind, pts] of counterSeries) {
      const sum = pts.reduce((s, p) => s + p.v, 0);
      totals.set(kind, sum);
    }
    const instructionsTotal = totals.get('Instructions') || totals.get('Ipc');

    // KPI tabela (Counter | Total | Per-instr | Trend).
    const formatNumber = (n) => {
      if (!Number.isFinite(n)) return '—';
      if (Math.abs(n) >= 1e12) return (n / 1e12).toFixed(2) + 'T';
      if (Math.abs(n) >= 1e9)  return (n / 1e9).toFixed(2) + 'G';
      if (Math.abs(n) >= 1e6)  return (n / 1e6).toFixed(2) + 'M';
      if (Math.abs(n) >= 1e3)  return (n / 1e3).toFixed(2) + 'k';
      return n.toFixed(2);
    };
    const perInstr = (kind, sum) => {
      if (kind === 'Ipc' || kind === 'Instructions' || !instructionsTotal) return '—';
      const r = sum / instructionsTotal;
      if (r >= 1) return r.toFixed(3);
      if (r >= 0.001) return (r * 100).toFixed(2) + '%';
      return (r * 1000).toFixed(2) + '/k';
    };

    // Stabilna kolejnosc kluczowych counterow (jesli istnieja).
    const orderHints = ['Ipc', 'Instructions', 'CacheMissL1', 'CacheMissL2', 'CacheMissL3', 'BranchMiss', 'ContextSwitches', 'PageFaults', 'TlbMiss'];
    const ordered = [
      ...orderHints.filter((k) => counterSeries.has(k)),
      ...Array.from(counterSeries.keys()).filter((k) => !orderHints.includes(k)),
    ];

    const counterRows = ordered.map((kind) => {
      const pts = counterSeries.get(kind);
      const sum = totals.get(kind);
      const avg = sum / pts.length;
      const last = pts[pts.length - 1].v;
      return `
        <tr>
          <td class="mono">${escape(kind)}</td>
          <td class="num mono">${formatNumber(sum)}</td>
          <td class="num mono">${formatNumber(avg)}</td>
          <td class="num mono">${formatNumber(last)}</td>
          <td class="num mono">${escape(perInstr(kind, sum))}</td>
        </tr>
      `;
    }).join('');

    // Linie chart (max 4 najwazniejsze: Ipc/CacheMissL3/BranchMiss/Instructions).
    const chartKinds = ordered.filter((k) => k !== 'ContextSwitches').slice(0, 4);
    const colors = {
      Ipc: '#a78bfa', Instructions: '#60a5fa',
      CacheMissL1: '#fb923c', CacheMissL2: '#f59e0b', CacheMissL3: '#f59e0b',
      BranchMiss: '#22c55e', PageFaults: '#f472b6', TlbMiss: '#ef4444',
    };
    const lines = chartKinds.map((kind) => {
      const pts = counterSeries.get(kind).slice().sort((a, b) => a.t - b.t);
      const max = Math.max(...pts.map((p) => p.v), 1);
      const path = pts.map((p, i, arr) => {
        const x = (i / Math.max(1, arr.length - 1)) * 880 + 40;
        const y = 160 - (p.v / max) * 140;
        return `${i === 0 ? 'M' : 'L'} ${x.toFixed(1)} ${y.toFixed(1)}`;
      }).join(' ');
      const c = colors[kind] || '#a0a8c8';
      const dash = kind.startsWith('CacheMiss') ? ' stroke-dasharray="4 2"' : kind === 'BranchMiss' ? ' stroke-dasharray="2 3"' : '';
      return `<path d="${path}" stroke="${c}" stroke-width="1.6" fill="none"${dash}/>`;
    }).join('');
    const legend = chartKinds.map((k) => {
      const c = colors[k] || '#a0a8c8';
      return `<span class="pr-pmu-legend-item"><span class="sw" style="background:${c};"></span>${escape(k)}</span>`;
    }).join('');

    pmuSection = `
      <section class="pr-card">
        <h2 class="pr-card-title">${escape(t('profiling.report.cpu_pmu_title', null, 'Hardware counters (PMU)'))}</h2>
        <div class="pr-pmu-chart">
          <svg viewBox="0 0 920 180" preserveAspectRatio="none" style="width:100%; height:180px;">
            <line x1="40" y1="160" x2="920" y2="160" stroke="#1f2548"/>
            <line x1="40" y1="20" x2="40" y2="160" stroke="#1f2548"/>
            ${lines}
          </svg>
        </div>
        <div class="pr-pmu-legend">${legend}</div>
        <table class="pr-table" style="margin-top:10px;">
          <thead>
            <tr>
              <th>${escape(t('profiling.report.col_counter', null, 'Counter'))}</th>
              <th class="num">${escape(t('profiling.report.col_total', null, 'Total'))}</th>
              <th class="num">${escape(t('profiling.report.col_avg_per_sample', null, 'Avg/sample'))}</th>
              <th class="num">${escape(t('profiling.report.col_last', null, 'Last'))}</th>
              <th class="num">${escape(t('profiling.report.col_per_instr', null, 'Per-instr'))}</th>
            </tr>
          </thead>
          <tbody>${counterRows}</tbody>
        </table>
      </section>
    `;
  } else {
    pmuSection = `
      <section class="pr-card">
        <h2 class="pr-card-title">${escape(t('profiling.report.cpu_pmu_title', null, 'Hardware counters (PMU)'))}</h2>
        <div class="pr-banner-degraded">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
          <div>${t('profiling.report.cpu_pmu_no_data', null, '<strong>PMU counters require perf stat.</strong> Add <code>linux.perf.pmu_counters</code> source (perf stat -e cycles,instructions,cache-misses,branch-misses) to capture IPC, L3 miss rate and branch miss rate.')}</div>
        </div>
      </section>
    `;
  }

  // -- Top symbols + Hot threads (CpuSample required) ------------------------
  let symbolsSection;
  let threadsSection;
  if (sampleEvents.length > 0) {
    // Self% = sample landed na leaf frame. Total% = symbol pojawia sie gdziekolwiek
    // w stacku (any frame). `module` jest na rekordzie Frame.
    const selfCounts = new Map();    // symbol -> { module, n }
    const totalCounts = new Map();   // symbol -> n (per-stack-occurrence)
    const threadCounts = new Map();  // tid -> { tid, samples, cmdHint }

    for (const e of sampleEvents) {
      const p = unwrapPayload(e.payload) || e.payload;
      if (!p) continue;
      const stackId = p.stack_id;
      const stack = stacks[stackId] || [];
      if (stack.length > 0) {
        const leafIdx = stack[0];
        const leaf = frames[leafIdx];
        if (leaf) {
          const sym = leaf.symbol || `frame_${leafIdx}`;
          const mod = leaf.module || '';
          if (!selfCounts.has(sym)) selfCounts.set(sym, { module: mod, n: 0 });
          selfCounts.get(sym).n += 1;
        }
        // Total: dedup w obrebie pojedynczego stacka.
        const seenInStack = new Set();
        for (const fIdx of stack) {
          const fr = frames[fIdx];
          if (!fr) continue;
          const s = fr.symbol || `frame_${fIdx}`;
          if (seenInStack.has(s)) continue;
          seenInStack.add(s);
          totalCounts.set(s, (totalCounts.get(s) || 0) + 1);
        }
      }
      // Hot threads (TID heuristic — cmd resolution: brak dedykowanego pola,
      // probujemy pierwszy frame stacku jako hint).
      if (typeof p.tid === 'number') {
        if (!threadCounts.has(p.tid)) {
          let cmdHint = '';
          if (stack.length > 0) {
            const root = frames[stack[stack.length - 1]];
            if (root && root.module) cmdHint = root.module.replace(/\.[^.]+$/, '');
          }
          threadCounts.set(p.tid, { tid: p.tid, samples: 0, cmdHint });
        }
        threadCounts.get(p.tid).samples += 1;
      }
    }

    const totalSamples = sampleEvents.length;
    const top = Array.from(selfCounts.entries())
      .map(([sym, info]) => ({
        sym,
        module: info.module,
        selfPct: (info.n / totalSamples) * 100,
        totalPct: ((totalCounts.get(sym) || info.n) / totalSamples) * 100,
        n: info.n,
      }))
      .sort((a, b) => b.selfPct - a.selfPct)
      .slice(0, 12);

    const symbolRows = top.map((r) => `
      <tr data-symbol="${escape(r.sym)}">
        <td class="mono">${escape(r.sym)}</td>
        <td class="mono dim">${escape(r.module || '—')}</td>
        <td class="num mono">${r.selfPct.toFixed(1)}%</td>
        <td class="num mono">${r.totalPct.toFixed(1)}%</td>
      </tr>
    `).join('');

    symbolsSection = `
      <section class="pr-card">
        <h2 class="pr-card-title">
          ${escape(t('profiling.report.cpu_top_symbols_title', null, 'Top symbols'))}
          <span class="pr-card-actions"><tf-button variant="ghost" size="sm" data-action="open-flame">${escape(t('profiling.report.cpu_open_flame_btn', null, 'Open flamegraph →'))}</tf-button></span>
        </h2>
        <table class="pr-table">
          <thead>
            <tr><th>${escape(t('profiling.report.col_symbol', null, 'Symbol'))}</th><th>${escape(t('profiling.report.col_module', null, 'Module'))}</th><th class="num">${escape(t('profiling.report.col_self_pct', null, 'Self %'))}</th><th class="num">${escape(t('profiling.report.col_total_pct', null, 'Total %'))}</th></tr>
          </thead>
          <tbody>${symbolRows}</tbody>
        </table>
      </section>
    `;

    const hotThreads = Array.from(threadCounts.values())
      .sort((a, b) => b.samples - a.samples)
      .slice(0, 10);
    const threadRows = hotThreads.map((t) => {
      const cpuPct = (t.samples / totalSamples) * 100;
      return `
        <tr>
          <td class="mono accent">${t.tid}</td>
          <td class="mono">${escape(t.cmdHint || '—')}</td>
          <td class="num mono">${cpuPct.toFixed(1)}%</td>
          <td class="num mono">${t.samples.toLocaleString()}</td>
        </tr>
      `;
    }).join('');
    threadsSection = `
      <section class="pr-card">
        <h2 class="pr-card-title">${escape(t('profiling.report.cpu_hot_threads_title', null, 'Hot threads'))}</h2>
        <table class="pr-table">
          <thead>
            <tr><th>${escape(t('profiling.report.col_tid', null, 'TID'))}</th><th>${escape(t('profiling.report.col_name', null, 'Name'))}</th><th class="num">${escape(t('profiling.report.col_cpu_pct', null, 'CPU %'))}</th><th class="num">${escape(t('profiling.report.col_samples', null, 'Samples'))}</th></tr>
          </thead>
          <tbody>${threadRows}</tbody>
        </table>
      </section>
    `;
  } else {
    const banner = (title, msg) => `
      <section class="pr-card">
        <h2 class="pr-card-title">${title}</h2>
        <div class="pr-banner-degraded">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18"/></svg>
          <div>${msg}</div>
        </div>
      </section>
    `;
    symbolsSection = banner(
      escape(t('profiling.report.cpu_top_symbols_title', null, 'Top symbols')),
      t('profiling.report.cpu_top_symbols_no_data', null, '<strong>Top symbols require perf record.</strong> Add <code>linux.perf.cpu_sampling</code> source (perf record -F 99 -g) to enable per-symbol hotspots and the flamegraph view.'),
    );
    threadsSection = banner(
      escape(t('profiling.report.cpu_hot_threads_title', null, 'Hot threads')),
      t('profiling.report.cpu_hot_threads_no_data', null, '<strong>Hot threads require perf record.</strong> Per-thread CPU breakdown is reconstructed from CPU sampling events.'),
    );
  }

  return `
    <section class="pr-card">
      <h2 class="pr-card-title">
        ${escape(t('profiling.report.cpu_per_core_title', null, 'Per-core utilization'))}
        <span class="pr-card-actions"><span class="muted" style="font-size:11px;">${escape(totalThreadsHint)}</span></span>
      </h2>
      <div class="pr-cores-grid">${coreCells}</div>
    </section>

    ${pmuSection}

    <div class="pr-cpu-detail-row">
      ${symbolsSection}
      ${threadsSection}
    </div>

    ${ctxSwitchSection}
  `;
}

// Klik kafla per-core: emit event do timeline highlight.
// Klik symbolu: switch flamegraph tab z preselected symbol.
function bindCpuDetailTab(host) {
  host.addEventListener('click', (e) => {
    const coreBtn = e.target.closest('[data-core]');
    if (coreBtn) {
      const core = Number(coreBtn.dataset.core);
      host.dispatchEvent(new CustomEvent('pr2:cpu-filter-by-core', { detail: { core }, bubbles: true }));
      host.querySelectorAll('[data-core]').forEach((b) => b.removeAttribute('data-active'));
      coreBtn.setAttribute('data-active', 'true');
      return;
    }
    const flameBtn = e.target.closest('[data-action="open-flame"]');
    if (flameBtn) {
      host.dispatchEvent(new CustomEvent('pr2:open-tab', { detail: { tab: 'flame' }, bubbles: true }));
      return;
    }
    const symRow = e.target.closest('[data-symbol]');
    if (symRow) {
      const symbol = symRow.dataset.symbol;
      host.dispatchEvent(new CustomEvent('pr2:open-tab', { detail: { tab: 'flame', symbol }, bubbles: true }));
    }
  });
}

// =============================================================================
// Tab: GPU (unified per-vendor).
// =============================================================================

function renderGpuTab(ctx) {
  const { devices } = ctx;
  if (devices.length === 0) {
    return noDataCard(t('profiling.report.gpu_no_data', null, 'No GPU data collected for this session.'));
  }
  const subTabs = devices.map((d, idx) => {
    const badge = vendorBadge(d.vendor);
    return `
      <button type="button" class="pr-vendor-tab" data-gpu-tab="${d.device_id}" ${idx === 0 ? 'data-active="true"' : ''} role="tab" aria-selected="${idx === 0 ? 'true' : 'false'}">
        <span class="pr-vendor-badge ${badge.cls}">${escape(badge.label)}</span>
        <span>${escape(t('profiling.report.kpi_gpu_label', { id: d.device_id, name: d.name }, `GPU ${d.device_id} — ${d.name}`))}</span>
      </button>
    `;
  }).join('');

  const cards = devices.map((d, idx) => `
    <div class="pr-gpu-device-pane" data-gpu-pane="${d.device_id}" ${idx === 0 ? '' : 'hidden'}>
      ${renderGpuDeviceCard(ctx, d)}
    </div>
  `).join('');

  return `
    <section class="pr-card">
      <div class="pr-vendor-tabs" role="tablist" aria-label="${escape(t('profiling.report.gpu_aria_device', null, 'GPU device'))}">${subTabs}</div>
      ${cards}
    </section>
  `;
}

function bindGpuTab(host, ctx) {
  host.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-gpu-tab]');
    if (!btn) return;
    const id = btn.dataset.gpuTab;
    host.querySelectorAll('[data-gpu-tab]').forEach((b) => {
      const isActive = b.dataset.gpuTab === id;
      if (isActive) b.setAttribute('data-active', 'true'); else b.removeAttribute('data-active');
      b.setAttribute('aria-selected', isActive ? 'true' : 'false');
    });
    host.querySelectorAll('[data-gpu-pane]').forEach((p) => {
      if (p.dataset.gpuPane === id) p.removeAttribute('hidden');
      else p.setAttribute('hidden', '');
    });
  });
}

function renderGpuDeviceCard(ctx, d) {
  const { events, report, kpis } = ctx;
  const k = kpis.gpu.get(d.device_id) || { peakCompute: 0, peakMem: 0, memUsedBytes: 0, topKernel: null, topPct: 0, avgW: 0, peakW: 0, kernels: [], apis: [] };
  const badge = vendorBadge(d.vendor);

  const collectorStatus = d.limited
    ? `<span class="pr-status-pill warn">${escape(t('profiling.report.gpu_status_limited', { coll: d.collector || t('profiling.report.gpu_collector_unknown', null, 'unknown') }, `Limited (${d.collector || 'unknown'})`))}</span>`
    : `<span class="pr-status-pill ok">${escape(d.collector || t('profiling.report.gpu_collector_default', null, 'collector'))}</span>`;

  const computeSeries = buildTimeSeries(events, 'GpuUtilSample', d.device_id, 'compute_pct');
  const memSeries = buildTimeSeries(events, 'GpuUtilSample', d.device_id, 'mem_pct');
  const powerSeries = buildPowerSeriesForGpu(events, d.device_id);

  const colors = { nv: '#76b900', amd: '#ed1c24', intel: '#0071c5', apple: '#d4d4d8' };
  const color = colors[badge.cls] || '#a78bfa';

  // Memory chart: Apple Silicon → unified memory banner; otherwise mem%.
  const memChart = d.vendor === 'apple'
    ? `<div class="pr-mini-chart"><div class="mc-title"><span>${escape(t('profiling.report.gpu_memory_pct', null, 'Memory %'))}</span><span class="v">${escape(t('profiling.report.kpi_unified_mem', null, 'unified'))}</span></div><div class="pr-banner-degraded inline"><div>${escape(t('profiling.report.gpu_unified_mem_banner', null, 'Unified memory — see RAM tab for combined pressure.'))}</div></div></div>`
    : `<div class="pr-mini-chart"><div class="mc-title"><span>${escape(t('profiling.report.gpu_memory_pct', null, 'Memory %'))}</span><span class="v">${escape(t('profiling.report.gpu_max_suffix', { val: formatPct(k.peakMem, 0) }, `${formatPct(k.peakMem, 0)} max`))}</span></div>${renderLineChart(memSeries, { color, height: 60, ariaLabel: t('profiling.report.gpu_aria_memory', null, 'GPU memory utilization') })}</div>`;

  const charts = `
    <div class="pr-charts-row">
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.gpu_compute_pct', null, 'Compute %'))}</span><span class="v">${escape(t('profiling.report.gpu_max_suffix', { val: formatPct(k.peakCompute, 0) }, `${formatPct(k.peakCompute, 0)} max`))}</span></div>
        ${renderLineChart(computeSeries, { color, height: 60, ariaLabel: t('profiling.report.gpu_aria_compute', null, 'GPU compute utilization') })}
      </div>
      ${memChart}
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.gpu_power_w', null, 'Power W'))}</span><span class="v">${escape(t('profiling.report.gpu_max_suffix', { val: formatPower(k.peakW) }, `${formatPower(k.peakW)} max`))}</span></div>
        ${renderLineChart(powerSeries, { color: '#f59e0b', height: 60, ariaLabel: t('profiling.report.gpu_aria_power', null, 'GPU power') })}
      </div>
    </div>
  `;

  // Limited / no-kernel banner.
  const degradedBanner = (() => {
    if (d.vendor === 'intel') {
      return `<div class="pr-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>${t('profiling.report.gpu_intel_kernel_banner', null, '<strong>Kernel-level metrics not available on this platform.</strong> Engine utilization captured via <code class=\"mono\">intel_gpu_top</code>. Install <code class=\"mono\">Intel GPA</code> or <code class=\"mono\">VTune Profiler</code> for kernel traces.')}</div>
      </div>`;
    }
    if (d.vendor === 'apple') {
      return `<div class="pr-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>${t('profiling.report.gpu_apple_kernel_banner', null, '<strong>Apple Silicon — utilization and power only.</strong> For kernel-level insight, capture a Metal trace via <code class=\"mono\">Xcode Instruments</code>.')}</div>
      </div>`;
    }
    return '';
  })();

  // KPI row.
  const memKpi = d.vendor === 'apple'
    ? `<div class="pr-kpi-tile ${badge.cls}"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.kpi_peak_mem', null, 'Peak mem'))}</span></div><div class="pr-kpi-value pr-kpi-text-sm">${escape(t('profiling.report.kpi_unified_mem', null, 'unified'))}</div><div class="pr-kpi-sub">${escape(t('profiling.report.kpi_unified_mem_sub', null, 'shared with CPU'))}</div></div>`
    : `<div class="pr-kpi-tile ${badge.cls}"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.kpi_peak_mem', null, 'Peak mem'))}</span></div><div class="pr-kpi-value">${formatPct(k.peakMem, 0)}</div><div class="pr-kpi-sub">VRAM ${formatBytes(k.memUsedBytes)}${d.memTotalBytes ? ' / ' + formatBytes(d.memTotalBytes) : ''}</div></div>`;

  const topKernelKpi = k.topKernel
    ? `<div class="pr-kpi-tile ${badge.cls}"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.kpi_top_kernel', null, 'Top kernel'))}</span></div><div class="pr-kpi-value pr-kpi-text-sm">${escape(k.topKernel)}</div><div class="pr-kpi-sub">${t('profiling.report.kpi_top_pct', { pct: formatPct(k.topPct) }, `<strong>${formatPct(k.topPct)}</strong> total time`)}</div></div>`
    : `<div class="pr-kpi-tile ${badge.cls}"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.kpi_top_kernel', null, 'Top kernel'))}</span></div><div class="pr-kpi-value pr-kpi-text-sm muted">${escape(t('profiling.report.kpi_no_kernel_data', null, 'no kernel data'))}</div><div class="pr-kpi-sub">${escape(d.vendor === 'intel' ? t('profiling.report.kpi_use_intel_gpa', null, 'use Intel GPA') : t('profiling.report.kpi_use_metal_trace', null, 'use Metal trace'))}</div></div>`;

  const computeSubLabel = d.vendor === 'amd' ? t('profiling.report.kpi_cu_util', null, 'CU utilization')
    : d.vendor === 'intel' ? t('profiling.report.kpi_render_compute', null, 'render+compute')
    : t('profiling.report.kpi_sm_util', null, 'SM utilization');
  const kpiRow = `
    <div class="pr-gpu-kpi-row">
      <div class="pr-kpi-tile ${badge.cls}"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.kpi_peak_compute', null, 'Peak compute'))}</span></div><div class="pr-kpi-value">${formatPct(k.peakCompute, 0)}</div><div class="pr-kpi-sub">${escape(computeSubLabel)}</div></div>
      ${memKpi}
      ${topKernelKpi}
      <div class="pr-kpi-tile ${badge.cls}"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.kpi_power_avg', null, 'Power avg'))}</span></div><div class="pr-kpi-value">${formatPower(k.avgW)}</div><div class="pr-kpi-sub">${t('profiling.report.power_peak_strong', { val: formatPower(k.peakW) }, `Peak <strong>${formatPower(k.peakW)}</strong>`)}</div></div>
    </div>
  `;

  // Kernels table — precomputed in single-pass aggregation.
  const kernels = k.kernels;
  const kernelNoCapture = d.vendor === 'apple' ? t('profiling.report.gpu_apple_kernel_table_note', null, 'Per-kernel timings require Metal capture (Xcode Instruments).')
    : d.vendor === 'intel' ? t('profiling.report.gpu_intel_kernel_table_note', null, 'Per-kernel timings require Intel GPA / Level Zero tracer.')
    : t('profiling.report.gpu_no_kernel_capture', null, 'No kernel data captured.');
  const kernelTable = kernels.length === 0
    ? `<div class="pr-banner-degraded inline"><div>${escape(kernelNoCapture)}</div></div>`
    : `<table class="pr-table">
        <thead><tr><th>${escape(t('profiling.report.col_kernel', null, 'Kernel'))}</th><th class="num">${escape(t('profiling.report.col_count', null, 'Count'))}</th><th class="num">${escape(t('profiling.report.col_total_ms', null, 'Total ms'))}</th><th class="num">${escape(t('profiling.report.col_avg_us', null, 'Avg µs'))}</th><th class="num">${escape(t('profiling.report.col_pct', null, '%'))}</th></tr></thead>
        <tbody>${kernels.slice(0, 30).map((r) => `<tr><td class="mono">${escape(r.name)}</td><td class="num mono">${formatInt(r.count)}</td><td class="num mono">${(r.totalNs / 1e6).toFixed(1)}</td><td class="num mono">${(r.avgNs / 1e3).toFixed(1)}</td><td class="num mono">${formatPct(r.pct)}</td></tr>`).join('')}</tbody>
      </table>`;

  const apis = k.apis;
  const apiHeader = d.vendor === 'amd' ? t('profiling.report.gpu_apis_hip', null, 'HIP APIs')
    : d.vendor === 'intel' ? t('profiling.report.gpu_apis_levelzero', null, 'Level Zero APIs')
    : d.vendor === 'apple' ? t('profiling.report.gpu_apis_metal', null, 'Metal calls')
    : t('profiling.report.gpu_apis_cuda', null, 'CUDA APIs');
  const apiNoCapture = d.vendor === 'intel' ? t('profiling.report.gpu_intel_api_note', null, 'Per-API timings require Intel GPA / Level Zero tracer.')
    : t('profiling.report.gpu_no_api_capture', null, 'No API call data captured.');
  const apiTable = apis.length === 0
    ? (d.vendor === 'apple'
        ? `<table class="pr-table">
            <thead><tr><th>${escape(t('profiling.report.col_api', null, 'API'))}</th><th class="num">${escape(t('profiling.report.col_calls', null, 'Calls'))}</th><th class="num">${escape(t('profiling.report.col_total_ms', null, 'Total ms'))}</th></tr></thead>
            <tbody><tr><td class="mono muted">${escape(t('profiling.report.gpu_apple_api_placeholder', null, '— Metal calls available only with Xcode Instruments trace'))}</td><td></td><td></td></tr></tbody>
          </table>`
        : `<div class="pr-banner-degraded inline"><div>${escape(apiNoCapture)}</div></div>`)
    : `<table class="pr-table">
        <thead><tr><th>${escape(t('profiling.report.col_api', null, 'API'))}</th><th class="num">${escape(t('profiling.report.col_calls', null, 'Calls'))}</th><th class="num">${escape(t('profiling.report.col_total_ms', null, 'Total ms'))}</th></tr></thead>
        <tbody>${apis.slice(0, 30).map((r) => `<tr><td class="mono">${escape(r.name)}</td><td class="num mono">${formatInt(r.count)}</td><td class="num mono">${r.totalNs == null ? `<span class="muted">${escape(t('profiling.report.gpu_limited_pill', null, 'limited'))}</span>` : (r.totalNs / 1e6).toFixed(1)}</td></tr>`).join('')}</tbody>
      </table>`;

  const transferBlock = d.vendor === 'apple'
    ? `<div class="pr-banner-degraded"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 12h18"/><circle cx="12" cy="12" r="10"/></svg><div>${t('profiling.report.gpu_transfer_apple', null, '<strong>Unified memory architecture</strong> — no explicit host/device transfers on Apple Silicon. See RAM tab for combined memory pressure.')}</div></div>`
    : renderTransferChart(events, d, color);

  return `
    <article class="pr-gpu-device-card ${badge.cls}">
      <header class="gd-head">
        <span class="pr-vendor-badge ${badge.cls}">${escape(badge.label)}</span>
        <div class="gd-name">${escape(t('profiling.report.kpi_gpu_label', { id: d.device_id, name: d.name }, `GPU ${d.device_id} — ${d.name}`))}</div>
        ${d.version ? `<span class="gd-id mono">${escape(d.version)}</span>` : ''}
        <span class="gd-status">${collectorStatus}</span>
      </header>

      ${degradedBanner}
      ${kpiRow}
      ${charts}

      <h3 class="pr-subhead">${escape(t('profiling.report.gpu_top_kernels_h', null, 'Top kernels'))}</h3>
      ${kernelTable}

      <h3 class="pr-subhead">${escape(apiHeader)}</h3>
      ${apiTable}

      <h3 class="pr-subhead">${escape(t('profiling.report.gpu_transfer_h', null, 'Memory transfer (D2H / H2D / D2D)'))}</h3>
      ${transferBlock}
    </article>
  `;
}

function buildPowerSeriesForGpu(events, deviceId) {
  const out = [];
  for (const e of eventsForCategory(events, 'PowerSample')) {
    const p = unwrapPayload(e.payload);
    if (!p || !p.domain) continue;
    if (typeof p.domain === 'object' && 'Gpu' in p.domain && p.domain.Gpu === deviceId) {
      out.push([e.t_start_ns, p.watts]);
    }
  }
  out.sort((a, b) => a[0] - b[0]);
  return out;
}

// Mockup #08: trzy linie czasowe H2D / D2H / D2D w jednym SVG (solid + dwa
// wzory dasharray). Jezeli mamy GpuMemTransfer events, agregujemy bajty/s
// w 60-bucketowym histogramie i rysujemy realne serie. Bez eventow padamy
// na bary z peakow (skompresowany manifest).
function renderTransferChart(events, d, color) {
  const transferEvents = [];
  for (const e of eventsForCategory(events, 'GpuMemTransfer')) {
    const p = unwrapPayload(e.payload) || e.payload;
    if (!p || p.device_id !== d.device_id) continue;
    transferEvents.push({ t: e.t_start_ns, kind: p.kind, bytes: Number(p.bytes) || 0, dur: Math.max(1, Number(e.dur_ns) || 1) });
  }

  if (transferEvents.length > 0) {
    return renderTransferTimeSeries(transferEvents, d, color);
  }

  if (!d.transfers) {
    return `<div class="pr-banner-degraded inline"><div>${escape(t('profiling.report.gpu_transfer_no_telemetry', null, 'Transfer telemetry not collected for this device.'))}</div></div>`;
  }
  const { h2d_bytes_per_s_peak, d2h_bytes_per_s_peak, d2d_bytes_per_s_peak } = d.transfers;
  const peak = Math.max(h2d_bytes_per_s_peak || 0, d2h_bytes_per_s_peak || 0, d2d_bytes_per_s_peak || 0);
  if (peak === 0) {
    return `<div class="pr-banner-degraded inline"><div>${escape(t('profiling.report.gpu_transfer_no_window', null, 'No host-device transfers observed during this window.'))}</div></div>`;
  }
  const w = 600; const h = 60;
  const bars = ['H2D', 'D2H', 'D2D'].map((label, i) => {
    const v = [h2d_bytes_per_s_peak, d2h_bytes_per_s_peak, d2d_bytes_per_s_peak][i] || 0;
    const barW = (v / peak) * (w - 80);
    return `<g><text x="0" y="${18 + i * 18}" font-family="JetBrains Mono" font-size="10" fill="#a0a8c8">${label}</text><rect x="40" y="${10 + i * 18}" width="${barW.toFixed(0)}" height="10" fill="${color}" opacity="${0.9 - i * 0.2}"/><text x="${44 + barW}" y="${18 + i * 18}" font-family="JetBrains Mono" font-size="9" fill="#e8ebf5">${formatBytesPerSec(v)}</text></g>`;
  }).join('');
  return `<div class="pr-mini-chart"><div class="mc-title"><span>${escape(t('profiling.report.gpu_transfer_legend', null, 'MB/s — H2D · D2H · D2D'))}</span><span class="v">${escape(t('profiling.report.gpu_transfer_peak', { val: formatBytesPerSec(peak) }, `peak ${formatBytesPerSec(peak)}`))}</span></div><svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${escape(t('profiling.report.gpu_aria_transfer_peaks', null, 'GPU transfer peaks'))}">${bars}</svg></div>`;
}

function renderTransferTimeSeries(transferEvents, d, color) {
  const BUCKETS = 60;
  const ts = transferEvents.map((e) => e.t).sort((a, b) => a - b);
  const tMin = ts[0];
  const tMax = ts[ts.length - 1] + 1;
  const span = Math.max(1, tMax - tMin);
  const bucketNs = span / BUCKETS;

  const series = { H2D: new Float64Array(BUCKETS), D2H: new Float64Array(BUCKETS), D2D: new Float64Array(BUCKETS) };
  for (const ev of transferEvents) {
    if (!series[ev.kind]) continue; // UnifiedAccess etc. ignored for this chart
    const idx = Math.min(BUCKETS - 1, Math.floor((ev.t - tMin) / bucketNs));
    // bytes/s rate for this event: bytes / dur_ns * 1e9.
    const rate = (ev.bytes * 1e9) / ev.dur;
    if (rate > series[ev.kind][idx]) series[ev.kind][idx] = rate;
  }

  let peak = 0;
  for (const arr of Object.values(series)) {
    for (const v of arr) if (v > peak) peak = v;
  }
  if (peak === 0) {
    return `<div class="pr-banner-degraded inline"><div>${escape(t('profiling.report.gpu_transfer_no_window', null, 'No host-device transfers observed during this window.'))}</div></div>`;
  }

  const w = 600; const h = 60;
  const baseColors = vendorTransferColors(d.vendor, color);
  const path = (arr) => {
    let s = `M0 ${h - (arr[0] / peak) * (h - 6) - 3}`;
    for (let i = 1; i < BUCKETS; i++) {
      const x = (i / (BUCKETS - 1)) * w;
      const y = h - (arr[i] / peak) * (h - 6) - 3;
      s += ` L${x.toFixed(1)} ${y.toFixed(1)}`;
    }
    return s;
  };

  const lines = [
    `<path d="${path(series.H2D)}" stroke="${baseColors.h2d}" stroke-width="1.4" fill="none"/>`,
    `<path d="${path(series.D2H)}" stroke="${baseColors.d2h}" stroke-width="1.4" fill="none" stroke-dasharray="3 2"/>`,
    `<path d="${path(series.D2D)}" stroke="${baseColors.d2d}" stroke-width="1.2" fill="none" stroke-dasharray="2 3"/>`,
  ].join('');

  return `<div class="pr-mini-chart"><div class="mc-title"><span>${escape(t('profiling.report.gpu_transfer_legend', null, 'MB/s — H2D · D2H · D2D'))}</span><span class="v">${escape(t('profiling.report.gpu_transfer_peak', { val: formatBytesPerSec(peak) }, `peak ${formatBytesPerSec(peak)}`))}</span></div><svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${escape(t('profiling.report.gpu_aria_transfer_ts', null, 'GPU transfer time-series H2D D2H D2D'))}">${lines}</svg></div>`;
}

// Stroke palette per vendor, matching mockup #08 (NV greens, AMD reds, Intel blues).
function vendorTransferColors(vendor, fallback) {
  if (vendor === 'amd')   return { h2d: '#ed1c24', d2h: '#ff6b6b', d2d: '#fbbf24' };
  if (vendor === 'intel') return { h2d: '#0071c5', d2h: '#4ba3e0', d2d: '#7dd3fc' };
  if (vendor === 'nvidia' || vendor === 'nv') return { h2d: '#76b900', d2h: '#b9d300', d2d: '#4ade80' };
  return { h2d: fallback, d2h: fallback, d2d: fallback };
}

// =============================================================================
// Tab: Memory.
// =============================================================================

function renderMemoryTab(ctx) {
  const { events, report } = ctx;
  if (eventsForCategory(events, 'RamSample').length === 0) {
    return noDataCard(t('profiling.report.memory_no_data', null, 'No memory samples collected for this session.'));
  }

  const used = buildTimeSeries(events, 'RamSample', null, 'used_bytes').map(([t, v]) => [t, v / 1024 / 1024 / 1024]);
  const avail = buildTimeSeries(events, 'RamSample', null, 'available_bytes').map(([t, v]) => [t, v / 1024 / 1024 / 1024]);
  const faults = buildTimeSeries(events, 'RamSample', null, 'page_faults_per_s');

  const usedPeak = used.reduce((m, [, v]) => Math.max(m, v), 0);
  const availMin = avail.reduce((m, [, v]) => Math.min(m, v), Infinity);
  const faultsPeak = faults.reduce((m, [, v]) => Math.max(m, v), 0);
  // Total fizyczny RAM aproksymujemy jako max(used+available) — w typowym Linux
  // sumie tej dwojki nie zmienia sie istotnie (pamiec klasyfikowana inaczej).
  const totalGbApprox = (() => {
    let max = 0;
    for (let i = 0; i < used.length && i < avail.length; i += 1) {
      const sum = used[i][1] + avail[i][1];
      if (sum > max) max = sum;
    }
    return max;
  })();

  const bwEvents = eventsForCategory(events, 'RamBandwidth');
  const hasBw = bwEvents.length > 0;
  const readSeries = hasBw ? buildTimeSeries(events, 'RamBandwidth', null, 'read_bps').map(([t, v]) => [t, v / 1e9]) : [];
  const writeSeries = hasBw ? buildTimeSeries(events, 'RamBandwidth', null, 'write_bps').map(([t, v]) => [t, v / 1e9]) : [];

  const charts = `
    <div class="pr-charts-row">
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.memory_used_gb', null, 'Used GB'))}</span><span class="v">${escape(t('profiling.report.memory_used_peak', { val: usedPeak.toFixed(1) }, `${usedPeak.toFixed(1)} peak`))}</span></div>
        ${renderAreaChart(used, { color: '#a78bfa', fill: 'rgba(167,139,250,0.25)', height: 60 })}
      </div>
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.memory_avail_gb', null, 'Available GB'))}</span><span class="v">${availMin === Infinity ? '—' : escape(t('profiling.report.memory_avail_min', { val: availMin.toFixed(1) }, `${availMin.toFixed(1)} min`))}</span></div>
        ${renderLineChart(avail, { color: '#22c55e', height: 60 })}
      </div>
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.memory_page_faults', null, 'Page faults / s'))}</span><span class="v">${escape(t('profiling.report.memory_faults_peak', { val: formatInt(faultsPeak) }, `${formatInt(faultsPeak)} peak`))}</span></div>
        ${renderLineChart(faults, { color: '#f59e0b', height: 60 })}
      </div>
    </div>
  `;

  const readPeak = readSeries.reduce((m, [, v]) => Math.max(m, v), 0);
  const writePeak = writeSeries.reduce((m, [, v]) => Math.max(m, v), 0);
  const bwBlock = hasBw ? `
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.memory_bw_h', null, 'RAM bandwidth (uncore counters)'))}</h2>
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.memory_bw_legend', null, 'Read · Write GB/s'))}</span><span class="v">${escape(t('profiling.report.memory_bw_peaks', { r: readPeak.toFixed(1), w: writePeak.toFixed(1) }, `read peak ${readPeak.toFixed(1)} · write peak ${writePeak.toFixed(1)} GB/s`))}</span></div>
        <svg viewBox="0 0 920 80" preserveAspectRatio="none" role="img" aria-label="${escape(t('profiling.report.memory_bw_aria', null, 'RAM bandwidth'))}">
          ${ramBandwidthSvg(readSeries, writeSeries)}
        </svg>
      </div>
    </section>
  ` : `
    <section class="pr-card">
      <div class="pr-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>${t('profiling.report.memory_bw_no_data', null, '<strong>RAM bandwidth not collected.</strong> Requires uncore counters (sudo + cap_perfmon) on x86, or Apple Silicon performance counters.')}</div>
      </div>
    </section>
  `;

  return `
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.memory_pressure_h', null, 'Memory pressure'))}</h2>
      ${charts}
    </section>
    ${bwBlock}
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.memory_top_rss_h', null, 'Top processes by RSS'))}</h2>
      ${renderTopRssTable(events, report.names || [], totalGbApprox)}
    </section>
  `;
}

// Mockup #09 - top procs po RSS. Iteruje ProcessRssSample events, agreguje
// peak RSS per pid, renderuje table 10 najwiekszych. % of total liczony
// wzgledem fizycznego RAM (przyblizenie z RamSample); fallback na sume top10.
function renderTopRssTable(events, names, totalGb) {
  const rss = eventsForCategory(events, 'ProcessRssSample');
  if (rss.length === 0) {
    return `
      <div class="pr-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>${t('profiling.report.memory_rss_no_data', null, '<strong>Per-process RSS not collected.</strong> Add <code>linux.proc.top_processes</code> source.')}</div>
      </div>
    `;
  }
  const byPid = new Map();
  for (const e of rss) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    const cur = byPid.get(p.pid);
    const comm = names[p.comm_name_id] || `pid_${p.pid}`;
    if (!cur || cur.peakRss < p.rss_bytes) {
      byPid.set(p.pid, { pid: p.pid, comm, peakRss: p.rss_bytes, peakVsz: p.vsz_bytes });
    }
  }
  const top = Array.from(byPid.values()).sort((a, b) => b.peakRss - a.peakRss).slice(0, 10);
  const totalBytes = (totalGb && totalGb > 0)
    ? totalGb * 1024 * 1024 * 1024
    : (top.reduce((s, p) => s + p.peakRss, 0) || 1);
  const rows = top.map((p) => `
    <tr>
      <td class="mono">${formatInt(p.pid)}</td>
      <td>${escape(p.comm)}</td>
      <td class="num mono">${formatBytes(p.peakRss)}</td>
      <td class="num mono">${formatBytes(p.peakVsz)}</td>
      <td class="num mono">${formatPct((p.peakRss / totalBytes) * 100, 1)}</td>
    </tr>
  `).join('');
  return `
    <table class="pr-table">
      <thead><tr><th>${escape(t('profiling.report.col_pid', null, 'PID'))}</th><th>${escape(t('profiling.report.col_process', null, 'Process'))}</th><th class="num">${escape(t('profiling.report.col_rss', null, 'RSS'))}</th><th class="num">${escape(t('profiling.report.col_vsz', null, 'VSZ'))}</th><th class="num">${escape(t('profiling.report.col_pct_of_total', null, '% of total'))}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
  `;
}

function ramBandwidthSvg(readSeries, writeSeries) {
  const w = 920; const h = 80;
  const all = [...readSeries, ...writeSeries];
  if (all.length < 2) return '';
  let minX = Infinity; let maxX = -Infinity; let maxY = 0;
  for (const [x, y] of all) {
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y > maxY) maxY = y;
  }
  const dx = Math.max(1, maxX - minX);
  const sx = (x) => ((x - minX) / dx) * w;
  const sy = (y) => h - (maxY ? (y / maxY) * (h - 12) : 0);
  const pathFor = (pts) => pts.map(([x, y], i) => `${i === 0 ? 'M' : 'L'} ${sx(x).toFixed(1)} ${sy(y).toFixed(1)}`).join(' ');
  return `
    <line x1="0" y1="14" x2="${w}" y2="14" stroke="#ef4444" stroke-width="0.5" stroke-dasharray="3 3"/>
    <text x="6" y="12" font-family="JetBrains Mono" font-size="9" fill="#ef4444">${maxY.toFixed(0)} GB/s peak</text>
    <path d="${pathFor(readSeries)}" stroke="#60a5fa" stroke-width="1.6" fill="none"/>
    <path d="${pathFor(writeSeries)}" stroke="#a78bfa" stroke-width="1.6" fill="none" stroke-dasharray="3 2"/>
  `;
}

// =============================================================================
// Tab: Disk IO.
// =============================================================================

function renderDiskTab(ctx) {
  const { events, report } = ctx;
  const names = report.names || [];
  const resolveDevice = (p) => (p && p.deviceNameId !== undefined ? names[p.deviceNameId] : null) || `disk_${p?.deviceNameId ?? '?'}`;
  const all = eventsForCategory(events, 'DiskIoBurst').map((e) => ({ t: e.t_start_ns, p: unwrapPayload(e.payload) })).filter((s) => s.p);
  if (all.length === 0) {
    return noDataCard(t('profiling.report.disk_no_data', null, 'No disk IO samples collected.'));
  }
  const byDevice = new Map();
  for (const s of all) {
    const dev = resolveDevice(s.p);
    const arr = byDevice.get(dev) || [];
    arr.push(s);
    byDevice.set(dev, arr);
  }
  const cards = Array.from(byDevice.entries()).map(([device, samples]) => {
    const model = samples[0]?.p?.model || '';
    const readBpsPts = samples.map((s) => [s.t, s.p.read_bps || 0]);
    const writeBpsPts = samples.map((s) => [s.t, s.p.write_bps || 0]);
    const iopsPts = samples.map((s) => [s.t, (s.p.iops_r || 0) + (s.p.iops_w || 0)]);
    const latPts = samples.map((s) => [s.t, s.p.await_ms_p99 || 0]);
    const peakReadBps = readBpsPts.reduce((m, [, v]) => Math.max(m, v), 0);
    const peakWriteBps = writeBpsPts.reduce((m, [, v]) => Math.max(m, v), 0);
    const peakIops = iopsPts.reduce((m, [, v]) => Math.max(m, v), 0);
    const p99 = latPts.reduce((m, [, v]) => Math.max(m, v), 0);

    return `
      <section class="pr-card">
        <h2 class="pr-card-title">
          ${escape(device)}${model ? ` — <span class="muted">${escape(model)}</span>` : ''}
          <span class="pr-card-actions">
            <span class="pr-status-pill ok">${escape(t('profiling.report.disk_active_pill', null, 'Active'))}</span>
          </span>
        </h2>
        <div class="pr-charts-row">
          <div class="pr-mini-chart">
            <div class="mc-title"><span>${escape(t('profiling.report.disk_throughput', null, 'Throughput'))}</span><span class="v">${escape(t('profiling.report.disk_throughput_rw', { r: formatBytesPerSec(peakReadBps), w: formatBytesPerSec(peakWriteBps) }, `R ${formatBytesPerSec(peakReadBps)} · W ${formatBytesPerSec(peakWriteBps)}`))}</span></div>
            <svg viewBox="0 0 200 60" preserveAspectRatio="none" role="img" aria-label="${escape(t('profiling.report.disk_aria_throughput', null, 'Disk throughput'))}">
              ${twoLineSvg(readBpsPts, writeBpsPts, '#22c55e', '#ef4444')}
            </svg>
          </div>
          <div class="pr-mini-chart">
            <div class="mc-title"><span>${escape(t('profiling.report.disk_iops', null, 'IOPS (R+W)'))}</span><span class="v">${escape(t('profiling.report.disk_iops_peak', { val: formatInt(peakIops) }, `peak ${formatInt(peakIops)}`))}</span></div>
            ${renderLineChart(iopsPts, { color: '#a78bfa', height: 60 })}
          </div>
          <div class="pr-mini-chart">
            <div class="mc-title"><span>${escape(t('profiling.report.disk_latency_p99', null, 'Latency p99 ms'))}</span><span class="v">${escape(t('profiling.report.disk_latency_val', { val: p99.toFixed(1) }, `${p99.toFixed(1)} ms`))}</span></div>
            ${renderLineChart(latPts, { color: '#ef4444', height: 60 })}
          </div>
        </div>
      </section>
    `;
  }).join('');

  return `
    ${cards}
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.disk_top_io_h', null, 'Top processes by IO'))}</h2>
      ${renderTopIoTable(events, report.names || [])}
    </section>
  `;
}

// Mockup #10 - top procs po sumie read+write bytes. Bytes sa kumulatywne
// w /proc/[pid]/io, wiec last - first sample = delta during session.
function renderTopIoTable(events, names) {
  const io = eventsForCategory(events, 'ProcessIoSample');
  if (io.length === 0) {
    return `
      <div class="pr-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>${t('profiling.report.disk_io_no_data', null, '<strong>Per-process IO not collected.</strong> Add <code>linux.proc.top_processes</code> source (parses /proc/[pid]/io).')}</div>
      </div>
    `;
  }
  // Agreguj per pid: (first read, first write, last read, last write).
  const byPid = new Map();
  for (const e of io) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    const cur = byPid.get(p.pid) || {
      pid: p.pid,
      comm: names[p.comm_name_id] || `pid_${p.pid}`,
      firstRead: p.read_bytes,
      firstWrite: p.write_bytes,
      lastRead: p.read_bytes,
      lastWrite: p.write_bytes,
      tFirst: e.t_start_ns,
      tLast: e.t_start_ns,
    };
    if (e.t_start_ns < cur.tFirst) {
      cur.tFirst = e.t_start_ns;
      cur.firstRead = p.read_bytes;
      cur.firstWrite = p.write_bytes;
    }
    if (e.t_start_ns > cur.tLast) {
      cur.tLast = e.t_start_ns;
      cur.lastRead = p.read_bytes;
      cur.lastWrite = p.write_bytes;
    }
    byPid.set(p.pid, cur);
  }
  const top = Array.from(byPid.values())
    .map((p) => ({
      pid: p.pid,
      comm: p.comm,
      readDelta: Math.max(0, p.lastRead - p.firstRead),
      writeDelta: Math.max(0, p.lastWrite - p.firstWrite),
    }))
    .filter((p) => p.readDelta + p.writeDelta > 0)
    .sort((a, b) => (b.readDelta + b.writeDelta) - (a.readDelta + a.writeDelta))
    .slice(0, 10);
  if (top.length === 0) {
    return `<div class="pr-banner-degraded"><div>${escape(t('profiling.report.disk_io_no_window', null, 'No process IO during this session window.'))}</div></div>`;
  }
  const rows = top.map((p) => `
    <tr>
      <td class="mono">${formatInt(p.pid)}</td>
      <td>${escape(p.comm)}</td>
      <td class="num mono">${formatBytes(p.readDelta)}</td>
      <td class="num mono">${formatBytes(p.writeDelta)}</td>
      <td class="num mono">${formatBytes(p.readDelta + p.writeDelta)}</td>
    </tr>
  `).join('');
  return `
    <table class="pr-table">
      <thead><tr><th>${escape(t('profiling.report.col_pid', null, 'PID'))}</th><th>${escape(t('profiling.report.col_process', null, 'Process'))}</th><th class="num">${escape(t('profiling.report.col_read', null, 'Read'))}</th><th class="num">${escape(t('profiling.report.col_write', null, 'Write'))}</th><th class="num">${escape(t('profiling.report.col_total', null, 'Total'))}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
  `;
}

function twoLineSvg(a, b, colorA, colorB) {
  const w = 200; const h = 60;
  if (!a || a.length < 2) return '';
  let minX = Infinity; let maxX = -Infinity; let maxY = 0;
  for (const [x, y] of [...a, ...b]) {
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y > maxY) maxY = y;
  }
  const dx = Math.max(1, maxX - minX);
  const sx = (x) => ((x - minX) / dx) * w;
  const sy = (y) => h - (maxY ? (y / maxY) * h : 0);
  const path = (pts, c) => `<path d="${pts.map(([x, y], i) => `${i === 0 ? 'M' : 'L'} ${sx(x).toFixed(1)} ${sy(y).toFixed(1)}`).join(' ')}" stroke="${c}" stroke-width="1.5" fill="none"/>`;
  return path(a, colorA) + path(b, colorB);
}

// =============================================================================
// Tab: Power.
// =============================================================================

function renderPowerTab(ctx) {
  const { events, report } = ctx;
  const samples = eventsForCategory(events, 'PowerSample');
  if (samples.length === 0) {
    return `
      <section class="pr-card">
        <div class="pr-banner-degraded">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
          <div>${t('profiling.report.power_no_samples', null, '<strong>No power samples collected.</strong> Power telemetry requires <code class=\"mono\">RAPL</code> (sudo / CAP_SYS_RAWIO on Linux), <code class=\"mono\">nvidia-smi</code>, <code class=\"mono\">rocm-smi</code>, or <code class=\"mono\">powermetrics</code> (macOS, sudo). Re-run the session with elevated privileges.')}</div>
        </div>
      </section>
    `;
  }

  // Grupowanie po domenie → seria czasowa watów.
  const byDomain = new Map();
  for (const e of samples) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    const key = domainKey(p.domain);
    const arr = byDomain.get(key) || [];
    arr.push([e.t_start_ns, p.watts]);
    byDomain.set(key, arr);
  }

  const palette = { CpuPkg: '#a78bfa', CpuCore: '#7c3aed', Dram: '#60a5fa', Ane: '#22c55e', Soc: '#fbbf24', Other: '#71717a' };
  const gpuColors = ['#76b900', '#ed1c24', '#0071c5', '#d4d4d8'];
  const humanLabel = (key) => {
    if (key === 'CpuPkg') return 'CPU pkg';
    if (key === 'CpuCore') return 'CPU core';
    if (key === 'Dram') return 'DRAM';
    if (key === 'Ane') return 'ANE';
    if (key === 'Soc') return 'SoC';
    if (key.startsWith('Gpu(')) {
      const idx = parseInt(key.match(/\d+/)?.[0] || '0', 10);
      return `GPU${idx}`;
    }
    return key;
  };
  const colorForKey = (key) => {
    if (key.startsWith('Gpu(')) {
      const idx = parseInt(key.match(/\d+/)?.[0] || '0', 10);
      return gpuColors[idx % gpuColors.length];
    }
    return palette[key] || '#a78bfa';
  };

  const sortedKeys = Array.from(byDomain.keys()).sort((a, b) => domainOrder(a) - domainOrder(b));
  const seriesList = [];
  const colors = [];
  const labels = [];
  const avgs = [];
  for (const key of sortedKeys) {
    const points = byDomain.get(key).slice().sort((a, b) => a[0] - b[0]);
    seriesList.push(points);
    colors.push(colorForKey(key));
    labels.push(humanLabel(key));
    const avgW = points.length ? points.reduce((s, [, v]) => s + v, 0) / points.length : 0;
    avgs.push(avgW);
  }

  const power = ctx.kpis.power;
  const kWh = power.totalKj / 3600;
  const cost = kWh * 0.15;

  const legend = labels.map((l, i) =>
    `<span class="lg"><span class="sw" style="background:${colors[i]};"></span>${escape(t('profiling.report.power_legend_avg', { label: l, avg: avgs[i].toFixed(0) }, `${l} (${avgs[i].toFixed(0)}W avg)`))}</span>`
  ).join('') +
    `<span class="lg"><span class="sw" style="background:#f59e0b;height:2px;"></span>${escape(t('profiling.report.power_legend_total', { peak: formatPower(power.peakW) }, `Total (peak ${formatPower(power.peakW)})`))}</span>`;

  const hasAne = byDomain.has('Ane');
  const anePlaceholder = !hasAne ? `
    <div class="pr-mini-chart">
      <div class="mc-title"><span>${escape(t('profiling.report.power_ane_not_detected', null, 'ANE W'))}</span><span class="v">${escape(t('profiling.report.power_ane_not_detected_v', null, '— not detected'))}</span></div>
      <svg viewBox="0 0 200 60" preserveAspectRatio="none" aria-label="${escape(t('profiling.report.power_ane_aria', null, 'ANE not detected'))}">
        <path d="M0 56 L200 56" stroke="#71717a" stroke-width="1" stroke-dasharray="2 3" fill="none"/>
      </svg>
    </div>
  ` : '';

  const miniCharts = labels.map((l, i) => {
    const pts = seriesList[i];
    const peak = pts.reduce((m, [, v]) => Math.max(m, v), 0);
    return `
      <div class="pr-mini-chart">
        <div class="mc-title"><span>${escape(t('profiling.report.power_domain_w', { label: l }, `${l} W`))}</span><span class="v">${escape(t('profiling.report.power_domain_max', { val: peak.toFixed(0) }, `${peak.toFixed(0)} max`))}</span></div>
        ${renderLineChart(pts, { color: colors[i], height: 60 })}
      </div>
    `;
  }).join('') + anePlaceholder;

  // Banner gdy RAPL/CPU pkg niedostępne — pokazujemy częściowe dane (np. tylko GPU).
  const raplSkipped = (report.collectors || []).some((c) => {
    const id = (c.id || '').toLowerCase();
    if (!id.includes('rapl') && !id.includes('power')) return false;
    const status = normalizeCollectorStatus(c.status);
    return status.kind === 'skipped' || status.kind === 'failed';
  });
  const partialBanner = (raplSkipped && !byDomain.has('CpuPkg') && !byDomain.has('Dram')) ? `
    <div class="pr-banner-degraded inline" style="margin-bottom:10px;">
      <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
      <div>${t('profiling.report.power_rapl_unavailable', null, '<strong>RAPL unavailable</strong> — CPU pkg / DRAM power missing. Showing GPU-only domains. Re-run with sudo for full breakdown.')}</div>
    </div>
  ` : '';

  return `
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.power_total_h', null, 'Total power broken down per domain'))}</h2>
      ${partialBanner}
      <div class="pr-stacked-chart-wrap">
        ${renderStackedArea(seriesList, { width: 920, height: 220, colors })}
        <div class="pr-stacked-legend">${legend}</div>
      </div>
    </section>

    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.power_energy_h', null, 'Energy budget'))}</h2>
      <div class="pr-kpi-grid">
        <div class="pr-kpi-tile"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.power_total_energy', null, 'Total energy'))}</span></div><div class="pr-kpi-value">${power.totalKj.toFixed(1)} kJ</div><div class="pr-kpi-sub">${escape(t('profiling.report.power_total_energy_sub', { dur: formatDurationNs(report.duration_ns) }, `over ${formatDurationNs(report.duration_ns)}`))}</div></div>
        <div class="pr-kpi-tile"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.power_energy_label', null, 'Energy'))}</span></div><div class="pr-kpi-value">${(kWh * 1000).toFixed(1)} Wh</div><div class="pr-kpi-sub">${escape(t('profiling.report.power_energy_sub', { val: kWh.toFixed(3) }, `= ${kWh.toFixed(3)} kWh`))}</div></div>
        <div class="pr-kpi-tile"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.power_estimated_cost', null, 'Estimated cost'))}</span></div><div class="pr-kpi-value">$${cost.toFixed(4)}</div><div class="pr-kpi-sub">${escape(t('profiling.report.power_cost_sub', null, '@ $0.15 / kWh'))}</div></div>
        <div class="pr-kpi-tile"><div class="pr-kpi-head"><span class="pr-kpi-label">${escape(t('profiling.report.power_avg_peak', null, 'Avg / Peak'))}</span></div><div class="pr-kpi-value">${formatPower(power.avgW)}</div><div class="pr-kpi-sub">${t('profiling.report.power_peak_strong', { val: formatPower(power.peakW) }, `peak <strong>${formatPower(power.peakW)}</strong>`)}</div></div>
      </div>
    </section>

    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.power_per_domain_h', null, 'Per-domain mini charts'))}</h2>
      <div class="pr-charts-row pr-charts-row-flexible">${miniCharts}</div>
    </section>
  `;
}

function domainKey(domain) {
  if (typeof domain === 'string') return domain;
  if (domain && typeof domain === 'object') {
    const k = Object.keys(domain)[0];
    if (k === 'Gpu') return `Gpu(${domain.Gpu})`;
    return k || 'Other';
  }
  return 'Other';
}

function domainOrder(key) {
  if (key === 'CpuPkg') return 0;
  if (key === 'CpuCore') return 1;
  if (key === 'Dram') return 2;
  if (key.startsWith('Gpu(')) return 10 + parseInt(key.match(/\d+/)?.[0] || '0', 10);
  if (key === 'Ane') return 100;
  if (key === 'Soc') return 101;
  return 200;
}

// =============================================================================
// Tab: Sources.
// =============================================================================

function renderSourcesTab(ctx) {
  const { report } = ctx;
  const collectors = report.collectors || [];

  // Heurystyka: kolektor w kategorii wymagajacej elewacji + status Used ⇒ sudo
  // dostarczone w czasie sesji. Pozwala uzupelnic kolumne "Reason" dla wierszy
  // USED, zgodnie z mockupem (#12 — sudo provided przy linux.uncore.imc, rapl).
  const elevationHintCategories = new Set(['ram bw', 'power', 'memory bandwidth']);
  const elevationHintIdPatterns = /(rapl|uncore|imc|powermetrics|ebpf|perf\.pmu|perf\.kernel)/i;

  const rows = collectors.map((c) => {
    const s = normalizeCollectorStatus(c.status);
    const statusCls = s.kind === 'used' ? 'ok' : s.kind === 'skipped' ? 'warn' : s.kind === 'failed' ? 'bad' : 'lim';
    const statusLabel = s.kind.toUpperCase();
    const dim = s.kind === 'skipped' || s.kind === 'failed' ? ' class="dim"' : '';
    let reason = s.reason || '';
    if (!reason && s.kind === 'used') {
      const cat = String(c.primary_category || '').toLowerCase();
      if (elevationHintCategories.has(cat) || elevationHintIdPatterns.test(c.id || '')) {
        reason = t('profiling.report.sources_sudo_provided', null, 'sudo provided');
      }
    }
    return `<tr${dim}>
      <td class="mono">${escape(c.id)}</td>
      <td>${escape(c.primary_category || '—')}</td>
      <td><span class="pr-src-status ${statusCls}">${escape(statusLabel)}</span></td>
      <td>${escape(reason || '—')}</td>
      <td class="num mono">${c.samples_collected ? formatInt(c.samples_collected) : '—'}</td>
      <td class="num mono">${c.raw_size_bytes ? formatBytes(c.raw_size_bytes) : '—'}</td>
    </tr>`;
  }).join('');

  const drift = report.drift_report || {};
  const driftMs = (drift.max_observed_drift_ns || 0) / 1e6;
  const tolMs = (drift.tolerance_ns || 5_000_000) / 1e6;
  const driftOk = !drift.exceeded_tolerance;
  const perCollectorClocks = Array.isArray(drift.per_collector) ? drift.per_collector.length : 0;
  // NTP "yes" tylko gdy mamy wiele zrodel czasu i drift jest dobrze wewnatrz tolerancji.
  // W przeciwnym razie nie wiemy — pokazujemy "unknown" aby nie klamac.
  const ntpKnownOk = perCollectorClocks >= 2 && drift.max_observed_drift_ns < (drift.tolerance_ns || 5_000_000) / 2;

  // Klasyfikacja kolektorow wedlug rodzaju powodu pominiecia. Bierzemy reason
  // z normalizeCollectorStatus oraz oryginalny status enum (SkippedRequiresElevation).
  let skippedElevation = 0;
  let skippedUnavailable = 0;
  let failed = 0;
  let used = 0;
  for (const c of collectors) {
    const s = normalizeCollectorStatus(c.status);
    if (s.kind === 'used') { used += 1; continue; }
    if (s.kind === 'failed') { failed += 1; continue; }
    if (s.kind !== 'skipped') continue;
    const reason = (s.reason || '').toLowerCase();
    if (reason === 'requires elevation' || reason.includes('cap_') || reason.includes('sudo') || reason.includes('admin')) {
      skippedElevation += 1;
    } else {
      skippedUnavailable += 1;
    }
  }
  const totalSkipped = skippedElevation + skippedUnavailable;

  const sudoUsed = used > 0 && collectors.some((c) => {
    if (normalizeCollectorStatus(c.status).kind !== 'used') return false;
    const cat = String(c.primary_category || '').toLowerCase();
    return elevationHintCategories.has(cat) || elevationHintIdPatterns.test(c.id || '');
  });

  const capPerfmonCls = skippedElevation > 0 ? 'warn' : 'muted';
  const capPerfmonText = skippedElevation > 0 ? t('profiling.report.sources_cap_not_set', null, 'not set') : t('profiling.report.sources_cap_unknown', null, 'unknown');
  const capBpfCls = skippedElevation > 0 ? 'warn' : 'muted';
  const capBpfText = capPerfmonText;

  const rerunBtn = totalSkipped > 0 || failed > 0
    ? `<tf-button variant="ghost" size="sm" icon="refresh" data-action="rerun" aria-label="${escape(t('profiling.report.sources_rerun_aria', null, 'Re-run with elevation'))}">${escape(t('profiling.report.sources_rerun_btn', null, 'Re-run with elevated permissions'))}</tf-button>`
    : '';

  // Drift summary alert: kontener .pr-alert ma teraz strukture .pr-alert-icon
  // + .pr-alert-body, identyczna jak .alert-box w mockupie #12.
  const alertCls = driftOk ? 'ok' : 'bad';
  const alertIcon = driftOk
    ? `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 13l4 4L19 7"/></svg>`
    : `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 8v4M12 16h.01"/></svg>`;
  const alertBody = driftOk
    ? t('profiling.report.sources_drift_ok', null, '<strong>Within tolerance</strong> — cross-source correlation reliable.')
    : t('profiling.report.sources_drift_bad', null, '<strong>Exceeded tolerance</strong> — cross-source correlation unreliable; re-collect with NTP-synced clocks.');

  return `
    <section class="pr-card">
      <h2 class="pr-card-title">${escape(t('profiling.report.sources_collectors_h', null, 'Collectors used'))}</h2>
      <table class="pr-table">
        <thead><tr><th>${escape(t('profiling.report.col_id', null, 'ID'))}</th><th>${escape(t('profiling.report.col_category', null, 'Category'))}</th><th>${escape(t('profiling.report.col_status', null, 'Status'))}</th><th>${escape(t('profiling.report.col_reason', null, 'Reason'))}</th><th class="num">${escape(t('profiling.report.col_samples', null, 'Samples'))}</th><th class="num">${escape(t('profiling.report.col_raw_size', null, 'Raw size'))}</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>
    </section>

    <div class="pr-two-col">
      <section class="pr-card">
        <h2 class="pr-card-title">${escape(t('profiling.report.sources_privilege_h', null, 'Privilege summary'))}</h2>
        <div class="pr-sp-list">
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_sudo_label', null, 'Sudo provided'))}</span><span class="pct ${sudoUsed ? 'ok' : 'warn'}">${escape(sudoUsed ? t('profiling.report.sources_sudo_yes', null, 'yes (test passed)') : t('profiling.report.sources_sudo_no', null, 'no / not tested'))}</span></div>
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_admin_label', null, 'Admin (Windows)'))}</span><span class="pct muted">${escape(t('profiling.report.sources_admin_na', null, 'n/a'))}</span></div>
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_cap_perfmon', null, 'cap_perfmon'))}</span><span class="pct ${capPerfmonCls}">${escape(capPerfmonText)}</span></div>
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_cap_bpf', null, 'cap_bpf'))}</span><span class="pct ${capBpfCls}">${escape(capBpfText)}</span></div>
        </div>
        ${rerunBtn}
      </section>

      <section class="pr-card">
        <h2 class="pr-card-title">${escape(t('profiling.report.sources_drift_h', null, 'Drift report'))}</h2>
        <div class="pr-sp-list">
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_max_drift', null, 'Max clock drift'))}</span><span class="pct">${driftMs.toFixed(2)} ms</span></div>
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_tolerance', null, 'Tolerance'))}</span><span class="pct">${tolMs.toFixed(1)} ms</span></div>
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_reference', null, 'Reference'))}</span><span class="pct">CLOCK_MONOTONIC_RAW</span></div>
          <div class="sp-item"><span class="sym">${escape(t('profiling.report.sources_ntp_synced', null, 'NTP-synced'))}</span><span class="pct ${ntpKnownOk ? 'ok' : 'muted'}">${escape(ntpKnownOk ? t('profiling.report.sources_ntp_yes', null, 'yes') : t('profiling.report.sources_ntp_unknown', null, 'unknown'))}</span></div>
        </div>
        <div class="pr-alert ${alertCls}">
          <span class="pr-alert-icon">${alertIcon}</span>
          <span class="pr-alert-body">${alertBody}</span>
        </div>
      </section>
    </div>
  `;
}

// =============================================================================
// Misc helpers.
// =============================================================================

function noDataCard(msg) {
  return `<section class="pr-card"><div class="pr-banner-degraded"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg><div>${escape(msg)}</div></div></section>`;
}

// ---- Inline SVG icons (small, monochrome) -----------------------------------

function iconCpu()    { return `<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="6" width="12" height="12" rx="1"/><path d="M9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3"/></svg>`; }
function iconRam()    { return `<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="6" width="18" height="12" rx="1"/></svg>`; }
function iconDisk()   { return `<svg viewBox="0 0 24 24" aria-hidden="true"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/></svg>`; }
function iconPower()  { return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2L4 7v10l8 5 8-5V7l-8-5z"/></svg>`; }
function iconNet()    { return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="3"/><circle cx="4" cy="4" r="1.5"/><circle cx="20" cy="4" r="1.5"/><circle cx="4" cy="20" r="1.5"/><circle cx="20" cy="20" r="1.5"/></svg>`; }
function iconFinding(kind) {
  if (kind === 'info') return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>`;
  if (kind === 'bad')  return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 9v4M12 17h.01"/></svg>`;
  if (kind === 'ok')   return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 13l4 4L19 7"/></svg>`;
  return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 9v4M12 17h.01"/><path d="M10.3 3.86l-8.06 14a2 2 0 0 0 1.7 3h16.12a2 2 0 0 0 1.7-3l-8.06-14a2 2 0 0 0-3.4 0z"/></svg>`;
}

// Mockup #04: NVIDIA pokazuje "Peak SM" + nazwe konkretnego kernela; AMD
// "Peak compute" + ostatnia gorna funkcja rocBLAS; Intel/Apple "No kernel
// detail" bo zaden runtime nie strumieniuje per-kernel telemetrii do
// collectorow w aktualnej generacji.
function vendorPeakLabel(vendorCls, k) {
  const hasKernel = k && k.topKernel;
  if (vendorCls === 'nv') {
    return hasKernel
      ? t('profiling.report.kpi_peak_gpu_sub_nv', { kernel: escape(k.topKernel), pct: formatPct(k.topPct) }, `Peak SM · Top kernel <strong>${escape(k.topKernel)} ${formatPct(k.topPct)}</strong>`)
      : t('profiling.report.kpi_peak_gpu_sub_nv_no', null, 'Peak SM · <strong>No kernel detail</strong>');
  }
  if (vendorCls === 'amd') {
    return hasKernel
      ? t('profiling.report.kpi_peak_gpu_sub_amd', { kernel: escape(k.topKernel), pct: formatPct(k.topPct) }, `Peak compute · Top <strong>${escape(k.topKernel)} ${formatPct(k.topPct)}</strong>`)
      : t('profiling.report.kpi_peak_gpu_sub_amd_no', null, 'Peak compute · <strong>No kernel detail</strong>');
  }
  return t('profiling.report.kpi_peak_gpu_sub_other', null, 'Peak compute · <strong>No kernel detail</strong>');
}
