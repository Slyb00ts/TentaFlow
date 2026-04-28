// =============================================================================
// File: modules/profile-report-v2.js
// Purpose: Multi-source Profile Report V2 screen. Renders a session report
//          with dynamic tabs (Overview / Unified Timeline / CPU Flamegraph /
//          GPU per-vendor / Memory / Disk / Power / Sources). Tabs hide when
//          their underlying event categories are absent from the report.
//          Falls back to legacy `profile-report.js` for V1 reports.
// =============================================================================

import {
  expandCompactSeries,
  groupEventsByCategory,
  eventsForCategory,
  eventsForDevice,
  uniqueDevices,
  unwrapPayload,
  computeKpiCpu,
  computeKpiGpu,
  computeKpiPower,
  computeKpiRam,
  computeKpiDisk,
  computeKpiNetwork,
  aggregateKernels,
  aggregateApiCalls,
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
  profilingDelete,
  profilingDownload,
} from '/js/protocol/profiling.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-window.js';

const FIXTURE_PATH = '/js/modules/__fixtures__/profile-report.json';

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

// Fallback to the legacy V1 view when the report is V1Legacy.
async function renderLegacyV1(container, params) {
  const { default: ProfileReportScreen } = await import('/js/modules/profile-report.js');
  // Legacy module uses `document.getElementById('main')` directly; we reuse its
  // show() entry point, but make sure the container points at #main.
  if (container && container.id !== 'main') {
    container.innerHTML = '';
  }
  await ProfileReportScreen.show(params);
}

// =============================================================================
// Public API.
// =============================================================================

export class ProfileReportV2View {
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

    // Envelope detection. Binary protocol returns { envelope: { kind, report }};
    // fixture JSON may carry { V2: ... } / { V1Legacy: ... } / a bare V2.
    let report = raw;
    if (raw && typeof raw === 'object') {
      if (raw.envelope && typeof raw.envelope === 'object') {
        if (raw.envelope.kind === 'v1_legacy') {
          await renderLegacyV1(container, { sessionId, nodeId });
          return;
        }
        if (raw.envelope.kind === 'v2' && raw.envelope.report) {
          report = raw.envelope.report;
        }
      } else if ('V2' in raw && raw.V2) {
        report = raw.V2;
      } else if ('V1Legacy' in raw && raw.V1Legacy) {
        await renderLegacyV1(container, { sessionId, nodeId });
        return;
      }
    }
    if (!report || report.schema_version !== 2) {
      // Attempt legacy fallback if not V2.
      try {
        await renderLegacyV1(container, { sessionId, nodeId });
        return;
      } catch (err) {
        container.innerHTML = renderError(new Error('Unsupported report schema'));
        bindBackHandler(container);
        return;
      }
    }

    const expanded = expandCompactSeries(report);
    const ctx = buildContext(expanded);
    container.innerHTML = renderShell(ctx);
    bindShell(container, ctx);
    renderTab(container, ctx, ctx.defaultTab);
  }
}

export default ProfileReportV2View;

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

  const tabs = [
    { id: 'overview',  label: 'Overview',       icon: 'grid',     visible: true },
    { id: 'timeline',  label: 'Unified Timeline', icon: 'timeline', visible: true },
    { id: 'flame',     label: 'CPU Flamegraph', icon: 'bars',     visible: true },
    { id: 'gpu',       label: 'GPU',            icon: 'cpu',      visible: hasGpu, count: devices.length || undefined },
    { id: 'memory',    label: 'Memory',         icon: 'memory',   visible: hasMemory },
    { id: 'disk',      label: 'Disk IO',        icon: 'disk',     visible: hasDisk },
    { id: 'power',     label: 'Power',          icon: 'power',    visible: hasPower },
    { id: 'sources',   label: 'Sources',        icon: 'list',     visible: true },
  ].filter((t) => t.visible);

  return {
    report,
    events,
    grouped,
    devices,
    tabs,
    defaultTab: 'overview',
    counts: countUsedCollectors(report.collectors || []),
    hasGpu, hasDisk, hasPower, hasMemory, hasNetwork,
  };
}

// =============================================================================
// Shell (header + tabs strip + body container).
// =============================================================================

