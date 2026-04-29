// =============================================================================
// File: modules/profile-compare.js
// Purpose: Compare Sessions view (mockup section 13). Single-page diff:
//          two summary cards (Baseline vs Compared) z deltami KPI oraz
//          differential flamegraph (czerwony=wolniej w B, zielony=szybciej w B,
//          żółty/neutralny=bez zmian) plus tabela meta z opisem źródeł.
// =============================================================================

import {
  expandCompactSeries,
  computeAllKpis,
  formatBytes,
  formatPower,
  formatDateTime,
  formatDurationNs,
  escape,
} from '/js/modules/profile-report-helpers.js';
import { profilingReport } from '/js/protocol/profiling.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';

function ti(key, vars, fallback) {
  const v = I18n.t(key, vars || null);
  return v === key && fallback != null ? fallback : v;
}

const FIXTURE_PATH = '/js/modules/__fixtures__/profile-report.json';

// Próg powyżej którego frame uznajemy za realnie szybszy/wolniejszy w B.
// Liczone jako (sampleShareB - sampleShareA) / max(A,B). Dwa progi:
// - DELTA_FRAME — barwienie diff-flamegraph
// - DELTA_KPI   — barwienie pigułek delty w KPI grid
const DELTA_FRAME = 0.10; // 10%
const DELTA_KPI = 0.05;   // 5%

function fixtureMode() {
  return typeof window !== 'undefined' && window.__TF_PROFILING_FIXTURE === true;
}

// Równoległe pobranie dwóch raportów. Compare = client-side diff,
// backend nie ma dedykowanego endpointu — używamy istniejącego profilingReport.
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

// Rozpakowuje envelope z profilingReport (raw może być { V2: ... } albo
// surowy V2). Po cleanup #11 nie wspieramy V1 ani envelope.kind.
function unpackReport(raw) {
  if (!raw || typeof raw !== 'object') return null;
  if (raw.report && typeof raw.report === 'object') return raw.report;
  if ('V2' in raw && raw.V2) return raw.V2;
  if (raw.schema_version === 2 || raw.schemaVersion === 2) return raw;
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
      container.innerHTML = renderError(new Error(ti('profiling.report.err_compare_two_required', null, 'Two session IDs are required for comparison')));
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
      container.innerHTML = renderError(new Error(ti('profiling.report.err_compare_v2_required', null, 'Compare requires both reports in V2 schema.')));
      bindBack(container);
      return;
    }

    const ctx = { a: buildOne(repA), b: buildOne(repB) };
    container.innerHTML = renderShell(ctx);
    bindShell(container);
  }
}

export default ProfileCompareView;

function buildOne(report) {
  const expanded = expandCompactSeries(report);
  const kpis = computeAllKpis(expanded.events || [], expanded.names || [], expanded.duration_ns);
  return { report: expanded, kpis };
}

// =============================================================================
// Shell rendering.
// =============================================================================

function renderShell(ctx) {
  return `
    <div class="profile-compare compare-screen">
      <nav class="pr-breadcrumb" aria-label="Breadcrumb">
        <a href="#" data-action="back-mesh">${escape(ti('profiling.compare.breadcrumb_mesh', null, 'Mesh'))}</a>
        <span class="sep">/</span>
        <span>${escape(ti('profiling.compare.breadcrumb_profiling', null, 'Profiling'))}</span>
        <span class="sep">/</span>
        <span>${escape(ti('profiling.compare.breadcrumb_compare', null, 'Compare'))}</span>
      </nav>

      ${renderHeader(ctx)}
      ${renderSummaryGrid(ctx)}
      ${renderDifferentialFlamegraph(ctx)}
      ${renderMetaTable(ctx)}
    </div>
  `;
}

function renderSkeleton() {
  return `
    <div class="profile-compare pc-loading">
      <div class="pr-skeleton" style="width:50%; height:24px;"></div>
      <div class="pr-skeleton" style="width:100%; height:120px; margin-top:14px;"></div>
      <div class="pr-skeleton" style="width:100%; height:300px; margin-top:14px;"></div>
    </div>
  `;
}

