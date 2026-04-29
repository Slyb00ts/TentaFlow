// =============================================================================
// File: modules/profile-compare.js
// Purpose: Compare Sessions view (mockup section 13). Side-by-side widok dwoch
//          raportow profilingu z deltami KPI i differential flamegraph
//          (dwa flamegraphy obok siebie + tablica delt funkcji).
// =============================================================================

import {
  expandCompactSeries,
  computeAllKpis,
  formatBytes,
  formatPower,
  formatPct,
  formatDateTime,
  formatDurationNs,
  escape,
} from '/js/modules/profile-report-helpers.js';
import { profilingReport } from '/js/protocol/profiling.js';
import { CpuFlamegraph } from '/js/modules/profile-flamegraph.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-chip.js';

const FIXTURE_PATH = '/js/modules/__fixtures__/profile-report.json';

function fixtureMode() {
  return typeof window !== 'undefined' && window.__TF_PROFILING_FIXTURE === true;
}

// Rownolegle pobranie dwoch raportow. Compare = czysty client-side diff,
// backend nie ma osobnego endpointu — uzywamy istniejacego profilingReport
// dwa razy.
async function loadTwoReports({ nodeId, sessionA, sessionB }) {
  const fetchOne = async (sid) => {
    if (fixtureMode()) {
      const resp = await fetch(FIXTURE_PATH, { headers: { accept: 'application/json' } });
      if (!resp.ok) throw new Error(`fixture HTTP ${resp.status}`);
      return await resp.json();
    }
    return profilingReport({ nodeId: nodeId || '', sessionId: sid });
  };
  const [a, b] = await Promise.all([fetchOne(sessionA), fetchOne(sessionB)]);
  return { a, b };
}

// Rozpakowuje envelope z profilingReport (raw może być { V2: ... } /
// { envelope: { kind:'v2', report } } / bare V2). Zwraca null gdy V1 lub
// nieobsługiwane.
function unpackReport(raw) {
  if (!raw || typeof raw !== 'object') return null;
  if (raw.envelope && raw.envelope.kind === 'v2' && raw.envelope.report) {
    return raw.envelope.report;
  }
  if ('V2' in raw && raw.V2) return raw.V2;
  if (raw.schema_version === 2) return raw;
  return null;
}

function navigateBack() {
  if (window.Router && typeof window.Router.navigate === 'function') {
    window.Router.navigate('mesh');
    return;
  }
  history.back();
}

// =============================================================================
// Public API.
// =============================================================================

export class ProfileCompareView {
  static async render(container, { nodeId, sessionA, sessionB } = {}) {
    if (!container) throw new Error('container is required');
    if (!sessionA || !sessionB) {
      container.innerHTML = renderError(new Error('Two session IDs are required for comparison'));
      bindBack(container);
      return;
    }
    container.innerHTML = renderSkeleton();

    let raw;
    try {
      raw = await loadTwoReports({ nodeId, sessionA, sessionB });
    } catch (err) {
      container.innerHTML = renderError(err);
      bindBack(container);
      return;
    }

    const repA = unpackReport(raw.a);
    const repB = unpackReport(raw.b);
    if (!repA || !repB) {
      container.innerHTML = renderError(new Error('Compare requires both reports in V2 schema (legacy V1 not supported).'));
      bindBack(container);
      return;
    }

    const ctxA = buildOne(repA);
    const ctxB = buildOne(repB);
    const ctx = {
      a: ctxA,
      b: ctxB,
      defaultTab: 'overview',
      // Lazy cache rendered HTML per tab (re-mount flamegraph instances on demand).
      _tabHtml: new Map(),
    };

    container.innerHTML = renderShell(ctx);
    bindShell(container, ctx);
    renderTab(container, ctx, ctx.defaultTab);
  }
}

export default ProfileCompareView;

function buildOne(report) {
  const expanded = expandCompactSeries(report);
  const kpis = computeAllKpis(expanded.events || [], expanded.names || [], expanded.duration_ns);
  return { report: expanded, kpis };
}

// =============================================================================
// Shell.
// =============================================================================