function renderShell(ctx) {
  const { report, counts } = ctx;
  const startedAt = formatDateTime(report.t0_wallclock_unix_ns);
  const rel = relativeTime(report.t0_wallclock_unix_ns);
  const dur = formatDurationNs(report.duration_ns);
  const scopeLabel = escape(report.scope?.label || '—');
  const sessionShort = escape((report.session_id || '').slice(0, 8));
  const node = escape(report.node_id || '—');
  const sourcesPill = `${counts.used}/${counts.total} sources used`;
  const sourcesPillCls = counts.skipped > 0 || counts.failed > 0 ? 'warn' : 'ok';

  const breadcrumb = `
    <nav class="pr2-breadcrumb" aria-label="Breadcrumb">
      <a href="#" data-action="back-mesh">Mesh</a>
      <span class="sep">/</span>
      <a href="#" data-action="back-mesh">Nodes</a>
      <span class="sep">/</span>
      <a href="#" data-action="back-mesh">${node}</a>
      <span class="sep">/</span>
      <span>Profiling</span>
      <span class="sep">/</span>
      <span class="mono">${sessionShort}</span>
    </nav>
  `;

  const headerActions = `
    <span class="pr2-status-pill ${sourcesPillCls}" role="status">
      ${counts.skipped > 0 || counts.failed > 0
        ? `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M12 2L2 22h20L12 2z"/><path d="M12 9v6M12 18h.01"/></svg>`
        : `<svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="M5 13l4 4L19 7"/></svg>`}
      ${escape(sourcesPill)}
    </span>
    <tf-button variant="ghost" size="sm" data-action="refresh" aria-label="Refresh report">Refresh</tf-button>
    <tf-button variant="ghost" size="sm" data-action="download" aria-label="Download report">Download</tf-button>
    <tf-button variant="danger" size="sm" data-action="delete" aria-label="Delete report">Delete</tf-button>
  `;

  const tabsHtml = ctx.tabs.map((t) => {
    const countAttr = t.count ? ` count="${t.count}"` : '';
    return `<tf-tab id="${escape(t.id)}"${countAttr}>${escape(t.label)}</tf-tab>`;
  }).join('');

  return `
    <div class="profile-report-v2">
      ${breadcrumb}

      <header class="pr2-header">
        <div class="pr2-header-main">
          <h1 class="pr2-title">${scopeLabel}</h1>
          <div class="pr2-meta">
            <span class="mono">session ${sessionShort}</span>
            <span class="sep">·</span>
            <span>${escape(startedAt)}</span>
            ${rel ? `<span class="sep">·</span><span class="muted">${escape(rel)}</span>` : ''}
            <span class="sep">·</span>
            <span>${escape(dur)}</span>
            <span class="sep">·</span>
            <span class="mono">${node}</span>
          </div>
        </div>
        <div class="pr2-header-actions">
          ${headerActions}
        </div>
      </header>

      <div class="pr2-tabs-wrap">
        <tf-tabs variant="underline" value="${escape(ctx.defaultTab)}" id="pr2-tabs">
          ${tabsHtml}
        </tf-tabs>
      </div>

      <main class="pr2-body" id="pr2-body" role="region" aria-label="Report body"></main>
    </div>
  `;
}

function renderSkeleton() {
  return `
    <div class="profile-report-v2 pr2-loading">
      <div class="pr2-skeleton" style="width:60%; height:24px;"></div>
      <div class="pr2-skeleton" style="width:100%; height:80px; margin-top:14px;"></div>
      <div class="pr2-skeleton" style="width:100%; height:200px; margin-top:14px;"></div>
    </div>
  `;
}

function renderError(err) {
  const msg = err?.message || String(err || 'Unknown error');
  return `
    <div class="profile-report-v2 pr2-error">
      <div class="pr2-error-card">
        <h2>Failed to load report</h2>
        <pre class="mono">${escape(msg)}</pre>
        <tf-button variant="ghost" size="sm" data-action="back-mesh">Back to Mesh</tf-button>
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
    } else if (action === 'refresh') {
      ProfileReportV2View.render(container, {
        sessionId: ctx.report.session_id,
        nodeId: ctx.report.node_id,
      });
    } else if (action === 'download') {
      handleDownload(ctx);
    } else if (action === 'delete') {
      handleDelete(container, ctx);
    } else if (action === 'open-timeline') {
      const tabs = container.querySelector('#pr2-tabs');
      if (tabs) tabs.value = 'timeline';
    } else if (action === 'rerun') {
      handleRerun(ctx);
    }
  });

  // Tab switching.
  const tabsEl = container.querySelector('#pr2-tabs');
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

