// =============================================================================
// File: profile-range-detail.js — Range Select cross-source fullscreen modal
//                                 (mockup section 14).
// Opens a tf-window showing aggregate stats for a time range selected in the
// Unified Timeline (CPU symbols, GPU kernels per vendor, IO, power summary +
// per-range power line, NVTX ranges intersecting the selection).
// =============================================================================

import { unwrapPayload, detectVendor, escape } from './profile-report-helpers.js';

const NS_PER_S = 1_000_000_000;

// ---- formatting helpers (kept local; mirror the small inline ones in
//      profile-timeline.js which are not exported) -----------------------------

function fmtTime(ns) {
  const totalSec = ns / NS_PER_S;
  if (totalSec >= 60) {
    const m = Math.floor(totalSec / 60);
    const s = totalSec - m * 60;
    return `${m}:${s.toFixed(1).padStart(4, '0')}`;
  }
  if (totalSec >= 1) return `${totalSec.toFixed(2)} s`;
  if (totalSec >= 1e-3) return `${(totalSec * 1e3).toFixed(1)} ms`;
  return `${(totalSec * 1e6).toFixed(0)} µs`;
}

function fmtBytes(b) {
  if (!b) return '0 B';
  const u = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = b;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${u[i]}`;
}

function vendorOf(label) {
  const s = String(label || '').toLowerCase();
  if (s.includes('nvidia') || s.includes('rtx') || s.includes('gtx') || s.includes('cuda')) return 'nv';
  if (s.includes('amd') || s.includes('radeon') || s.includes('rocm') || s.includes('rx ')) return 'amd';
  if (s.includes('intel') || s.includes('arc ')) return 'intel';
  if (s.includes('apple') || s.includes('m1') || s.includes('m2') || s.includes('m3')) return 'apple';
  return 'nv';
}

function vendorShort(v) {
  return v === 'nv' ? 'NV' : v === 'amd' ? 'A' : v === 'intel' ? 'I' : v === 'apple' ? 'AP' : '?';
}

// ---- range aggregation -----------------------------------------------------

// Returns aggregated stats for events overlapping [s, en] in nanoseconds.
function aggregateRange(report, s, en) {
  const events = report.events || [];
  const names = report.names || {};
  const stacks = report.stacks || [];
  const frames = report.frames || [];

  const cpuByStack = new Map();        // stack_id -> sample count
  const gpuByDevice = new Map();        // dev -> Map<name, totalNs>
  const powerSamples = [];              // [{ tNs, watts, domain }]
  const powerByDomain = new Map();      // 'cpu' / 'gpu:0' / ... -> { sum, n }
  let diskRead = 0;
  let diskWrite = 0;
  let netIn = 0;
  let netOut = 0;
  let iopsSum = 0;
  let iopsN = 0;
  const nvtxRanges = [];                // { name, vendor, startNs, durNs }

  for (const ev of events) {
    if (ev.t_end_ns < s || ev.t_start_ns > en) continue;
    const overlap = Math.max(0, Math.min(ev.t_end_ns, en) - Math.max(ev.t_start_ns, s));
    const p = unwrapPayload(ev.payload) || ev.payload || {};
    switch (ev.category) {
      case 'CpuSample': {
        const key = p.stack_id ?? 0;
        cpuByStack.set(key, (cpuByStack.get(key) || 0) + 1);
        break;
      }
      case 'GpuKernel': {
        const dev = p.device_id ?? 0;
        if (!gpuByDevice.has(dev)) gpuByDevice.set(dev, new Map());
        const inner = gpuByDevice.get(dev);
        const nm = names[p.name_id] || `kernel_${p.name_id}`;
        inner.set(nm, (inner.get(nm) || 0) + (overlap || 1));
        break;
      }
      case 'PowerSample': {
        const watts = p.watts || 0;
        powerSamples.push({ tNs: ev.t_start_ns, watts });
        // Domain may be { Cpu: null } or { Gpu: <id> } from rkyv.
        let domainKey = 'other';
        if (p.domain && typeof p.domain === 'object') {
          if ('Cpu' in p.domain) domainKey = 'cpu';
          else if ('Gpu' in p.domain) domainKey = `gpu:${p.domain.Gpu}`;
        }
        const slot = powerByDomain.get(domainKey) || { sum: 0, n: 0 };
        slot.sum += watts;
        slot.n++;
        powerByDomain.set(domainKey, slot);
        break;
      }
      case 'DiskIoBurst': {
        const sec = overlap / NS_PER_S;
        diskRead += (p.read_bps || 0) * sec;
        diskWrite += (p.write_bps || 0) * sec;
        if (p.iops) { iopsSum += p.iops; iopsN++; }
        break;
      }
      case 'NetworkSample': {
        const sec = overlap / NS_PER_S;
        netIn += (p.rx_bps || 0) * sec;
        netOut += (p.tx_bps || 0) * sec;
        break;
      }
      case 'NvtxRange': {
        // Determine vendor from collector / device hint when possible.
        const nm = names[p.name_id] || 'nvtx';
        const collId = ev.collector_id ?? null;
        const vendor = detectVendor(nm, collId) || 'nv';
        nvtxRanges.push({
          name: nm,
          vendor,
          startNs: ev.t_start_ns,
          durNs: ev.t_end_ns - ev.t_start_ns,
        });
        break;
      }
      default:
        break;
    }
  }

  // Resolve top CPU symbols from stacks.
  const totalCpu = Array.from(cpuByStack.values()).reduce((a, b) => a + b, 0) || 1;
  const cpuRows = Array.from(cpuByStack.entries())
    .sort((a, b) => b[1] - a[1])
    .slice(0, 8)
    .map(([stackId, cnt]) => {
      const stack = stacks[stackId] || [];
      const top = stack.length ? frames[stack[0]] : null;
      const sym = (top && top.name) || `stack_${stackId}`;
      return { sym, pct: (cnt / totalCpu) * 100 };
    });

  // Group GPU per vendor for the 3-column kernel grid.
  const collectors = report.collectors || [];
  const vendorGroups = new Map(); // vendor -> { label, kernels: [{name, pct}], deviceIds: [] }
  for (const [dev, inner] of gpuByDevice) {
    const collector = collectors.find((c) => c.device_id === dev);
    const label = (collector && collector.label) || names[`gpu_${dev}`] || `GPU ${dev}`;
    const vendor = vendorOf(label);
    const total = Array.from(inner.values()).reduce((a, b) => a + b, 0) || 1;
    const kernels = Array.from(inner.entries())
      .sort((a, b) => b[1] - a[1])
      .slice(0, 6)
      .map(([nm, dur]) => ({ name: nm, pct: (dur / total) * 100 }));
    if (!vendorGroups.has(vendor)) {
      vendorGroups.set(vendor, { label, kernels, deviceIds: [dev] });
    } else {
      const g = vendorGroups.get(vendor);
      g.deviceIds.push(dev);
      // Merge kernels (keep top of either).
      const merged = new Map();
      [...g.kernels, ...kernels].forEach((k) => {
        merged.set(k.name, Math.max(merged.get(k.name) || 0, k.pct));
      });
      g.kernels = Array.from(merged.entries())
        .sort((a, b) => b[1] - a[1])
        .slice(0, 6)
        .map(([name, pct]) => ({ name, pct }));
    }
  }

  // Power KPIs: avg/peak total = sum across domains per timestamp tick.
  const byTick = new Map();
  for (const s2 of powerSamples) {
    const arr = byTick.get(s2.tNs) || [];
    arr.push(s2.watts);
    byTick.set(s2.tNs, arr);
  }
  let avgTotalW = 0;
  let peakTotalW = 0;
  let nTicks = 0;
  const powerLine = []; // [{tNs, totalW}] sorted by time, used for the mini chart.
  for (const [tNs, arr] of Array.from(byTick.entries()).sort((a, b) => a[0] - b[0])) {
    const total = arr.reduce((a, b) => a + b, 0);
    avgTotalW += total;
    if (total > peakTotalW) peakTotalW = total;
    nTicks++;
    powerLine.push({ tNs, totalW: total });
  }
  avgTotalW = nTicks ? avgTotalW / nTicks : 0;
  const durSec = (en - s) / NS_PER_S;
  const totalKj = (avgTotalW * durSec) / 1000;

  // Per-domain averages for the bottom of the power summary card.
  const perDomain = [];
  for (const [key, slot] of powerByDomain) {
    perDomain.push({ key, avgW: slot.n ? slot.sum / slot.n : 0 });
  }
  perDomain.sort((a, b) => b.avgW - a.avgW);

  // Filter NVTX ranges to those intersecting the window already (done above).
  nvtxRanges.sort((a, b) => a.startNs - b.startNs);

  return {
    cpuRows,
    vendorGroups,
    diskRead,
    diskWrite,
    netIn,
    netOut,
    avgIops: iopsN ? iopsSum / iopsN : 0,
    avgTotalW,
    peakTotalW,
    totalKj,
    perDomain,
    powerLine,
    nvtxRanges,
  };
}

// ---- rendering -------------------------------------------------------------

function renderMiniTimeline(report, s, en) {
  const totalNs = report.duration_ns || 1;
  const W = 920;
  const H = 80;
  // Build a coarse total-power line across the whole session for context.
  const totalSamples = (report.events || [])
    .filter((e) => e.category === 'PowerSample')
    .map((e) => ({ t: e.t_start_ns, w: (unwrapPayload(e.payload) || {}).watts || 0 }))
    .sort((a, b) => a.t - b.t);
  let path = '';
  if (totalSamples.length > 1) {
    const wMax = Math.max(...totalSamples.map((p) => p.w), 1);
    const step = Math.max(1, Math.floor(totalSamples.length / 80));
    const points = [];
    for (let i = 0; i < totalSamples.length; i += step) {
      const p = totalSamples[i];
      const x = (p.t / totalNs) * W;
      const y = H - 20 - (p.w / wMax) * (H - 30);
      points.push(`${x.toFixed(1)} ${y.toFixed(1)}`);
    }
    path = `M ${points.join(' L ')}`;
  }
  const x0 = (s / totalNs) * W;
  const x1 = (en / totalNs) * W;
  const labelX = (x0 + x1) / 2;
  const dur = en - s;
  return `
    <div class="range-mini-timeline">
      <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">
        <line class="range-baseline" x1="0" y1="${H - 20}" x2="${W}" y2="${H - 20}"/>
        ${path ? `<path class="range-power-line" d="${path}"/>` : ''}
        <rect class="range-overlay" x="${x0.toFixed(1)}" y="0"
              width="${Math.max(2, x1 - x0).toFixed(1)}" height="${H - 15}"/>
        <text class="range-label" x="${labelX.toFixed(1)}" y="${H - 4}" text-anchor="middle">
          selection ${escape(fmtTime(dur))}
        </text>
      </svg>
    </div>`;
}

function renderCpuSection(stats) {
  if (!stats.cpuRows.length) {
    return `
      <div class="range-section">
        <h3>Top CPU symbols</h3>
        <div class="range-empty">No CPU samples in this range.</div>
      </div>`;
  }
  const rows = stats.cpuRows.map((r) => `
    <div class="range-symbol-row">
      <span class="sym">${escape(r.sym)}</span>
      <span class="pct">${r.pct.toFixed(1)}%</span>
    </div>`).join('');
  return `
    <div class="range-section">
      <h3>Top CPU symbols</h3>
      <div class="range-list">${rows}</div>
    </div>`;
}

function renderIoSection(stats) {
  const items = [
    ['Disk read', fmtBytes(stats.diskRead)],
    ['Disk write', fmtBytes(stats.diskWrite)],
    ['Avg IOPS', stats.avgIops ? Math.round(stats.avgIops).toLocaleString() : '—'],
    ['Net in', fmtBytes(stats.netIn)],
    ['Net out', fmtBytes(stats.netOut)],
  ];
  const rows = items.map(([k, v]) => `
    <div class="range-io-row">
      <span class="sym">${escape(k)}</span>
      <span class="pct">${escape(String(v))}</span>
    </div>`).join('');
  return `
    <div class="range-section">
      <h3>IO summary</h3>
      <div class="range-list">${rows}</div>
    </div>`;
}

function renderPowerLine(powerLine, s, en) {
  if (powerLine.length < 2) return '';
  const W = 220;
  const H = 50;
  const span = Math.max(1, en - s);
  const wMax = Math.max(...powerLine.map((p) => p.totalW), 1);
  const points = powerLine.map((p) => {
    const x = ((p.tNs - s) / span) * W;
    const y = H - (p.totalW / wMax) * (H - 4) - 2;
    return `${x.toFixed(1)} ${y.toFixed(1)}`;
  });
  return `
    <div class="range-power-mini">
      <div class="range-power-mini-title">Power timeline (range)</div>
      <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">
        <path d="M ${points.join(' L ')}"/>
      </svg>
    </div>`;
}

function renderPowerSection(stats, s, en) {
  const rows = [
    ['Avg total', `${stats.avgTotalW.toFixed(0)} W`],
    ['Peak total', `${stats.peakTotalW.toFixed(0)} W`],
    ['Total energy', `${stats.totalKj.toFixed(2)} kJ`],
    ...stats.perDomain.slice(0, 4).map((d) => {
      let label = d.key;
      if (d.key === 'cpu') label = 'CPU pkg';
      else if (d.key.startsWith('gpu:')) label = `GPU ${d.key.slice(4)}`;
      return [label, `${d.avgW.toFixed(0)} W`];
    }),
  ];
  const items = rows.map(([k, v]) => `
    <div class="range-symbol-row">
      <span class="sym">${escape(k)}</span>
      <span class="pct">${escape(v)}</span>
    </div>`).join('');
  return `
    <div class="range-section range-power-summary">
      <h3>Power summary</h3>
      <div class="range-list">${items}</div>
      ${renderPowerLine(stats.powerLine, s, en)}
    </div>`;
}

function renderVendorColumn(vendor, group) {
  if (!group) {
    // Empty placeholder column with degraded banner — vendor present in
    // collector list but no kernel data available.
    return `
      <div>
        <div class="range-vendor-name ${vendor}">
          <span class="range-vendor-badge ${vendor}">${vendorShort(vendor)}</span>
          ${vendor.toUpperCase()}
        </div>
        <div class="range-banner-degraded">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/>
          </svg>
          <div>No kernel data in this range.</div>
        </div>
      </div>`;
  }
  const rows = group.kernels.map((k) => `
    <div class="range-kernel-row">
      <span class="sym">${escape(k.name)}</span>
      <span class="pct">${k.pct.toFixed(0)}%</span>
    </div>`).join('') || '<div class="range-empty">no kernels</div>';
  return `
    <div>
      <div class="range-vendor-name ${vendor}">
        <span class="range-vendor-badge ${vendor}">${vendorShort(vendor)}</span>
        ${escape(group.label)}
      </div>
      <div class="range-list">${rows}</div>
    </div>`;
}

function renderKernelsAndNvtx(stats, report) {
  // Always render NV / AMD / Intel columns when we have any GPU collector
  // for that vendor; otherwise show only the present ones (min 1).
  const collectors = report.collectors || [];
  const vendorsPresent = new Set();
  for (const c of collectors) {
    if (c.kind === 'gpu' || (c.label && /gpu/i.test(c.label))) {
      vendorsPresent.add(vendorOf(c.label || ''));
    }
  }
  // Always include vendors that produced kernels in this range.
  for (const v of stats.vendorGroups.keys()) vendorsPresent.add(v);
  if (vendorsPresent.size === 0) vendorsPresent.add('nv');
  const ordered = ['nv', 'amd', 'intel', 'apple'].filter((v) => vendorsPresent.has(v));
  const cols = ordered.map((v) => renderVendorColumn(v, stats.vendorGroups.get(v))).join('');

  const nvtxRows = stats.nvtxRanges.length
    ? stats.nvtxRanges.slice(0, 50).map((r) => `
        <tr>
          <td class="mono">${escape(r.name)}</td>
          <td><span class="range-vendor-badge ${r.vendor}">${vendorShort(r.vendor)}</span></td>
          <td class="num">${escape(fmtTime(r.startNs))}</td>
          <td class="num">${escape(fmtTime(r.durNs))}</td>
        </tr>`).join('')
    : `<tr><td colspan="4" class="range-empty">No NVTX ranges intersect this selection.</td></tr>`;

  return `
    <div class="range-section">
      <h3>Top GPU kernels per vendor</h3>
      <div class="range-vendor-grid">${cols}</div>
      <h3 style="margin-top:14px;">NVTX ranges in selection</h3>
      <table class="range-nvtx-table">
        <thead>
          <tr>
            <th>Range</th><th>Vendor</th>
            <th class="num">Start</th><th class="num">Duration</th>
          </tr>
        </thead>
        <tbody>${nvtxRows}</tbody>
      </table>
    </div>`;
}

// ---- public class ----------------------------------------------------------

export class ProfileRangeDetailView {
  // Render the full panel into `host`. `range` carries { tStartNs, tEndNs }.
  // `report` is the same expanded report object the timeline already uses.
  render(host, { report, range }) {
    if (!host) return;
    const s = range.tStartNs;
    const en = range.tEndNs;
    if (s == null || en == null || en <= s) {
      host.innerHTML = `<div class="range-empty">Invalid time range.</div>`;
      return;
    }
    const stats = aggregateRange(report, s, en);

    host.classList.add('profile-range');
    host.innerHTML = `
      <div class="range-header">
        <span class="status-pill warn">Range</span>
        <span class="range-time">${escape(fmtTime(s))} → ${escape(fmtTime(en))}</span>
        <span class="range-duration">${escape(fmtTime(en - s))}</span>
        <div class="range-actions"></div>
      </div>
      ${renderMiniTimeline(report, s, en)}
      <div class="range-grid">
        <div class="range-left">
          ${renderCpuSection(stats)}
          ${renderIoSection(stats)}
          ${renderPowerSection(stats, s, en)}
        </div>
        ${renderKernelsAndNvtx(stats, report)}
      </div>
    `;
  }
}

// ---- modal opener ----------------------------------------------------------

// Opens the range-detail view inside a tf-window modal. Returns the Promise
// from TfWindow.open so callers can await close.
export function openRangeDetailModal({ report, range }) {
  const host = document.createElement('div');
  const view = new ProfileRangeDetailView();
  view.render(host, { report, range });
  // Defer import: tf-window.js registers the custom element on first import
  // anywhere in the app; we assume it is loaded by this point (used widely).
  const TfWindow = customElements.get('tf-window');
  if (!TfWindow || typeof TfWindow.open !== 'function') {
    // Fallback: append host to body in a simple overlay so the feature still
    // works if the component bundle has not registered yet.
    const overlay = document.createElement('div');
    overlay.className = 'tf-window-backdrop';
    overlay.style.cssText = 'position:fixed; inset:0; background:rgba(0,0,0,0.6); z-index:9999; overflow:auto; padding:24px;';
    overlay.appendChild(host);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) overlay.remove();
    });
    document.body.appendChild(overlay);
    return Promise.resolve({ action: 'fallback' });
  }
  return TfWindow.open({
    title: 'Range Select — cross-source detail',
    subtitle: `${fmtTime(range.tStartNs)} → ${fmtTime(range.tEndNs)} · ${fmtTime(range.tEndNs - range.tStartNs)}`,
    icon: 'i-grid-rows',
    body: host,
    buttons: 'close',
    draggable: true,
    resizable: true,
    modal: true,
    width: Math.min(1200, window.innerWidth - 60),
    height: Math.min(820, window.innerHeight - 60),
  });
}

export default ProfileRangeDetailView;