function renderShell(ctx) {
  const a = ctx.a.report;
  const b = ctx.b.report;
  const labelA = escape(a.scope?.label || a.session_id?.slice(0, 8) || 'A');
  const labelB = escape(b.scope?.label || b.session_id?.slice(0, 8) || 'B');
  const sidA = escape((a.session_id || '').slice(0, 8));
  const sidB = escape((b.session_id || '').slice(0, 8));
  const dateA = escape(formatDateTime(a.t0_wallclock_unix_ns));
  const dateB = escape(formatDateTime(b.t0_wallclock_unix_ns));

  return `
    <div class="profile-compare">
      <nav class="pr2-breadcrumb" aria-label="Breadcrumb">
        <a href="#" data-action="back-mesh">Mesh</a>
        <span class="sep">/</span>
        <span>Profiling</span>
        <span class="sep">/</span>
        <span>Compare</span>
      </nav>

      <header class="pc-header">
        <div class="pc-title-block">
          <h1 class="pc-title">Compare sessions</h1>
          <div class="pc-sub">
            <span class="pc-chip pc-chip-a">A · ${labelA}</span>
            <span class="pc-vs">vs</span>
            <span class="pc-chip pc-chip-b">B · ${labelB}</span>
          </div>
          <div class="pc-meta">
            <span class="mono">${sidA}</span> @ <span>${dateA}</span>
            <span class="sep">·</span>
            <span class="mono">${sidB}</span> @ <span>${dateB}</span>
          </div>
        </div>
        <div class="pc-actions">
          <tf-button variant="ghost" size="sm" data-action="back-mesh">Back</tf-button>
        </div>
      </header>

      <div class="pr2-tabs-wrap">
        <tf-tabs variant="underline" value="${escape(ctx.defaultTab)}" id="pc-tabs">
          <tf-tab id="overview">Overview</tf-tab>
          <tf-tab id="flame">CPU Flamegraph</tf-tab>
          <tf-tab id="gpu">GPU</tf-tab>
          <tf-tab id="memory">Memory</tf-tab>
          <tf-tab id="power">Power</tf-tab>
        </tf-tabs>
      </div>

      <main class="pc-body" id="pc-body" role="region" aria-label="Compare body"></main>
    </div>
  `;
}

function renderSkeleton() {
  return `
    <div class="profile-compare pc-loading">
      <div class="pr2-skeleton" style="width:50%; height:24px;"></div>
      <div class="pr2-skeleton" style="width:100%; height:120px; margin-top:14px;"></div>
      <div class="pr2-skeleton" style="width:100%; height:300px; margin-top:14px;"></div>
    </div>
  `;
}

function renderError(err) {
  const msg = err?.message || String(err || 'Unknown error');
  return `
    <div class="profile-compare pc-error">
      <div class="pc-error-card">
        <h2>Compare failed</h2>
        <pre class="mono">${escape(msg)}</pre>
        <tf-button variant="ghost" size="sm" data-action="back-mesh">Back</tf-button>
      </div>
    </div>
  `;
}

function bindBack(container) {
  container.addEventListener('click', (e) => {
    const t = e.target.closest('[data-action="back-mesh"]');
    if (!t) return;
    e.preventDefault();
    navigateBack();
  });
}

function bindShell(container, ctx) {
  container.addEventListener('click', (e) => {
    const t = e.target.closest('[data-action]');
    if (!t) return;
    if (t.dataset.action === 'back-mesh') {
      e.preventDefault();
      navigateBack();
    }
  });

  const tabsEl = container.querySelector('#pc-tabs');
  if (tabsEl) {
    tabsEl.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id) renderTab(container, ctx, id);
    });
  }
}

// =============================================================================
// Tab dispatch + per-tab content.
// =============================================================================