async function handleDelete(container, ctx) {
  if (fixtureMode()) {
    navigateBack();
    return;
  }
  try {
    await profilingDelete({
      nodeId: ctx.report.node_id || '',
      sessionId: ctx.report.session_id,
    });
    navigateBack();
  } catch (err) {
    console.error('delete failed', err);
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
  const body = container.querySelector('#pr2-body');
  if (!body) return;
  switch (tabId) {
    case 'overview': body.innerHTML = renderOverviewTab(ctx); break;
    case 'timeline': renderLazyTab(body, '/js/modules/profile-timeline.js', ctx, 'TimelineView'); break;
    case 'flame':    renderLazyTab(body, '/js/modules/profile-flamegraph.js', ctx, 'FlamegraphView'); break;
    case 'gpu':      body.innerHTML = renderGpuTab(ctx); bindGpuTab(body, ctx); break;
    case 'memory':   body.innerHTML = renderMemoryTab(ctx); break;
    case 'disk':     body.innerHTML = renderDiskTab(ctx); break;
    case 'power':    body.innerHTML = renderPowerTab(ctx); break;
    case 'sources':  body.innerHTML = renderSourcesTab(ctx); break;
    default:         body.innerHTML = '';
  }
}

// Lazy-load tabs (Timeline + Flamegraph) implemented by sibling agents. While
// loading we show a spinner; if the module is missing we render an info
// banner so the rest of the report stays usable.
async function renderLazyTab(host, modulePath, ctx, exportName) {
  host.innerHTML = `<div class="pr2-card pr2-loading-card"><div class="pr2-skeleton" style="height:24px;width:50%;"></div><div class="pr2-skeleton" style="height:200px;margin-top:14px;"></div></div>`;
  try {
    const mod = await import(modulePath);
    const ViewClass = mod[exportName] || mod.default;
    if (ViewClass && typeof ViewClass.render === 'function') {
      await ViewClass.render(host, ctx);
      return;
    }
    host.innerHTML = pendingModuleBanner(modulePath, 'export missing');
  } catch (err) {
    host.innerHTML = pendingModuleBanner(modulePath, err?.message || 'module unavailable');
  }
}

function pendingModuleBanner(modulePath, reason) {
  return `
    <div class="pr2-card">
      <div class="pr2-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div>
          <strong>Tab not yet available.</strong>
          The module <code class="mono">${escape(modulePath)}</code> is being implemented separately
          (${escape(reason)}). Other tabs of this report are fully functional.
        </div>
      </div>
    </div>
  `;
}

// =============================================================================
// Tab: Overview.
// =============================================================================

function renderOverviewTab(ctx) {
  const { events, devices, report, hasMemory, hasDisk, hasPower, hasNetwork } = ctx;
  const names = report.names || [];
  const cpu = computeKpiCpu(events, names);
  const ram = computeKpiRam(events);
  const disk = computeKpiDisk(events);
  const power = computeKpiPower(events, report.duration_ns);
  const net = computeKpiNetwork(events);

  const gpuTiles = devices.slice(0, 3).map((d) => {
    const k = computeKpiGpu(events, d.device_id, names);
    const badge = vendorBadge(d.vendor);
    const topKernel = k.topKernel ? `${escape(k.topKernel)} ${formatPct(k.topPct)}` : '<span class="muted">no kernel data</span>';
    return `
      <div class="pr2-kpi-tile ${badge.cls}">
        <div class="pr2-kpi-head">
          <span class="pr2-kpi-ico"><span class="pr2-vendor-badge ${badge.cls}">${escape(badge.label)}</span></span>
          <span class="pr2-kpi-label">GPU ${d.device_id} — ${escape(d.name)}</span>
        </div>
        <div class="pr2-kpi-value">${formatPct(k.peakCompute, 0)}</div>
        <div class="pr2-kpi-sub">Peak compute · Top: <strong>${topKernel}</strong></div>
      </div>
    `;
  }).join('');

  const cpuTile = `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconCpu()}</span><span class="pr2-kpi-label">CPU</span></div>
      <div class="pr2-kpi-value">${formatPct(cpu.avgUtil, 0)}</div>
      <div class="pr2-kpi-sub">Avg util · Peak <strong>${formatPct(cpu.peakUtil, 0)}</strong> · Top: <strong>${escape(cpu.topSymbol)}</strong></div>
    </div>
  `;
  const ramTile = hasMemory ? `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconRam()}</span><span class="pr2-kpi-label">RAM</span></div>
      <div class="pr2-kpi-value">${formatBytes(ram.peakUsedBytes)}</div>
      <div class="pr2-kpi-sub">Peak used · BW peak <strong>${formatBytesPerSec(ram.peakBwBps)}</strong></div>
    </div>
  ` : '';
  const diskTile = hasDisk ? `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconDisk()}</span><span class="pr2-kpi-label">Disk</span></div>
      <div class="pr2-kpi-value">${formatBytesPerSec(disk.peakReadBps)}</div>
      <div class="pr2-kpi-sub">Peak R · W <strong>${formatBytesPerSec(disk.peakWriteBps)}</strong> · p99 await <strong>${disk.p99AwaitMs.toFixed(1)} ms</strong></div>
    </div>
  ` : '';
  const powerTile = hasPower ? `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconPower()}</span><span class="pr2-kpi-label">Power</span></div>
      <div class="pr2-kpi-value">${formatPower(power.avgW)}</div>
      <div class="pr2-kpi-sub">Avg · Peak <strong>${formatPower(power.peakW)}</strong> · Total <strong>${power.totalKj.toFixed(1)} kJ</strong></div>
    </div>
  ` : '';
  const netTile = hasNetwork ? `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconNet()}</span><span class="pr2-kpi-label">Network</span></div>
      <div class="pr2-kpi-value">${formatBytesPerSec(net.peakRxBps)}</div>
      <div class="pr2-kpi-sub">Peak in · Peak out <strong>${formatBytesPerSec(net.peakTxBps)}</strong></div>
    </div>
  ` : '';
  const wallTile = `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconClock()}</span><span class="pr2-kpi-label">Wallclock</span></div>
      <div class="pr2-kpi-value">${formatDurationNs(report.duration_ns)}</div>
      <div class="pr2-kpi-sub">Sources <strong>${ctx.counts.used} of ${ctx.counts.total}</strong></div>
    </div>
  `;

  // CPU counters tile only when CpuCounter events exist (not in fixture).
  const counterEvents = eventsForCategory(events, 'CpuCounter');
  const counterTile = counterEvents.length > 0 ? `
    <div class="pr2-kpi-tile">
      <div class="pr2-kpi-head"><span class="pr2-kpi-ico">${iconCpu()}</span><span class="pr2-kpi-label">CPU Counters</span></div>
      <div class="pr2-kpi-value">${counterEvents.length}</div>
      <div class="pr2-kpi-sub">PMU samples</div>
    </div>
  ` : '';

  const findings = buildQuickFindings(events, devices, report.duration_ns, names);
  const findingsHtml = findings.length === 0 ? `<div class="muted">No notable findings.</div>` : findings.map((f) => `
    <div class="pr2-finding-card ${escape(f.kind)}">
      <span class="f-ico">${iconFinding(f.kind)}</span>
      <div class="f-body">
        <div class="f-title">${escape(f.title)}</div>
        <div class="f-detail">${escape(f.detail)}</div>
      </div>
    </div>
  `).join('');

  // Mini timeline preview: 4 lanes (CPU util, GPU0..2 compute, total power).
  const lanes = [];
  lanes.push({ label: 'CPU', color: '#a78bfa', bg: 'rgba(167,139,250,0.05)', points: buildTimeSeries(events, 'CpuUtil', null, 'util_pct') });
  for (const d of devices) {
    const badge = vendorBadge(d.vendor);
    const colors = { nv: '#76b900', amd: '#ed1c24', intel: '#0071c5', apple: '#d4d4d8' };
    lanes.push({
      label: `GPU${d.device_id}`,
      color: colors[badge.cls] || '#a78bfa',
      bg: 'rgba(255,255,255,0.02)',
      points: buildTimeSeries(events, 'GpuUtilSample', d.device_id, 'compute_pct'),
    });
  }
  if (hasPower) {
    // Sum tick-wise across all PowerSample events for the preview lane.
    const totals = new Map();
    for (const e of eventsForCategory(events, 'PowerSample')) {
      const p = unwrapPayload(e.payload);
      if (!p) continue;
      totals.set(e.t_start_ns, (totals.get(e.t_start_ns) || 0) + p.watts);
    }
    const arr = Array.from(totals.entries()).map(([t, v]) => [t, v]).sort((a, b) => a[0] - b[0]);
    lanes.push({ label: 'PWR', color: '#f59e0b', bg: 'rgba(245,158,11,0.04)', points: arr });
  }

  return `
    <section class="pr2-card">
      <h2 class="pr2-card-title">Headline metrics</h2>
      <div class="pr2-kpi-grid">
        ${cpuTile}
        ${gpuTiles}
        ${ramTile}
        ${diskTile}
        ${powerTile}
        ${counterTile}
        ${netTile}
        ${wallTile}
      </div>
    </section>

    <section class="pr2-card">
      <h2 class="pr2-card-title">Quick findings</h2>
      <div class="pr2-findings-stack">${findingsHtml}</div>
    </section>

    <section class="pr2-card">
      <h2 class="pr2-card-title">
        Timeline preview
        <span class="pr2-card-actions"><tf-button variant="ghost" size="sm" data-action="open-timeline">Open Unified Timeline →</tf-button></span>
      </h2>
      <div class="pr2-timeline-preview">${renderRidgelinePreview(lanes, { width: 920, height: 140 })}</div>
    </section>
  `;
}

// =============================================================================
// Tab: GPU (unified per-vendor).
// =============================================================================

function renderGpuTab(ctx) {
  const { devices } = ctx;
  if (devices.length === 0) {
    return noDataCard('No GPU data collected for this session.');
  }
  // Vendor sub-tabs (chips). First device active by default.
  const subTabs = devices.map((d, idx) => {
    const badge = vendorBadge(d.vendor);
    return `
      <button type="button" class="pr2-vendor-tab" data-gpu-tab="${d.device_id}" ${idx === 0 ? 'data-active="true"' : ''} role="tab" aria-selected="${idx === 0 ? 'true' : 'false'}">
        <span class="pr2-vendor-badge ${badge.cls}">${escape(badge.label)}</span>
        <span>GPU ${d.device_id} — ${escape(d.name)}</span>
      </button>
    `;
  }).join('');

  const cards = devices.map((d, idx) => `
    <div class="pr2-gpu-device-pane" data-gpu-pane="${d.device_id}" ${idx === 0 ? '' : 'hidden'}>
      ${renderGpuDeviceCard(ctx, d)}
    </div>
  `).join('');

  return `
    <section class="pr2-card">
      <div class="pr2-vendor-tabs" role="tablist" aria-label="GPU device">${subTabs}</div>
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
  const { events, report } = ctx;
  const names = report.names || [];
  const k = computeKpiGpu(events, d.device_id, names);
  const badge = vendorBadge(d.vendor);

  const collectorStatus = d.limited
    ? `<span class="pr2-status-pill warn">Limited (${escape(d.collector || 'unknown')})</span>`
    : `<span class="pr2-status-pill ok">${escape(d.collector || 'collector')}</span>`;

  const computeSeries = buildTimeSeries(events, 'GpuUtilSample', d.device_id, 'compute_pct');
  const memSeries = buildTimeSeries(events, 'GpuUtilSample', d.device_id, 'mem_pct');
  const powerSeries = buildPowerSeriesForGpu(events, d.device_id);

  const colors = { nv: '#76b900', amd: '#ed1c24', intel: '#0071c5', apple: '#d4d4d8' };
  const color = colors[badge.cls] || '#a78bfa';

  // Memory chart: Apple Silicon → unified memory banner; otherwise mem%.
  const memChart = d.vendor === 'apple'
    ? `<div class="pr2-mini-chart"><div class="mc-title"><span>Memory %</span><span class="v">unified</span></div><div class="pr2-banner-degraded inline"><div>Unified memory — see RAM tab for combined pressure.</div></div></div>`
    : `<div class="pr2-mini-chart"><div class="mc-title"><span>Memory %</span><span class="v">${formatPct(k.peakMem, 0)} max</span></div>${renderLineChart(memSeries, { color, height: 60, ariaLabel: 'GPU memory utilization' })}</div>`;

  const charts = `
    <div class="pr2-charts-row">
      <div class="pr2-mini-chart">
        <div class="mc-title"><span>Compute %</span><span class="v">${formatPct(k.peakCompute, 0)} max</span></div>
        ${renderLineChart(computeSeries, { color, height: 60, ariaLabel: 'GPU compute utilization' })}
      </div>
      ${memChart}
      <div class="pr2-mini-chart">
        <div class="mc-title"><span>Power W</span><span class="v">${formatPower(k.peakW)} max</span></div>
        ${renderLineChart(powerSeries, { color: '#f59e0b', height: 60, ariaLabel: 'GPU power' })}
      </div>
    </div>
  `;

  // Limited / no-kernel banner.
  const degradedBanner = (() => {
    if (d.vendor === 'intel') {
      return `<div class="pr2-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div><strong>Kernel-level metrics not available on this platform.</strong> Engine utilization captured via <code class="mono">intel_gpu_top</code>. Install <code class="mono">Intel GPA</code> or <code class="mono">VTune Profiler</code> for kernel traces.</div>
      </div>`;
    }
    if (d.vendor === 'apple') {
      return `<div class="pr2-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div><strong>Apple Silicon — utilization and power only.</strong> For kernel-level insight, capture a Metal trace via <code class="mono">Xcode Instruments</code>.</div>
      </div>`;
    }
    return '';
  })();

  // KPI row.
  const memKpi = d.vendor === 'apple'
    ? `<div class="pr2-kpi-tile ${badge.cls}"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Peak mem</span></div><div class="pr2-kpi-value pr2-kpi-text-sm">unified</div><div class="pr2-kpi-sub">shared with CPU</div></div>`
    : `<div class="pr2-kpi-tile ${badge.cls}"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Peak mem</span></div><div class="pr2-kpi-value">${formatPct(k.peakMem, 0)}</div><div class="pr2-kpi-sub">VRAM ${formatBytes(k.memUsedBytes)}${d.memTotalBytes ? ' / ' + formatBytes(d.memTotalBytes) : ''}</div></div>`;

  const topKernelKpi = k.topKernel
    ? `<div class="pr2-kpi-tile ${badge.cls}"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Top kernel</span></div><div class="pr2-kpi-value pr2-kpi-text-sm">${escape(k.topKernel)}</div><div class="pr2-kpi-sub"><strong>${formatPct(k.topPct)}</strong> total time</div></div>`
    : `<div class="pr2-kpi-tile ${badge.cls}"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Top kernel</span></div><div class="pr2-kpi-value pr2-kpi-text-sm muted">no kernel data</div><div class="pr2-kpi-sub">${d.vendor === 'intel' ? 'use Intel GPA' : 'use Metal trace'}</div></div>`;

  const kpiRow = `
    <div class="pr2-gpu-kpi-row">
      <div class="pr2-kpi-tile ${badge.cls}"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Peak compute</span></div><div class="pr2-kpi-value">${formatPct(k.peakCompute, 0)}</div><div class="pr2-kpi-sub">${escape(d.vendor === 'amd' ? 'CU utilization' : d.vendor === 'intel' ? 'render+compute' : 'SM utilization')}</div></div>
      ${memKpi}
      ${topKernelKpi}
      <div class="pr2-kpi-tile ${badge.cls}"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Power avg</span></div><div class="pr2-kpi-value">${formatPower(k.avgW)}</div><div class="pr2-kpi-sub">Peak <strong>${formatPower(k.peakW)}</strong></div></div>
    </div>
  `;

  // Kernels table.
  const kernels = aggregateKernels(events, d.device_id, names);
  const kernelTable = kernels.length === 0
    ? `<div class="pr2-banner-degraded inline"><div>${d.vendor === 'apple' ? 'Per-kernel timings require Metal capture (Xcode Instruments).' : d.vendor === 'intel' ? 'Per-kernel timings require Intel GPA / Level Zero tracer.' : 'No kernel data captured.'}</div></div>`
    : `<table class="pr2-table">
        <thead><tr><th>Kernel</th><th class="num">Count</th><th class="num">Total ms</th><th class="num">Avg µs</th><th class="num">%</th></tr></thead>
        <tbody>${kernels.slice(0, 30).map((r) => `<tr><td class="mono">${escape(r.name)}</td><td class="num mono">${formatInt(r.count)}</td><td class="num mono">${(r.totalNs / 1e6).toFixed(1)}</td><td class="num mono">${(r.avgNs / 1e3).toFixed(1)}</td><td class="num mono">${formatPct(r.pct)}</td></tr>`).join('')}</tbody>
      </table>`;

  // API calls table.
  const apis = aggregateApiCalls(events, d.device_id, names);
  const apiHeader = d.vendor === 'amd' ? 'HIP APIs' : d.vendor === 'intel' ? 'Level Zero APIs' : d.vendor === 'apple' ? 'Metal calls' : 'CUDA APIs';
  const apiTable = apis.length === 0
    ? `<div class="pr2-banner-degraded inline"><div>${d.vendor === 'apple' ? 'Use Xcode Instruments for Metal API tracing.' : 'No API call data captured.'}</div></div>`
    : `<table class="pr2-table">
        <thead><tr><th>API</th><th class="num">Calls</th><th class="num">Total ms</th></tr></thead>
        <tbody>${apis.slice(0, 30).map((r) => `<tr><td class="mono">${escape(r.name)}</td><td class="num mono">${formatInt(r.count)}</td><td class="num mono">${r.totalNs == null ? '<span class="muted">limited</span>' : (r.totalNs / 1e6).toFixed(1)}</td></tr>`).join('')}</tbody>
      </table>`;

  // Memory transfer chart / banner.
  const transferBlock = d.vendor === 'apple'
    ? `<div class="pr2-banner-degraded"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/></svg><div><strong>Unified memory architecture</strong> — no explicit host/device transfers on Apple Silicon.</div></div>`
    : renderTransferChart(d, color);

  return `
    <article class="pr2-gpu-device-card ${badge.cls}">
      <header class="gd-head">
        <span class="pr2-vendor-badge ${badge.cls}">${escape(badge.label)}</span>
        <div class="gd-name">GPU ${d.device_id} — ${escape(d.name)}</div>
        ${d.version ? `<span class="gd-id mono">${escape(d.version)}</span>` : ''}
        <span class="gd-status">${collectorStatus}</span>
      </header>

      ${degradedBanner}
      ${kpiRow}
      ${charts}

      <h3 class="pr2-subhead">Top kernels</h3>
      ${kernelTable}

      <h3 class="pr2-subhead">${escape(apiHeader)}</h3>
      ${apiTable}

      <h3 class="pr2-subhead">Memory transfer (D2H / H2D / D2D)</h3>
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

function renderTransferChart(d, color) {
  if (!d.transfers) {
    return `<div class="pr2-banner-degraded inline"><div>Transfer telemetry not collected for this device.</div></div>`;
  }
  const { h2d_bytes_per_s_peak, d2h_bytes_per_s_peak, d2d_bytes_per_s_peak } = d.transfers;
  const peak = Math.max(h2d_bytes_per_s_peak || 0, d2h_bytes_per_s_peak || 0, d2d_bytes_per_s_peak || 0);
  // We synthesize a small bar-style chart from the three peak values since
  // per-tick transfer events are not in the fixture.
  const w = 600; const h = 60;
  const bars = ['H2D', 'D2H', 'D2D'].map((label, i) => {
    const v = [h2d_bytes_per_s_peak, d2h_bytes_per_s_peak, d2d_bytes_per_s_peak][i] || 0;
    const barW = (v / peak) * (w - 80);
    return `<g><text x="0" y="${18 + i * 18}" font-family="JetBrains Mono" font-size="10" fill="#a0a8c8">${label}</text><rect x="40" y="${10 + i * 18}" width="${barW.toFixed(0)}" height="10" fill="${color}" opacity="${0.9 - i * 0.2}"/><text x="${44 + barW}" y="${18 + i * 18}" font-family="JetBrains Mono" font-size="9" fill="#e8ebf5">${formatBytesPerSec(v)}</text></g>`;
  }).join('');
  return `<div class="pr2-mini-chart"><div class="mc-title"><span>Peak rates</span><span class="v">peak ${formatBytesPerSec(peak)}</span></div><svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="GPU transfer peaks">${bars}</svg></div>`;
}

// =============================================================================
// Tab: Memory.
// =============================================================================

function renderMemoryTab(ctx) {
  const { events } = ctx;
  if (eventsForCategory(events, 'RamSample').length === 0) {
    return noDataCard('No memory samples collected for this session.');
  }

  const used = buildTimeSeries(events, 'RamSample', null, 'used_bytes').map(([t, v]) => [t, v / 1024 / 1024 / 1024]);
  const avail = buildTimeSeries(events, 'RamSample', null, 'available_bytes').map(([t, v]) => [t, v / 1024 / 1024 / 1024]);
  const faults = buildTimeSeries(events, 'RamSample', null, 'page_faults_per_s');

  const usedPeak = used.reduce((m, [, v]) => Math.max(m, v), 0);
  const availMin = avail.reduce((m, [, v]) => Math.min(m, v), Infinity);
  const faultsPeak = faults.reduce((m, [, v]) => Math.max(m, v), 0);

  const bwEvents = eventsForCategory(events, 'RamBandwidth');
  const hasBw = bwEvents.length > 0;
  const readSeries = hasBw ? buildTimeSeries(events, 'RamBandwidth', null, 'read_bps').map(([t, v]) => [t, v / 1e9]) : [];
  const writeSeries = hasBw ? buildTimeSeries(events, 'RamBandwidth', null, 'write_bps').map(([t, v]) => [t, v / 1e9]) : [];

  const charts = `
    <div class="pr2-charts-row">
      <div class="pr2-mini-chart">
        <div class="mc-title"><span>Used GB</span><span class="v">${usedPeak.toFixed(1)} peak</span></div>
        ${renderAreaChart(used, { color: '#a78bfa', fill: 'rgba(167,139,250,0.25)', height: 60 })}
      </div>
      <div class="pr2-mini-chart">
        <div class="mc-title"><span>Available GB</span><span class="v">${availMin === Infinity ? '—' : availMin.toFixed(1) + ' min'}</span></div>
        ${renderLineChart(avail, { color: '#22c55e', height: 60 })}
      </div>
      <div class="pr2-mini-chart">
        <div class="mc-title"><span>Page faults / s</span><span class="v">${formatInt(faultsPeak)} peak</span></div>
        ${renderLineChart(faults, { color: '#f59e0b', height: 60 })}
      </div>
    </div>
  `;

  const bwBlock = hasBw ? `
    <section class="pr2-card">
      <h2 class="pr2-card-title">RAM bandwidth (uncore counters)</h2>
      <div class="pr2-mini-chart" style="background:var(--bg-input);">
        <div class="mc-title"><span>Read · Write GB/s</span><span class="v">read peak ${readSeries.reduce((m, [, v]) => Math.max(m, v), 0).toFixed(1)} GB/s · write peak ${writeSeries.reduce((m, [, v]) => Math.max(m, v), 0).toFixed(1)} GB/s</span></div>
        <svg viewBox="0 0 920 80" preserveAspectRatio="none" role="img" aria-label="RAM bandwidth">
          ${ramBandwidthSvg(readSeries, writeSeries)}
        </svg>
      </div>
    </section>
  ` : `
    <section class="pr2-card">
      <div class="pr2-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div><strong>RAM bandwidth not collected.</strong> Requires uncore counters (sudo + cap_perfmon) on x86, or Apple Silicon performance counters.</div>
      </div>
    </section>
  `;

  return `
    <section class="pr2-card">
      <h2 class="pr2-card-title">Memory pressure</h2>
      ${charts}
    </section>
    ${bwBlock}
    <section class="pr2-card">
      <h2 class="pr2-card-title">Top processes by RSS</h2>
      <div class="pr2-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div><strong>Per-process RSS not available.</strong> Requires eBPF or psutil sampling — pending feature M5.</div>
      </div>
    </section>
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
  const { events } = ctx;
  const all = eventsForCategory(events, 'DiskIoBurst').map((e) => ({ t: e.t_start_ns, p: unwrapPayload(e.payload) })).filter((s) => s.p);
  if (all.length === 0) {
    return noDataCard('No disk IO samples collected.');
  }
  const byDevice = new Map();
  for (const s of all) {
    const arr = byDevice.get(s.p.device) || [];
    arr.push(s);
    byDevice.set(s.p.device, arr);
  }
  const cards = Array.from(byDevice.entries()).map(([device, samples]) => {
    const model = samples[0]?.p?.model || '';
    const readPts = samples.map((s) => [s.t, s.p.read_bps / 1e6]);
    const writePts = samples.map((s) => [s.t, s.p.write_bps / 1e6]);
    const iopsPts = samples.map((s) => [s.t, (s.p.iops_r || 0) + (s.p.iops_w || 0)]);
    const latPts = samples.map((s) => [s.t, s.p.await_ms_p99 || 0]);
    const peakRead = readPts.reduce((m, [, v]) => Math.max(m, v), 0);
    const peakWrite = writePts.reduce((m, [, v]) => Math.max(m, v), 0);
    const peakIops = iopsPts.reduce((m, [, v]) => Math.max(m, v), 0);
    const p99 = latPts.reduce((m, [, v]) => Math.max(m, v), 0);

    return `
      <section class="pr2-card">
        <h2 class="pr2-card-title">${escape(device)}${model ? ` — <span class="muted">${escape(model)}</span>` : ''}</h2>
        <div class="pr2-charts-row">
          <div class="pr2-mini-chart">
            <div class="mc-title"><span>Throughput MB/s</span><span class="v">R ${peakRead.toFixed(0)} · W ${peakWrite.toFixed(0)}</span></div>
            <svg viewBox="0 0 200 60" preserveAspectRatio="none" role="img" aria-label="Disk throughput">
              ${twoLineSvg(readPts, writePts, '#22c55e', '#ef4444')}
            </svg>
          </div>
          <div class="pr2-mini-chart">
            <div class="mc-title"><span>IOPS (R+W)</span><span class="v">peak ${formatInt(peakIops)}</span></div>
            ${renderLineChart(iopsPts, { color: '#a78bfa', height: 60 })}
          </div>
          <div class="pr2-mini-chart">
            <div class="mc-title"><span>Latency p99 ms</span><span class="v">${p99.toFixed(1)} ms</span></div>
            ${renderLineChart(latPts, { color: '#ef4444', height: 60 })}
          </div>
        </div>
      </section>
    `;
  }).join('');

  return `
    ${cards}
    <section class="pr2-card">
      <h2 class="pr2-card-title">Top processes by IO</h2>
      <div class="pr2-banner-degraded">
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>
        <div><strong>Per-process IO not available.</strong> Requires eBPF (cap_bpf) — pending feature M5.</div>
      </div>
    </section>
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
    return noDataCard('No power samples collected. RAPL likely unavailable; re-run with sudo.');
  }

  // Group by domain → time series.
  const byDomain = new Map();
  for (const e of samples) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    const key = domainKey(p.domain);
    const arr = byDomain.get(key) || [];
    arr.push([e.t_start_ns, p.watts]);
    byDomain.set(key, arr);
  }
  // Align all series on the same x-axis (use the longest one as reference).
  const reference = Array.from(byDomain.values()).reduce((longest, s) => (s.length > longest.length ? s : longest), []);
  reference.sort((a, b) => a[0] - b[0]);
  const seriesList = [];
  const colors = [];
  const labels = [];
  const palette = { CpuPkg: '#a78bfa', CpuCore: '#7c3aed', Dram: '#60a5fa', Ane: '#22c55e', Soc: '#fbbf24', Other: '#71717a' };
  const gpuColors = ['#76b900', '#ed1c24', '#0071c5', '#d4d4d8'];

  // Order: CPU pkg, DRAM, Gpu0..N, Ane, Soc, Other.
  const sortedKeys = Array.from(byDomain.keys()).sort((a, b) => domainOrder(a) - domainOrder(b));
  for (const key of sortedKeys) {
    const points = byDomain.get(key).slice().sort((a, b) => a[0] - b[0]);
    seriesList.push(points);
    if (key.startsWith('Gpu(')) {
      const idx = parseInt(key.match(/\d+/)?.[0] || '0', 10);
      colors.push(gpuColors[idx % gpuColors.length]);
      labels.push(`GPU ${idx}`);
    } else {
      colors.push(palette[key] || '#a78bfa');
      labels.push(key);
    }
  }

  const power = computeKpiPower(events, report.duration_ns);
  const kWh = power.totalKj / 3600;
  const cost = kWh * 0.15;

  const legend = labels.map((l, i) => `<span class="lg"><span class="sw" style="background:${colors[i]};"></span>${escape(l)}</span>`).join('') +
    `<span class="lg"><span class="sw" style="background:#f59e0b;height:2px;"></span>Total (peak ${formatPower(power.peakW)})</span>`;

  // Per-domain mini charts.
  const miniCharts = labels.map((l, i) => {
    const pts = seriesList[i];
    const peak = pts.reduce((m, [, v]) => Math.max(m, v), 0);
    return `
      <div class="pr2-mini-chart">
        <div class="mc-title"><span>${escape(l)} W</span><span class="v">${peak.toFixed(0)} max</span></div>
        ${renderLineChart(pts, { color: colors[i], height: 60 })}
      </div>
    `;
  }).join('');

  return `
    <section class="pr2-card">
      <h2 class="pr2-card-title">Total power broken down per domain</h2>
      <div class="pr2-stacked-chart-wrap">
        ${renderStackedArea(seriesList, { width: 920, height: 220, colors })}
        <div class="pr2-stacked-legend">${legend}</div>
      </div>
    </section>

    <section class="pr2-card">
      <h2 class="pr2-card-title">Energy budget</h2>
      <div class="pr2-kpi-grid">
        <div class="pr2-kpi-tile"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Total energy</span></div><div class="pr2-kpi-value">${power.totalKj.toFixed(1)} kJ</div><div class="pr2-kpi-sub">over ${formatDurationNs(report.duration_ns)}</div></div>
        <div class="pr2-kpi-tile"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Energy</span></div><div class="pr2-kpi-value">${(kWh * 1000).toFixed(1)} Wh</div><div class="pr2-kpi-sub">= ${kWh.toFixed(3)} kWh</div></div>
        <div class="pr2-kpi-tile"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Estimated cost</span></div><div class="pr2-kpi-value">$${cost.toFixed(4)}</div><div class="pr2-kpi-sub">@ $0.15 / kWh</div></div>
        <div class="pr2-kpi-tile"><div class="pr2-kpi-head"><span class="pr2-kpi-label">Avg / Peak</span></div><div class="pr2-kpi-value">${formatPower(power.avgW)}</div><div class="pr2-kpi-sub">peak <strong>${formatPower(power.peakW)}</strong></div></div>
      </div>
    </section>

    <section class="pr2-card">
      <h2 class="pr2-card-title">Per-domain breakdown</h2>
      <div class="pr2-charts-row pr2-charts-row-flexible">${miniCharts}</div>
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
  const rows = collectors.map((c) => {
    const s = normalizeCollectorStatus(c.status);
    const statusCls = s.kind === 'used' ? 'ok' : s.kind === 'skipped' ? 'warn' : s.kind === 'failed' ? 'bad' : 'lim';
    const statusLabel = s.kind.toUpperCase();
    const dim = s.kind === 'skipped' || s.kind === 'failed' ? ' class="dim"' : '';
    return `<tr${dim}>
      <td class="mono">${escape(c.id)}</td>
      <td>${escape(c.primary_category || '—')}</td>
      <td><span class="pr2-src-status ${statusCls}">${escape(statusLabel)}</span></td>
      <td>${escape(s.reason || '—')}</td>
      <td class="num mono">${c.samples_collected ? formatInt(c.samples_collected) : '—'}</td>
      <td class="num mono">${c.raw_size_bytes ? formatBytes(c.raw_size_bytes) : '—'}</td>
    </tr>`;
  }).join('');

  const drift = report.drift_report || {};
  const driftMs = (drift.max_observed_drift_ns || 0) / 1e6;
  const tolMs = (drift.tolerance_ns || 5_000_000) / 1e6;
  const driftOk = !drift.exceeded_tolerance;
  const skippedCount = collectors.filter((c) => normalizeCollectorStatus(c.status).kind === 'skipped').length;

  const hasElevationCollectors = collectors.some((c) => normalizeCollectorStatus(c.status).kind === 'used' && /sudo|admin|elev/i.test(JSON.stringify(c)));

  const rerunBtn = skippedCount > 0
    ? `<tf-button variant="ghost" size="sm" data-action="rerun" aria-label="Re-run with elevation">Re-run with elevated permissions</tf-button>`
    : '';

  return `
    <section class="pr2-card">
      <h2 class="pr2-card-title">Collectors</h2>
      <table class="pr2-table">
        <thead><tr><th>ID</th><th>Category</th><th>Status</th><th>Reason</th><th class="num">Samples</th><th class="num">Raw size</th></tr></thead>
        <tbody>${rows}</tbody>
      </table>
    </section>

    <div class="pr2-two-col">
      <section class="pr2-card">
        <h2 class="pr2-card-title">Privilege summary</h2>
        <div class="pr2-sp-list">
          <div class="sp-item"><span class="sym">Sudo provided</span><span class="pct ${hasElevationCollectors ? 'ok' : 'warn'}">${hasElevationCollectors ? 'yes' : 'no / not tested'}</span></div>
          <div class="sp-item"><span class="sym">Admin (Windows)</span><span class="pct muted">n/a</span></div>
          <div class="sp-item"><span class="sym">cap_perfmon</span><span class="pct warn">${skippedCount > 0 ? 'not set' : 'as needed'}</span></div>
          <div class="sp-item"><span class="sym">cap_bpf</span><span class="pct warn">${skippedCount > 0 ? 'not set' : 'as needed'}</span></div>
        </div>
        ${rerunBtn}
      </section>

      <section class="pr2-card">
        <h2 class="pr2-card-title">Drift report</h2>
        <div class="pr2-sp-list">
          <div class="sp-item"><span class="sym">Max clock drift</span><span class="pct">${driftMs.toFixed(2)} ms</span></div>
          <div class="sp-item"><span class="sym">Tolerance</span><span class="pct">${tolMs.toFixed(1)} ms</span></div>
          <div class="sp-item"><span class="sym">Reference</span><span class="pct">CLOCK_MONOTONIC_RAW</span></div>
        </div>
        <div class="pr2-alert ${driftOk ? 'ok' : 'bad'}">
          <strong>${driftOk ? 'Within tolerance' : 'Exceeded tolerance'}</strong> — cross-source correlation ${driftOk ? 'reliable' : 'unreliable; re-collect with NTP-synced clocks'}.
        </div>
      </section>
    </div>
  `;
}

// =============================================================================
// Misc helpers.
// =============================================================================

function noDataCard(msg) {
  return `<section class="pr2-card"><div class="pr2-banner-degraded"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg><div>${escape(msg)}</div></div></section>`;
}

// ---- Inline SVG icons (small, monochrome) -----------------------------------

function iconCpu()    { return `<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="6" width="12" height="12" rx="1"/><path d="M9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3"/></svg>`; }
function iconRam()    { return `<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="6" width="18" height="12" rx="1"/></svg>`; }
function iconDisk()   { return `<svg viewBox="0 0 24 24" aria-hidden="true"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6"/></svg>`; }
function iconPower()  { return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2L4 7v10l8 5 8-5V7l-8-5z"/></svg>`; }
function iconNet()    { return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="3"/><circle cx="4" cy="4" r="1.5"/><circle cx="20" cy="4" r="1.5"/><circle cx="4" cy="20" r="1.5"/><circle cx="20" cy="20" r="1.5"/></svg>`; }
function iconClock()  { return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/></svg>`; }
function iconFinding(kind) {
  if (kind === 'info') return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/></svg>`;
  if (kind === 'bad')  return `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="M12 9v4M12 17h.01"/></svg>`;
  if (kind === 'ok')   return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 13l4 4L19 7"/></svg>`;
  return `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 9v4M12 17h.01"/><path d="M10.3 3.86l-8.06 14a2 2 0 0 0 1.7 3h16.12a2 2 0 0 0 1.7-3l-8.06-14a2 2 0 0 0-3.4 0z"/></svg>`;
}