function renderError(err) {
  const msg = err?.message || String(err || ti('profiling.compare.fail_unknown', null, 'Unknown error'));
  return `
    <div class="profile-compare pc-error">
      <div class="pc-error-card">
        <h2>${escape(ti('profiling.compare.fail_title', null, 'Compare failed'))}</h2>
        <pre class="mono">${escape(msg)}</pre>
        <tf-button variant="ghost" size="sm" data-action="back-mesh">${escape(ti('profiling.compare.back', null, 'Back'))}</tf-button>
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

function bindShell(container) {
  container.addEventListener('click', (e) => {
    const t = e.target.closest('[data-action]');
    if (!t) return;
    if (t.dataset.action === 'back-mesh') {
      e.preventDefault();
      navigateBack();
    }
  });
}

// =============================================================================
// Header — tytuł + akcja Back.
// =============================================================================

function renderHeader(ctx) {
  const a = ctx.a.report;
  const b = ctx.b.report;
  const labelA = escape(sessionLabel(a));
  const labelB = escape(sessionLabel(b));
  return `
    <header class="compare-header">
      <div class="compare-title-block">
        <h1 class="compare-title">${escape(ti('profiling.compare.title', null, 'Compare sessions'))}</h1>
        <div class="compare-sub">
          <span class="status-pill local">${escape(ti('profiling.compare.baseline', null, 'Baseline'))}</span>
          <strong>${labelA}</strong>
          <span class="compare-vs">${escape(ti('profiling.compare.vs', null, 'vs'))}</span>
          <span class="status-pill sso">${escape(ti('profiling.compare.compared', null, 'Compared'))}</span>
          <strong>${labelB}</strong>
        </div>
      </div>
      <div class="compare-actions">
        <tf-button variant="ghost" size="sm" data-action="back-mesh">${escape(ti('profiling.compare.back', null, 'Back'))}</tf-button>
      </div>
    </header>
  `;
}

function sessionLabel(report) {
  if (report.scope?.label) return report.scope.label;
  if (report.session_id) return report.session_id.slice(0, 8);
  return ti('profiling.compare.session_default', null, 'session');
}

// =============================================================================
// Summary grid — dwie karty side-by-side z KPI i deltami (mockup s13).
// =============================================================================

function renderSummaryGrid(ctx) {
  return `
    <section class="tf-section-card compare-section">
      <h3 class="compare-section-h3">${escape(ti('profiling.compare.side_by_side', null, 'Side-by-side'))}</h3>
      <div class="compare-grid">
        ${renderSummaryCard(ctx.a, ctx.b, 'baseline')}
        ${renderSummaryCard(ctx.b, ctx.a, 'compared')}
      </div>
    </section>
  `;
}

// side: 'baseline' | 'compared'. Dla baseline nie pokazujemy delt; dla compared
// liczymy deltę względem baseline.
function renderSummaryCard(self, other, side) {
  const isCompared = side === 'compared';
  const pillClass = isCompared ? 'sso' : 'local';
  const pillText = isCompared ? ti('profiling.compare.compared', null, 'Compared') : ti('profiling.compare.baseline', null, 'Baseline');
  const sourcesCount = countSources(self.report);
  const dur = formatDurationShort(self.report.duration_ns || 0);
  const label = escape(sessionLabel(self.report));
  const date = escape(formatDateTime(self.report.t0_wallclock_unix_ns));

  const tiles = buildSummaryTiles(self.kpis, isCompared ? other.kpis : null, self.report, isCompared ? other.report : null);
  const tilesHtml = tiles.map((t) => `
    <div class="kpi-tile">
      <div class="kpi-label">${escape(t.label)}${t.deltaHtml ? ' ' + t.deltaHtml : ''}</div>
      <div class="kpi-value">${escape(t.value)}</div>
    </div>
  `).join('');

  return `
    <div class="compare-card">
      <div class="compare-card-head">
        <span class="status-pill ${pillClass}">${escape(pillText)}</span>
        <strong class="compare-card-title">${label}</strong>
        <span class="compare-card-meta">${dur} · ${escape(ti('profiling.compare.sources_count', { n: sourcesCount }, `${sourcesCount} sources`))} · ${date}</span>
      </div>
      <div class="kpi-grid kpi-grid-2">
        ${tilesHtml}
      </div>
    </div>
  `;
}

function buildSummaryTiles(self, other, repSelf, repOther) {
  const cpuVal = self.cpu.avgUtil;
  const gpu0 = self.gpu.get(0);
  const pwrVal = self.power.avgW;
  const totalNs = repSelf.duration_ns || 0;

  const tiles = [];
  tiles.push({
    label: ti('profiling.compare.kpi_cpu_avg', null, 'CPU avg'),
    value: Number.isFinite(cpuVal) ? `${cpuVal.toFixed(0)}%` : '—',
    deltaHtml: other ? deltaPctPill(other.cpu.avgUtil, cpuVal, { suffix: 'pp', lowerIsBetter: true }) : '',
  });
  if (gpu0) {
    const otherGpu0 = other ? other.gpu.get(0) : null;
    tiles.push({
      label: ti('profiling.compare.kpi_gpu0_sm_peak', null, 'GPU0 SM peak'),
      value: Number.isFinite(gpu0.peakCompute) ? `${gpu0.peakCompute.toFixed(0)}%` : '—',
      deltaHtml: otherGpu0 ? deltaPctPill(otherGpu0.peakCompute, gpu0.peakCompute, { suffix: 'pp', lowerIsBetter: false }) : '',
    });
  } else {
    tiles.push({ label: ti('profiling.compare.kpi_gpu0_sm_peak', null, 'GPU0 SM peak'), value: '—', deltaHtml: '' });
  }
  tiles.push({
    label: ti('profiling.compare.kpi_power_avg', null, 'Power avg'),
    value: Number.isFinite(pwrVal) ? formatPower(pwrVal) : '—',
    deltaHtml: other ? deltaAbsPill(other.power.avgW, pwrVal, ' W', 0, true) : '',
  });
  const otherNs = repOther ? (repOther.duration_ns || 0) : 0;
  tiles.push({
    label: ti('profiling.compare.kpi_total_time', null, 'Total time'),
    value: formatDurationShort(totalNs),
    deltaHtml: other ? deltaTimePill(otherNs, totalNs) : '',
  });
  return tiles;
}

// Pigułka delty w stylu mockupu (.delta-down zielone = lepiej dla B,
// .delta-up czerwone = gorzej dla B, .delta-flat = bez zmian).
// otherVal = baseline (A), selfVal = aktualna (B).
function deltaPctPill(baseline, current, { suffix = '%', lowerIsBetter = true } = {}) {
  if (!Number.isFinite(baseline) || !Number.isFinite(current)) return '';
  const diff = current - baseline;
  if (Math.abs(diff) < 0.05) {
    return `<span class="compare-delta same delta-flat">~0${suffix}</span>`;
  }
  const sign = diff > 0 ? '+' : '';
  const isBetter = lowerIsBetter ? diff < 0 : diff > 0;
  const cls = isBetter ? 'delta-down compare-delta down' : 'delta-up compare-delta up';
  return `<span class="${cls}">${sign}${diff.toFixed(0)}${suffix}</span>`;
}

function deltaAbsPill(baseline, current, unit, decimals, lowerIsBetter) {
  if (!Number.isFinite(baseline) || !Number.isFinite(current)) return '';
  const diff = current - baseline;
  if (Math.abs(diff) < 0.5) {
    return `<span class="compare-delta same delta-flat">~0${unit}</span>`;
  }
  const sign = diff > 0 ? '+' : '';
  const isBetter = lowerIsBetter ? diff < 0 : diff > 0;
  const cls = isBetter ? 'delta-down compare-delta down' : 'delta-up compare-delta up';
  return `<span class="${cls}">${sign}${diff.toFixed(decimals)}${unit}</span>`;
}

function deltaTimePill(baselineNs, currentNs) {
  if (!Number.isFinite(baselineNs) || !Number.isFinite(currentNs)) return '';
  const diff = currentNs - baselineNs;
  const absSec = Math.abs(diff) / 1e9;
  if (absSec < 0.05) {
    return `<span class="compare-delta same delta-flat">~0s</span>`;
  }
  const sign = diff > 0 ? '+' : '-';
  const cls = diff < 0 ? 'delta-down compare-delta down' : 'delta-up compare-delta up';
  return `<span class="${cls}">${sign}${absSec.toFixed(1)}s</span>`;
}

function formatDurationShort(ns) {
  if (!Number.isFinite(ns) || ns <= 0) return '0:00';
  const sec = ns / 1e9;
  const m = Math.floor(sec / 60);
  const s = sec - m * 60;
  return `${m}:${s.toFixed(1).padStart(4, '0')}`;
}

function countSources(report) {
  // Heurystyka: liczba unikalnych kategorii zdarzeń + GPU count.
  const cats = new Set();
  for (const ev of report.events || []) {
    if (ev.category) cats.add(String(ev.category));
  }
  return cats.size || 0;
}

// =============================================================================
// Differential flamegraph — własny SVG renderer.
// =============================================================================

function renderDifferentialFlamegraph(ctx) {
  const layout = computeFlameDiffLayout(ctx.a.report, ctx.b.report);
  if (!layout || layout.rects.length === 0) {
    return `
      <section class="tf-section-card compare-section">
        <h3 class="compare-section-h3">${escape(ti('profiling.compare.diff_h', null, 'Differential flamegraph'))}</h3>
        <div class="pc-empty diff-flamegraph-empty">${escape(ti('profiling.compare.diff_no_data', null, 'No CPU sample data available in either session.'))}</div>
      </section>
    `;
  }

  const W = 920;
  const ROW_H = 22;
  const H = layout.depth * ROW_H + ROW_H + 4;
  const rects = layout.rects.map((r) => {
    const cls = colorClass(r.deltaShare);
    const labelText = formatRectLabel(r);
    const showText = r.w >= 60;
    return `
      <g>
        <rect class="diff-rect ${cls}" x="${r.x.toFixed(1)}" y="${r.y.toFixed(1)}" width="${r.w.toFixed(1)}" height="${ROW_H - 1}"/>
        ${showText ? `<text x="${(r.x + 6).toFixed(1)}" y="${(r.y + 15).toFixed(1)}">${escape(labelText)}</text>` : ''}
      </g>
    `;
  }).join('');

  const pct = (DELTA_FRAME * 100).toFixed(0);
  return `
    <section class="tf-section-card compare-section">
      <h3 class="compare-section-h3">${escape(ti('profiling.compare.diff_h', null, 'Differential flamegraph'))}</h3>
      <div class="flame-wrap diff-flamegraph">
        <svg class="flame-svg" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">
          <g font-family="JetBrains Mono, ui-monospace, monospace" font-size="9">
            ${rects}
          </g>
        </svg>
      </div>
      <div class="diff-legend">
        <span><span class="sw slower-red"></span>${escape(ti('profiling.compare.diff_legend_slower', { pct }, `slower in compared (≥${pct}%)`))}</span>
        <span><span class="sw faster-green"></span>${escape(ti('profiling.compare.diff_legend_faster', { pct }, `faster in compared (≥${pct}%)`))}</span>
        <span><span class="sw unchanged"></span>${escape(ti('profiling.compare.diff_legend_unchanged', null, '~unchanged'))}</span>
      </div>
    </section>
  `;
}

// Klasa CSS dla diff-rect. deltaShare ∈ [-1, +1].
function colorClass(deltaShare) {
  if (!Number.isFinite(deltaShare)) return 'unchanged';
  if (deltaShare >= DELTA_FRAME) return 'slower-red';
  if (deltaShare <= -DELTA_FRAME) return 'faster-green';
  return 'unchanged';
}

function formatRectLabel(r) {
  if (Math.abs(r.deltaShare) < 0.005) return r.name;
  const sign = r.deltaShare > 0 ? '+' : '';
  return `${r.name} ${sign}${(r.deltaShare * 100).toFixed(1)}%`;
}

// Buduje layout differential flamegraph poprzez agregację CPU sampli z A i B
// per (depth, frame_name). Wynik to lista prostokątów z pozycją, szerokością
// proporcjonalną do max(shareA, shareB) i wartością deltaShare = (shareB-shareA)
// / max(shareA, shareB).
function computeFlameDiffLayout(repA, repB) {
  const aggA = aggregateSamples(repA);
  const aggB = aggregateSamples(repB);
  if (aggA.totalSamples === 0 && aggB.totalSamples === 0) return null;

  // Zbiór wszystkich (depth, name) z obu stron.
  const keys = new Set([...aggA.byKey.keys(), ...aggB.byKey.keys()]);
  const merged = [];
  for (const key of keys) {
    const [depthStr, name] = key.split('', 2);
    const depth = Number(depthStr);
    const a = aggA.byKey.get(key) || { samples: 0 };
    const b = aggB.byKey.get(key) || { samples: 0 };
    const shareA = aggA.totalSamples > 0 ? a.samples / aggA.totalSamples : 0;
    const shareB = aggB.totalSamples > 0 ? b.samples / aggB.totalSamples : 0;
    const maxShare = Math.max(shareA, shareB);
    if (maxShare < 0.005) continue; // odrzuć bardzo małe ramki
    const denom = Math.max(shareA, shareB);
    const deltaShare = denom > 0 ? (shareB - shareA) / denom : 0;
    merged.push({ depth, name, shareA, shareB, maxShare, deltaShare });
  }

  // Grupuj po głębokości; w każdej warstwie sortuj malejąco po maxShare
  // i layoutuj kolejno od x=0; szerokość proporcjonalna do maxShare względem
  // sumy maxShare w warstwie.
  const W = 920;
  const ROW_H = 22;
  const layers = new Map();
  for (const f of merged) {
    if (!layers.has(f.depth)) layers.set(f.depth, []);
    layers.get(f.depth).push(f);
  }

  let maxDepth = 0;
  for (const d of layers.keys()) if (d > maxDepth) maxDepth = d;
  const finalDepth = maxDepth + 1;

  const rects = [];
  for (const [depth, frames] of layers) {
    frames.sort((x, y) => y.maxShare - x.maxShare);
    const sumShare = frames.reduce((s, f) => s + f.maxShare, 0);
    if (sumShare <= 0) continue;
    // depth=0 (root) ląduje na dole; głębsze ramki nad nim.
    const y = (finalDepth - 1 - depth) * ROW_H;
    let cx = 0;
    for (const f of frames) {
      const w = (f.maxShare / sumShare) * W;
      rects.push({ x: cx, y, w, name: f.name, deltaShare: f.deltaShare });
      cx += w;
    }
  }

  return { rects, depth: finalDepth };
}

// Agreguje CPU sample według (depth, frame_name) korzystając z tabeli stacks
// + frames + names. Każdy sample rozkłada swoje "samples" (lub pct, jeśli brak
// liczby sampli) na każdą ramkę swojego stacka.
function aggregateSamples(report) {
  const events = report.events || [];
  const stacks = report.stacks || [];
  const frames = report.frames || [];
  const names = report.names || [];
  const byKey = new Map();
  let totalSamples = 0;

  for (const ev of events) {
    const cat = ev.category;
    if (cat !== 'CpuSample' && cat !== 0) continue;
    const payload = ev.payload && (ev.payload.CpuSample || ev.payload);
    if (!payload) continue;
    const stackId = Number(payload.stack_id);
    if (!Number.isFinite(stackId)) continue;
    const samples = Number(payload.samples) > 0 ? Number(payload.samples) : (Number(payload.pct) || 1);
    totalSamples += samples;
    const stack = stacks[stackId];
    if (!stack || !Array.isArray(stack)) continue;
    // stack = leaf-first. depth=0 to root (ostatni element).
    for (let i = 0; i < stack.length; i++) {
      const frameId = stack[i];
      const fr = frames[frameId];
      if (!fr) continue;
      const nameId = typeof fr === 'object' ? fr.name_id : fr;
      const name = (typeof nameId === 'number' ? names[nameId] : null) || '—';
      const depth = stack.length - 1 - i; // root ma depth 0
      const key = `${depth}${name}`;
      const cur = byKey.get(key) || { samples: 0 };
      cur.samples += samples;
      byKey.set(key, cur);
    }
  }

  // Fallback: bez stacks/frames używamy płaskiej agregacji po name_id na depth=0.
  if (byKey.size === 0) {
    for (const ev of events) {
      const cat = ev.category;
      if (cat !== 'CpuSample' && cat !== 0) continue;
      const payload = ev.payload && (ev.payload.CpuSample || ev.payload);
      if (!payload) continue;
      const nm = (typeof payload.name_id === 'number' ? names[payload.name_id] : null) || '—';
      const samples = Number(payload.pct) || 1;
      totalSamples += samples;
      const key = `0${nm}`;
      const cur = byKey.get(key) || { samples: 0 };
      cur.samples += samples;
      byKey.set(key, cur);
    }
  }
  return { byKey, totalSamples };
}

// =============================================================================
// Meta-table — informacje o obu sesjach (sources, durations, timestamps).
// =============================================================================

function renderMetaTable(ctx) {
  const a = ctx.a.report;
  const b = ctx.b.report;
  const rows = [
    { label: ti('profiling.compare.meta_session_id', null, 'Session ID'), a: (a.session_id || '').slice(0, 16) || '—', b: (b.session_id || '').slice(0, 16) || '—', mono: true },
    { label: ti('profiling.compare.meta_started', null, 'Started at'), a: formatDateTime(a.t0_wallclock_unix_ns), b: formatDateTime(b.t0_wallclock_unix_ns) },
    { label: ti('profiling.compare.meta_duration', null, 'Duration'), a: formatDurationNs(a.duration_ns), b: formatDurationNs(b.duration_ns) },
    { label: ti('profiling.compare.meta_source_kinds', null, 'Source kinds'), a: String(countSources(a)), b: String(countSources(b)) },
    { label: ti('profiling.compare.meta_cpu_samples', null, 'CPU samples'), a: countSamples(a).toLocaleString('en-US'), b: countSamples(b).toLocaleString('en-US') },
    { label: ti('profiling.compare.meta_gpu_devices', null, 'GPU devices'), a: String(ctx.a.kpis.gpu.size), b: String(ctx.b.kpis.gpu.size) },
    { label: ti('profiling.compare.meta_ram_peak', null, 'RAM peak'), a: formatBytes(ctx.a.kpis.ram.peakUsedBytes), b: formatBytes(ctx.b.kpis.ram.peakUsedBytes) },
    { label: ti('profiling.compare.meta_power_avg', null, 'Power avg'), a: formatPower(ctx.a.kpis.power.avgW), b: formatPower(ctx.b.kpis.power.avgW) },
  ];

  const body = rows.map((r) => `
    <tr>
      <td class="meta-label">${escape(r.label)}</td>
      <td class="${r.mono ? 'mono' : ''}">${escape(r.a)}</td>
      <td class="${r.mono ? 'mono' : ''}">${escape(r.b)}</td>
    </tr>
  `).join('');

  return `
    <section class="tf-section-card compare-section">
      <h3 class="compare-section-h3">${escape(ti('profiling.compare.meta_h', null, 'Sessions metadata'))}</h3>
      <table class="compare-meta-table">
        <thead>
          <tr>
            <th></th>
            <th><span class="status-pill local">${escape(ti('profiling.compare.baseline', null, 'Baseline'))}</span> A</th>
            <th><span class="status-pill sso">${escape(ti('profiling.compare.compared', null, 'Compared'))}</span> B</th>
          </tr>
        </thead>
        <tbody>${body}</tbody>
      </table>
    </section>
  `;
}

function countSamples(report) {
  let n = 0;
  for (const ev of report.events || []) {
    if (ev.category === 'CpuSample' || ev.category === 0) n++;
  }
  return n;
}