function renderTab(container, ctx, tabId) {
  const body = container.querySelector('#pc-body');
  if (!body) return;
  // Flamegraph tab nie cache'ujemy bo wymaga remount instances.
  if (tabId === 'flame') {
    body.innerHTML = '';
    renderFlameTab(body, ctx);
    return;
  }
  if (ctx._tabHtml.has(tabId)) {
    body.innerHTML = ctx._tabHtml.get(tabId);
    return;
  }
  let html = '';
  switch (tabId) {
    case 'overview': html = renderOverviewTab(ctx); break;
    case 'gpu':      html = renderGpuTab(ctx);      break;
    case 'memory':   html = renderMemoryTab(ctx);   break;
    case 'power':    html = renderPowerTab(ctx);    break;
    default:         html = `<div class="pc-empty">Unknown tab: ${escape(tabId)}</div>`;
  }
  ctx._tabHtml.set(tabId, html);
  body.innerHTML = html;
}

// ---- Delta helpers ----------------------------------------------------------

const DELTA_THRESHOLD = 0.05; // ±5% — scoring "better/worse/unchanged"

function pctDelta(a, b) {
  if (!Number.isFinite(a) || !Number.isFinite(b)) return null;
  if (a === 0) return b === 0 ? 0 : Infinity;
  return (b - a) / Math.abs(a);
}

function renderDeltaPill(rawA, rawB, opts = {}) {
  const { unit = '', formatter = (x) => String(x), lowerIsBetter = true } = opts;
  if (!Number.isFinite(rawA) || !Number.isFinite(rawB)) {
    return `<span class="pc-delta neutral">—</span>`;
  }
  const diff = rawB - rawA;
  const pct = pctDelta(rawA, rawB);
  const absPct = Math.abs(pct ?? 0);
  let cls = 'neutral';
  if (absPct >= DELTA_THRESHOLD) {
    const worse = (diff > 0) === lowerIsBetter;
    cls = worse ? 'worse' : 'better';
  }
  const sign = diff > 0 ? '+' : '';
  const valueStr = `${sign}${formatter(diff)}${unit}`;
  return `<span class="pc-delta ${cls}">${escape(valueStr)}</span>`;
}

// ---- Overview ---------------------------------------------------------------

function renderOverviewTab(ctx) {
  const ka = ctx.a.kpis;
  const kb = ctx.b.kpis;
  const a = ctx.a.report;
  const b = ctx.b.report;

  const rows = [
    {
      label: 'CPU avg',
      a: ka.cpu.avgUtil, b: kb.cpu.avgUtil,
      fmt: (x) => x.toFixed(1) + '%',
      delta: renderDeltaPill(ka.cpu.avgUtil, kb.cpu.avgUtil, { unit: 'pp', formatter: (d) => d.toFixed(1) }),
    },
    {
      label: 'CPU peak',
      a: ka.cpu.peakUtil, b: kb.cpu.peakUtil,
      fmt: (x) => x.toFixed(1) + '%',
      delta: renderDeltaPill(ka.cpu.peakUtil, kb.cpu.peakUtil, { unit: 'pp', formatter: (d) => d.toFixed(1) }),
    },
    {
      label: 'RAM peak',
      a: ka.ram.peakUsedBytes, b: kb.ram.peakUsedBytes,
      fmt: (x) => formatBytes(x),
      delta: renderDeltaPill(ka.ram.peakUsedBytes, kb.ram.peakUsedBytes, { formatter: (d) => formatBytes(Math.abs(d)) }),
    },
    {
      label: 'Power avg',
      a: ka.power.avgW, b: kb.power.avgW,
      fmt: (x) => formatPower(x),
      delta: renderDeltaPill(ka.power.avgW, kb.power.avgW, { unit: ' W', formatter: (d) => d.toFixed(1) }),
    },
    {
      label: 'Power peak',
      a: ka.power.peakW, b: kb.power.peakW,
      fmt: (x) => formatPower(x),
      delta: renderDeltaPill(ka.power.peakW, kb.power.peakW, { unit: ' W', formatter: (d) => d.toFixed(1) }),
    },
    {
      label: 'Total time',
      a: a.duration_ns, b: b.duration_ns,
      fmt: (x) => formatDurationNs(x),
      delta: renderDeltaPill(a.duration_ns, b.duration_ns, { formatter: (d) => formatDurationNs(Math.abs(d)) }),
    },
  ];

  // GPU0 peak compute (jeżeli oba mają)
  const gpu0A = ka.gpu.get(0);
  const gpu0B = kb.gpu.get(0);
  if (gpu0A && gpu0B) {
    rows.push({
      label: 'GPU0 SM peak',
      a: gpu0A.peakCompute, b: gpu0B.peakCompute,
      fmt: (x) => x.toFixed(1) + '%',
      delta: renderDeltaPill(gpu0A.peakCompute, gpu0B.peakCompute, { unit: 'pp', formatter: (d) => d.toFixed(1), lowerIsBetter: false }),
    });
  }

  const rowsHtml = rows.map((r) => `
    <div class="pc-kpi-row">
      <div class="pc-kpi-name">${escape(r.label)}</div>
      <div class="pc-kpi-a">${escape(r.fmt(r.a))}</div>
      <div class="pc-kpi-b">${escape(r.fmt(r.b))}</div>
      <div class="pc-kpi-delta">${r.delta}</div>
    </div>
  `).join('');

  return `
    <section class="pc-section">
      <h3 class="pc-h3">Side-by-side KPIs</h3>
      <div class="pc-kpi-table">
        <div class="pc-kpi-head">
          <div></div>
          <div class="pc-col-a">A · baseline</div>
          <div class="pc-col-b">B · compared</div>
          <div class="pc-col-d">Δ (B − A)</div>
        </div>
        ${rowsHtml}
      </div>
      <div class="pc-legend">
        <span><span class="sw better"></span>better in B (≥5%)</span>
        <span><span class="sw worse"></span>worse in B (≥5%)</span>
        <span><span class="sw neutral"></span>~unchanged</span>
      </div>
    </section>
  `;
}

// ---- CPU Flamegraph (side-by-side + top function deltas) -------------------

function renderFlameTab(body, ctx) {
  body.innerHTML = `
    <section class="pc-section">
      <h3 class="pc-h3">CPU Flamegraph — side-by-side</h3>
      <div class="pc-flame-grid">
        <div class="pc-flame-pane">
          <div class="pc-pane-head"><span class="pc-chip pc-chip-a">A · baseline</span></div>
          <div id="pc-flame-a" class="pc-flame-host"></div>
        </div>
        <div class="pc-flame-pane">
          <div class="pc-pane-head"><span class="pc-chip pc-chip-b">B · compared</span></div>
          <div id="pc-flame-b" class="pc-flame-host"></div>
        </div>
      </div>
      <div id="pc-flame-deltas" class="pc-flame-deltas"></div>
    </section>
  `;

  const hostA = body.querySelector('#pc-flame-a');
  const hostB = body.querySelector('#pc-flame-b');

  const dataA = {
    events: ctx.a.report.events || [],
    frames: ctx.a.report.frames || [],
    stacks: ctx.a.report.stacks || [],
    names: ctx.a.report.names || [],
    totalDurationNs: ctx.a.report.duration_ns || 0,
    source: 'A',
  };
  const dataB = {
    events: ctx.b.report.events || [],
    frames: ctx.b.report.frames || [],
    stacks: ctx.b.report.stacks || [],
    names: ctx.b.report.names || [],
    totalDurationNs: ctx.b.report.duration_ns || 0,
    source: 'B',
  };

  // Każda strona = osobna instancja flamegrapha (full mode). Differential mode
  // wbudowany w CpuFlamegraph operuje na zakresach czasu w obrębie jednego
  // raportu, więc nie da się go bezpośrednio zastosować cross-report.
  try {
    new CpuFlamegraph(hostA, dataA);
  } catch (err) {
    hostA.innerHTML = `<div class="pc-empty">Flamegraph A unavailable: ${escape(err?.message || err)}</div>`;
  }
  try {
    new CpuFlamegraph(hostB, dataB);
  } catch (err) {
    hostB.innerHTML = `<div class="pc-empty">Flamegraph B unavailable: ${escape(err?.message || err)}</div>`;
  }

  // Tablica delt funkcji CPU — top symbole z każdej strony, scal po nazwie.
  const deltasHost = body.querySelector('#pc-flame-deltas');
  deltasHost.innerHTML = renderTopSymbolDeltas(ctx);
}

function topCpuSymbols(report) {
  // Agregacja CpuSample.pct po name_id (compatibility: fixture nie ma stacks).
  const names = report.names || [];
  const byName = new Map();
  for (const ev of report.events || []) {
    if (ev.category !== 'CpuSample') continue;
    const p = ev.payload && (ev.payload.CpuSample || ev.payload);
    if (!p) continue;
    const nm = (typeof p.name_id === 'number' && names[p.name_id]) || '—';
    const cur = byName.get(nm) || 0;
    byName.set(nm, cur + (Number(p.pct) || 0));
  }
  return byName;
}

function renderTopSymbolDeltas(ctx) {
  const mapA = topCpuSymbols(ctx.a.report);
  const mapB = topCpuSymbols(ctx.b.report);
  const all = new Set([...mapA.keys(), ...mapB.keys()]);
  const rows = [];
  for (const nm of all) {
    const va = mapA.get(nm) || 0;
    const vb = mapB.get(nm) || 0;
    rows.push({ name: nm, a: va, b: vb, diff: vb - va });
  }
  rows.sort((x, y) => Math.abs(y.diff) - Math.abs(x.diff));
  const top = rows.slice(0, 12);
  if (top.length === 0) {
    return `<div class="pc-empty">No CPU sample data to compare.</div>`;
  }

  const body = top.map((r) => {
    const sign = r.diff > 0 ? '+' : '';
    let cls = 'neutral';
    if (Math.abs(r.diff) >= 0.5) cls = r.diff > 0 ? 'worse' : 'better';
    return `
      <tr>
        <td class="mono">${escape(r.name)}</td>
        <td class="num">${r.a.toFixed(2)}%</td>
        <td class="num">${r.b.toFixed(2)}%</td>
        <td class="num"><span class="pc-delta ${cls}">${sign}${r.diff.toFixed(2)}%</span></td>
      </tr>
    `;
  }).join('');

  return `
    <h3 class="pc-h3">Top function deltas</h3>
    <table class="pc-table">
      <thead><tr><th>Symbol</th><th class="num">A %</th><th class="num">B %</th><th class="num">Δ</th></tr></thead>
      <tbody>${body}</tbody>
    </table>
  `;
}

// ---- GPU --------------------------------------------------------------------

function renderGpuTab(ctx) {
  const ga = ctx.a.kpis.gpu;
  const gb = ctx.b.kpis.gpu;
  const ids = new Set([...ga.keys(), ...gb.keys()]);
  if (ids.size === 0) {
    return `<section class="pc-section"><div class="pc-empty">No GPU data in either session.</div></section>`;
  }
  const blocks = [];
  for (const id of Array.from(ids).sort((x, y) => x - y)) {
    const a = ga.get(id);
    const b = gb.get(id);
    const peakA = a?.peakCompute ?? NaN;
    const peakB = b?.peakCompute ?? NaN;
    const memA = a?.memUsedBytes ?? NaN;
    const memB = b?.memUsedBytes ?? NaN;
    const pwrA = a?.peakW ?? NaN;
    const pwrB = b?.peakW ?? NaN;

    blocks.push(`
      <div class="pc-card">
        <div class="pc-card-head">GPU ${id}</div>
        <div class="pc-kpi-row"><div class="pc-kpi-name">SM peak</div>
          <div class="pc-kpi-a">${Number.isFinite(peakA) ? peakA.toFixed(1) + '%' : '—'}</div>
          <div class="pc-kpi-b">${Number.isFinite(peakB) ? peakB.toFixed(1) + '%' : '—'}</div>
          <div class="pc-kpi-delta">${renderDeltaPill(peakA, peakB, { unit: 'pp', formatter: (d) => d.toFixed(1), lowerIsBetter: false })}</div>
        </div>
        <div class="pc-kpi-row"><div class="pc-kpi-name">Mem used</div>
          <div class="pc-kpi-a">${Number.isFinite(memA) ? formatBytes(memA) : '—'}</div>
          <div class="pc-kpi-b">${Number.isFinite(memB) ? formatBytes(memB) : '—'}</div>
          <div class="pc-kpi-delta">${renderDeltaPill(memA, memB, { formatter: (d) => formatBytes(Math.abs(d)) })}</div>
        </div>
        <div class="pc-kpi-row"><div class="pc-kpi-name">Power peak</div>
          <div class="pc-kpi-a">${Number.isFinite(pwrA) ? formatPower(pwrA) : '—'}</div>
          <div class="pc-kpi-b">${Number.isFinite(pwrB) ? formatPower(pwrB) : '—'}</div>
          <div class="pc-kpi-delta">${renderDeltaPill(pwrA, pwrB, { unit: ' W', formatter: (d) => d.toFixed(1) })}</div>
        </div>
      </div>
    `);
  }
  return `<section class="pc-section"><h3 class="pc-h3">GPU per-device comparison</h3>${blocks.join('')}</section>`;
}

// ---- Memory -----------------------------------------------------------------

function renderMemoryTab(ctx) {
  const ka = ctx.a.kpis.ram;
  const kb = ctx.b.kpis.ram;
  return `
    <section class="pc-section">
      <h3 class="pc-h3">Memory comparison</h3>
      <div class="pc-kpi-table">
        <div class="pc-kpi-head">
          <div></div><div class="pc-col-a">A</div><div class="pc-col-b">B</div><div class="pc-col-d">Δ</div>
        </div>
        <div class="pc-kpi-row">
          <div class="pc-kpi-name">Peak used</div>
          <div class="pc-kpi-a">${escape(formatBytes(ka.peakUsedBytes))}</div>
          <div class="pc-kpi-b">${escape(formatBytes(kb.peakUsedBytes))}</div>
          <div class="pc-kpi-delta">${renderDeltaPill(ka.peakUsedBytes, kb.peakUsedBytes, { formatter: (d) => formatBytes(Math.abs(d)) })}</div>
        </div>
        <div class="pc-kpi-row">
          <div class="pc-kpi-name">Peak BW</div>
          <div class="pc-kpi-a">${escape(formatBytes(ka.peakBwBps))}/s</div>
          <div class="pc-kpi-b">${escape(formatBytes(kb.peakBwBps))}/s</div>
          <div class="pc-kpi-delta">${renderDeltaPill(ka.peakBwBps, kb.peakBwBps, { formatter: (d) => formatBytes(Math.abs(d)) + '/s' })}</div>
        </div>
      </div>
    </section>
  `;
}

// ---- Power ------------------------------------------------------------------

function renderPowerTab(ctx) {
  const pa = ctx.a.kpis.power;
  const pb = ctx.b.kpis.power;
  return `
    <section class="pc-section">
      <h3 class="pc-h3">Power comparison</h3>
      <div class="pc-kpi-table">
        <div class="pc-kpi-head">
          <div></div><div class="pc-col-a">A</div><div class="pc-col-b">B</div><div class="pc-col-d">Δ</div>
        </div>
        <div class="pc-kpi-row">
          <div class="pc-kpi-name">Avg watts</div>
          <div class="pc-kpi-a">${escape(formatPower(pa.avgW))}</div>
          <div class="pc-kpi-b">${escape(formatPower(pb.avgW))}</div>
          <div class="pc-kpi-delta">${renderDeltaPill(pa.avgW, pb.avgW, { unit: ' W', formatter: (d) => d.toFixed(1) })}</div>
        </div>
        <div class="pc-kpi-row">
          <div class="pc-kpi-name">Peak watts</div>
          <div class="pc-kpi-a">${escape(formatPower(pa.peakW))}</div>
          <div class="pc-kpi-b">${escape(formatPower(pb.peakW))}</div>
          <div class="pc-kpi-delta">${renderDeltaPill(pa.peakW, pb.peakW, { unit: ' W', formatter: (d) => d.toFixed(1) })}</div>
        </div>
        <div class="pc-kpi-row">
          <div class="pc-kpi-name">Total energy</div>
          <div class="pc-kpi-a">${(pa.totalKj || 0).toFixed(2)} kJ</div>
          <div class="pc-kpi-b">${(pb.totalKj || 0).toFixed(2)} kJ</div>
          <div class="pc-kpi-delta">${renderDeltaPill(pa.totalKj, pb.totalKj, { unit: ' kJ', formatter: (d) => d.toFixed(2) })}</div>
        </div>
      </div>
    </section>
  `;
}
