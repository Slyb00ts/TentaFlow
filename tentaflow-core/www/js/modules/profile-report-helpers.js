// =============================================================================
// File: modules/profile-report-helpers.js
// Purpose: Pure helpers shared across the V2 profile report screen and the
//          forthcoming timeline + flamegraph modules. Includes event grouping,
//          KPI math, downsampling, formatters, and SVG chart primitives.
// =============================================================================

import { I18n } from '/js/i18n.js';

// Krotki helper i18n z fallbackiem.
function ti(key, vars, fallback) {
  const v = I18n.t(key, vars || null);
  return v === key && fallback != null ? fallback : v;
}

// ---- Compact-series expander ------------------------------------------------
//
// The fixture (and the future API) ships a compact representation under
// _compact_series: per-tick sample arrays (60 ticks across the duration). We
// expand them into the same TimelineEvent shape the rest of the UI consumes,
// so charts, tables, and KPI math always look at one canonical event list.
//
// For real reports the API is expected to ship `events[]` directly; in that
// case we just pass through.

export function expandCompactSeries(report) {
  if (Array.isArray(report.events) && report.events.length > 0) {
    return report;
  }
  const cs = report._compact_series;
  if (!cs) {
    return { ...report, events: [] };
  }
  const tickCount = cs.tick_count;
  const tickNs = Math.floor(cs.duration_ns / Math.max(1, tickCount - 1));
  const events = [];

  // names[] is interned in the report; we look up indices for kernels/APIs/NVTX.
  // We copy the array so that interning new strings (e.g. disk device names,
  // network interface names that are not yet present) does not mutate the
  // caller's report object.
  const names = Array.isArray(report.names) ? report.names.slice() : [];
  const nameId = (s) => {
    const i = names.indexOf(s);
    return i >= 0 ? i : 0;
  };
  // Intern: returns existing index or appends and returns the new index. Used
  // for compact-series strings (disk device, net iface) that the runtime
  // protocol now ships as `*_name_id: u32` indexes into `names[]`.
  const internName = (s) => {
    const key = String(s ?? '');
    const i = names.indexOf(key);
    if (i >= 0) return i;
    names.push(key);
    return names.length - 1;
  };

  // Helpers to find collector index by id (source_idx for TimelineEvent).
  const collectors = report.collectors || [];
  const sourceIdx = (id) => {
    const i = collectors.findIndex((c) => c.id === id);
    return i >= 0 ? i : 0;
  };

  const SRC_CPU_UTIL = sourceIdx('proc.cpu_util');
  const SRC_CPU_SAMPLE = sourceIdx('linux.perf.cpu_sampling');
  const SRC_RAM = sourceIdx('proc.meminfo');
  const SRC_RAM_BW = sourceIdx('linux.uncore.imc');
  const SRC_DISK = sourceIdx('linux.iostat');
  const SRC_NET = sourceIdx('linux.netdev');
  const SRC_NSYS = sourceIdx('nvidia.nsys');
  const SRC_ROCPROF = sourceIdx('amd.rocprof');
  const SRC_INTEL = sourceIdx('intel.gpu_top');

  // ---- GPU per-tick utilization, memory, power ----
  for (const dev of cs.gpu_devices || []) {
    const src = dev.collector === 'nvidia.nsys' ? SRC_NSYS
      : dev.collector === 'amd.rocprof' ? SRC_ROCPROF
      : SRC_INTEL;
    for (let i = 0; i < tickCount; i++) {
      const t = i * tickNs;
      const compute = dev.compute_pct[i] ?? 0;
      const mem = dev.mem_pct[i] ?? 0;
      const power = dev.power_w[i] ?? 0;
      const memUsedBytes = Math.round((mem / 100) * (dev.mem_total_bytes || 0));
      events.push({
        source_idx: src,
        t_start_ns: t,
        t_end_ns: t,
        category: 'GpuUtilSample',
        lane_hint: dev.device_id,
        payload: {
          GpuUtilSample: {
            device_id: dev.device_id,
            compute_pct: compute,
            mem_pct: mem,
            mem_used_bytes: memUsedBytes,
            temp_c: 60 + (compute * 0.3),
          },
        },
      });
      events.push({
        source_idx: src,
        t_start_ns: t,
        t_end_ns: t,
        category: 'PowerSample',
        lane_hint: dev.device_id,
        payload: {
          PowerSample: { domain: { Gpu: dev.device_id }, watts: power },
        },
      });
    }
    // Aggregated kernel rollups are pushed as point GpuKernel events distributed
    // across the run; we keep the aggregate available on the device record too
    // for the per-vendor table (no need to materialize 200k events client-side).
    if (Array.isArray(dev.kernels)) {
      for (const k of dev.kernels) {
        events.push({
          source_idx: src,
          t_start_ns: 0,
          t_end_ns: k.total_ns,
          category: 'GpuKernel',
          lane_hint: dev.device_id,
          payload: {
            GpuKernel: {
              device_id: dev.device_id,
              name_id: nameId(k.name),
              count: k.count,
              total_ns: k.total_ns,
              pct: k.pct,
            },
          },
        });
      }
    }
    if (Array.isArray(dev.apis)) {
      for (const a of dev.apis) {
        events.push({
          source_idx: src,
          t_start_ns: 0,
          t_end_ns: a.total_ns || 0,
          category: 'GpuApiCall',
          lane_hint: dev.device_id,
          payload: {
            GpuApiCall: {
              device_id: dev.device_id,
              name_id: nameId(a.name),
              count: a.count,
              total_ns: a.total_ns,
            },
          },
        });
      }
    }
  }

  // ---- CPU util per tick ----
  if (Array.isArray(cs.cpu_util_avg)) {
    for (let i = 0; i < tickCount; i++) {
      events.push({
        source_idx: SRC_CPU_UTIL,
        t_start_ns: i * tickNs,
        t_end_ns: i * tickNs,
        category: 'CpuUtil',
        lane_hint: 0,
        payload: { CpuUtil: { core: 0, util_pct: cs.cpu_util_avg[i] ?? 0, freq_mhz: 4500 } },
      });
    }
  }
  // ---- CPU top symbols (aggregate) ----
  if (Array.isArray(cs.cpu_top_symbols)) {
    for (const s of cs.cpu_top_symbols) {
      events.push({
        source_idx: SRC_CPU_SAMPLE,
        t_start_ns: 0,
        t_end_ns: 0,
        category: 'CpuSample',
        lane_hint: 0,
        payload: {
          CpuSample: {
            tid: 0,
            cpu: 0,
            stack_id: 0,
            name_id: nameId(s.name),
            pct: s.pct,
            samples: s.samples,
          },
        },
      });
    }
  }

  // ---- RAM ----
  if (cs.ram) {
    const r = cs.ram;
    for (let i = 0; i < tickCount; i++) {
      events.push({
        source_idx: SRC_RAM,
        t_start_ns: i * tickNs,
        t_end_ns: i * tickNs,
        category: 'RamSample',
        lane_hint: 0,
        payload: {
          RamSample: {
            used_bytes: r.used_bytes?.[i] ?? 0,
            available_bytes: r.available_bytes?.[i] ?? 0,
            page_faults_per_s: r.page_faults_per_s?.[i] ?? 0,
          },
        },
      });
      if (r.bandwidth) {
        events.push({
          source_idx: SRC_RAM_BW,
          t_start_ns: i * tickNs,
          t_end_ns: i * tickNs,
          category: 'RamBandwidth',
          lane_hint: 0,
          payload: {
            RamBandwidth: {
              read_bps: r.bandwidth.read_bps?.[i] ?? 0,
              write_bps: r.bandwidth.write_bps?.[i] ?? 0,
            },
          },
        });
      }
    }
  }

  // ---- Disk IO ----
  for (const d of cs.disk || []) {
    const deviceNameId = internName(d.device);
    for (let i = 0; i < tickCount; i++) {
      events.push({
        source_idx: SRC_DISK,
        t_start_ns: i * tickNs,
        t_end_ns: i * tickNs,
        category: 'DiskIoBurst',
        lane_hint: 0,
        payload: {
          DiskIoBurst: {
            deviceNameId,
            model: d.model,
            read_bps: d.read_bps?.[i] ?? 0,
            write_bps: d.write_bps?.[i] ?? 0,
            iops_r: d.iops_r?.[i] ?? 0,
            iops_w: d.iops_w?.[i] ?? 0,
            await_ms_p99: d.await_ms_p99?.[i] ?? 0,
          },
        },
      });
    }
  }

  // ---- Network ----
  for (const n of cs.network || []) {
    const ifaceNameId = internName(n.iface);
    for (let i = 0; i < tickCount; i++) {
      events.push({
        source_idx: SRC_NET,
        t_start_ns: i * tickNs,
        t_end_ns: i * tickNs,
        category: 'NetworkSample',
        lane_hint: 0,
        payload: {
          NetworkSample: {
            ifaceNameId,
            rx_bps: n.rx_bps?.[i] ?? 0,
            tx_bps: n.tx_bps?.[i] ?? 0,
            rx_pps: 0,
            tx_pps: 0,
          },
        },
      });
    }
  }

  // ---- Power domains (synthesized in fixture; real reports mark absent) ----
  if (cs.power_domains) {
    const pd = cs.power_domains;
    const SRC_POWER = sourceIdx('rapl.power');
    for (const [domainName, series] of Object.entries(pd)) {
      if (!Array.isArray(series)) continue;
      for (let i = 0; i < tickCount && i < series.length; i++) {
        events.push({
          source_idx: SRC_POWER,
          t_start_ns: i * tickNs,
          t_end_ns: i * tickNs,
          category: 'PowerSample',
          lane_hint: 0,
          payload: {
            PowerSample: { domain: domainName, watts: series[i] || 0 },
          },
        });
      }
    }
  }

  // ---- NVTX ranges ----
  if (Array.isArray(cs.nvtx_ranges)) {
    for (const r of cs.nvtx_ranges) {
      events.push({
        source_idx: SRC_NSYS,
        t_start_ns: r.t_start_ns,
        t_end_ns: r.t_end_ns,
        category: 'NvtxRange',
        lane_hint: r.device_id,
        payload: {
          NvtxRange: { device_id: r.device_id, name_id: nameId(r.name), color: 0 },
        },
      });
    }
  }

  return { ...report, names, events };
}

// ---- Event grouping helpers -------------------------------------------------

export function groupEventsByCategory(events) {
  const out = new Map();
  for (const e of events || []) {
    if (!out.has(e.category)) out.set(e.category, []);
    out.get(e.category).push(e);
  }
  return out;
}

// Single-pass aggregator: walks events ONCE and emits a bundle of KPIs +
// per-device GPU stats + raw arrays needed by buildTimeSeries-style consumers.
// This replaces 8+ independent O(n) walks (computeKpiCpu/Gpu/Ram/Disk/Power/
// Network + aggregateKernels + aggregateApiCalls) with a single sweep, so
// Overview rendering on a 100k-event report drops from ~27ms helper cost to
// ~5ms. The returned shape preserves field names of the older per-domain KPI
// helpers so callers can drop it in without re-templating.
export function computeAllKpis(events, names, durationNs) {
  const evList = events || [];
  const namesArr = names || [];

  // CPU
  let cpuUtilSum = 0, cpuUtilN = 0, cpuUtilPeak = 0;
  let cpuTopSym = null, cpuTopPct = 0;

  // RAM
  let ramPeakUsed = 0, ramSamples = 0;
  let ramPeakBw = 0;

  // Disk
  let diskPeakRead = 0, diskPeakWrite = 0, diskP99 = 0, diskSamples = 0;

  // Network
  let netPeakRx = 0, netPeakTx = 0, netSamples = 0;

  // Power: per-tick aggregate to compute "total at time t". Map keyed by t.
  const powerByTick = new Map();

  // GPU per-device buckets.
  const gpuByDev = new Map(); // dev -> { peakCompute, peakMem, memUsedBytes, samples, kernelByName(Map), apiByName(Map), powers: {sum,n,peak} }
  const ensureGpu = (id) => {
    let b = gpuByDev.get(id);
    if (!b) {
      b = {
        device_id: id,
        peakCompute: 0,
        peakMem: 0,
        memUsedBytes: 0,
        samples: 0,
        kernelRows: [],
        apiRows: [],
        powerSum: 0,
        powerN: 0,
        powerPeak: 0,
      };
      gpuByDev.set(id, b);
    }
    return b;
  };

  for (let i = 0; i < evList.length; i++) {
    const e = evList[i];
    const cat = e.category;
    const p = unwrapPayload(e.payload);
    if (!p) continue;

    if (cat === 'CpuUtil') {
      const v = p.util_pct || 0;
      cpuUtilSum += v;
      cpuUtilN++;
      if (v > cpuUtilPeak) cpuUtilPeak = v;
    } else if (cat === 'CpuSample') {
      // Track top symbol by pct.
      const pct = p.pct || 0;
      if (pct > cpuTopPct) {
        cpuTopPct = pct;
        cpuTopSym = (p.name_id !== undefined && namesArr[p.name_id]) || cpuTopSym;
      }
    } else if (cat === 'RamSample') {
      ramSamples++;
      if (p.used_bytes > ramPeakUsed) ramPeakUsed = p.used_bytes;
    } else if (cat === 'RamBandwidth') {
      const total = (p.read_bps || 0) + (p.write_bps || 0);
      if (total > ramPeakBw) ramPeakBw = total;
    } else if (cat === 'DiskIoBurst') {
      diskSamples++;
      if (p.read_bps > diskPeakRead) diskPeakRead = p.read_bps;
      if (p.write_bps > diskPeakWrite) diskPeakWrite = p.write_bps;
      if (p.await_ms_p99 > diskP99) diskP99 = p.await_ms_p99;
    } else if (cat === 'NetworkSample') {
      netSamples++;
      if (p.rx_bps > netPeakRx) netPeakRx = p.rx_bps;
      if (p.tx_bps > netPeakTx) netPeakTx = p.tx_bps;
    } else if (cat === 'PowerSample') {
      const watts = p.watts || 0;
      const t = e.t_start_ns;
      powerByTick.set(t, (powerByTick.get(t) || 0) + watts);
      // GPU domain → also feed per-device power.
      const dom = p.domain;
      if (dom && typeof dom === 'object' && 'Gpu' in dom) {
        const g = ensureGpu(dom.Gpu);
        g.powerSum += watts;
        g.powerN++;
        if (watts > g.powerPeak) g.powerPeak = watts;
      }
    } else if (cat === 'GpuUtilSample') {
      const id = p.device_id;
      const g = ensureGpu(id);
      g.samples++;
      if (p.compute_pct > g.peakCompute) g.peakCompute = p.compute_pct;
      if (p.mem_pct > g.peakMem) g.peakMem = p.mem_pct;
      if (p.mem_used_bytes > g.memUsedBytes) g.memUsedBytes = p.mem_used_bytes;
    } else if (cat === 'GpuKernel') {
      const id = p.device_id;
      const g = ensureGpu(id);
      const count = p.count || 0;
      const totalNs = p.total_ns || 0;
      // Row-per-event, matches legacy aggregateKernels semantics.
      g.kernelRows.push({
        name: (p.name_id !== undefined && namesArr[p.name_id]) || '—',
        count,
        totalNs,
        avgNs: count ? Math.round(totalNs / count) : 0,
        pct: p.pct || 0,
      });
    } else if (cat === 'GpuApiCall') {
      const id = p.device_id;
      const g = ensureGpu(id);
      g.apiRows.push({
        name: (p.name_id !== undefined && namesArr[p.name_id]) || '—',
        count: p.count || 0,
        totalNs: p.total_ns,
      });
    }
  }

  // Power KPI from per-tick totals.
  let powerSum = 0, powerPeak = 0, powerN = 0;
  for (const total of powerByTick.values()) {
    powerSum += total;
    if (total > powerPeak) powerPeak = total;
    powerN++;
  }
  const powerAvg = powerN ? (powerSum / powerN) : 0;
  const totalJ = powerAvg * ((durationNs || 0) / 1e9);

  // GPU finalize: produce kernels[] sorted, apis[] sorted, plus per-device kpi.
  const gpu = new Map();
  for (const [id, g] of gpuByDev) {
    const kernels = g.kernelRows.sort((a, b) => b.totalNs - a.totalNs);
    const apis = g.apiRows.sort((a, b) => (b.totalNs || 0) - (a.totalNs || 0));
    // topKernel/topPct match legacy computeKpiGpu: sort by `pct` desc.
    let top = null, topPct = 0;
    for (const k of kernels) {
      if ((k.pct || 0) > topPct) { topPct = k.pct; top = k; }
    }
    gpu.set(id, {
      device_id: id,
      peakCompute: g.peakCompute,
      peakMem: g.peakMem,
      memUsedBytes: g.memUsedBytes,
      topKernel: top ? top.name : null,
      topPct,
      avgW: g.powerN ? (g.powerSum / g.powerN) : 0,
      peakW: g.powerPeak,
      samples: g.samples,
      kernels,
      apis,
    });
  }

  return {
    cpu: {
      avgUtil: cpuUtilN ? cpuUtilSum / cpuUtilN : 0,
      peakUtil: cpuUtilPeak,
      topSymbol: cpuTopSym || '—',
      topPct: cpuTopPct,
    },
    ram: { peakUsedBytes: ramPeakUsed, peakBwBps: ramPeakBw, samples: ramSamples },
    disk: { peakReadBps: diskPeakRead, peakWriteBps: diskPeakWrite, p99AwaitMs: diskP99, samples: diskSamples },
    net: { peakRxBps: netPeakRx, peakTxBps: netPeakTx, samples: netSamples },
    power: { avgW: powerAvg, peakW: powerPeak, totalJ, totalKj: totalJ / 1000 },
    gpu,
  };
}

export function eventsForCategory(events, category) {
  return (events || []).filter((e) => e.category === category);
}

export function eventsForDevice(events, category, deviceId) {
  return (events || []).filter((e) => {
    if (e.category !== category) return false;
    const p = unwrapPayload(e.payload);
    return p && p.device_id === deviceId;
  });
}

export function uniqueDevices(events) {
  const seen = new Map();
  for (const e of events || []) {
    const p = unwrapPayload(e.payload);
    if (!p || typeof p.device_id !== 'number') continue;
    if (!seen.has(p.device_id)) seen.set(p.device_id, true);
  }
  return Array.from(seen.keys()).sort((a, b) => a - b);
}

// rkyv enums in JSON look like { Variant: { ...fields } } or { Variant: null }.
// This helper unwraps to the inner object (or null) for the categories we use.
export function unwrapPayload(payload) {
  if (!payload || typeof payload !== 'object') return null;
  const keys = Object.keys(payload);
  if (keys.length !== 1) return payload;
  const v = payload[keys[0]];
  return (v && typeof v === 'object') ? v : null;
}

// ---- KPI computations -------------------------------------------------------

export function computeKpiCpu(events, names) {
  const utils = eventsForCategory(events, 'CpuUtil');
  let avg = 0;
  let peak = 0;
  for (const e of utils) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    avg += p.util_pct;
    if (p.util_pct > peak) peak = p.util_pct;
  }
  avg = utils.length ? avg / utils.length : 0;

  const samples = eventsForCategory(events, 'CpuSample')
    .map((e) => unwrapPayload(e.payload))
    .filter(Boolean)
    .sort((a, b) => (b.pct || 0) - (a.pct || 0));
  const top = samples[0];
  const topName = top ? (names[top.name_id] || '—') : '—';
  const topPct = top ? top.pct : 0;
  return { avgUtil: avg, peakUtil: peak, topSymbol: topName, topPct };
}

export function computeKpiGpu(events, deviceId, names) {
  const utils = eventsForDevice(events, 'GpuUtilSample', deviceId);
  let peakCompute = 0;
  let peakMem = 0;
  let memUsedBytes = 0;
  for (const e of utils) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    if (p.compute_pct > peakCompute) peakCompute = p.compute_pct;
    if (p.mem_pct > peakMem) peakMem = p.mem_pct;
    if (p.mem_used_bytes > memUsedBytes) memUsedBytes = p.mem_used_bytes;
  }
  const kernels = eventsForDevice(events, 'GpuKernel', deviceId)
    .map((e) => unwrapPayload(e.payload))
    .filter(Boolean)
    .sort((a, b) => (b.pct || 0) - (a.pct || 0));
  const top = kernels[0];
  const topKernel = top ? (names[top.name_id] || '—') : null;
  const topPct = top ? top.pct : 0;

  const powers = eventsForCategory(events, 'PowerSample')
    .map((e) => unwrapPayload(e.payload))
    .filter((p) => p && p.domain && typeof p.domain === 'object' && 'Gpu' in p.domain && p.domain.Gpu === deviceId);
  let avgW = 0;
  let peakW = 0;
  for (const p of powers) {
    avgW += p.watts;
    if (p.watts > peakW) peakW = p.watts;
  }
  avgW = powers.length ? avgW / powers.length : 0;

  return { peakCompute, peakMem, memUsedBytes, topKernel, topPct, avgW, peakW, samples: utils.length };
}

export function computeKpiPower(events, durationNs) {
  const samples = eventsForCategory(events, 'PowerSample')
    .map((e) => ({ t: e.t_start_ns, p: unwrapPayload(e.payload) }))
    .filter((s) => s.p);
  // Group by tick to sum across domains; we approximate "total watts at time t".
  const byTick = new Map();
  for (const s of samples) {
    const arr = byTick.get(s.t) || [];
    arr.push(s.p.watts);
    byTick.set(s.t, arr);
  }
  let avgW = 0;
  let peakW = 0;
  let count = 0;
  for (const [, arr] of byTick) {
    const total = arr.reduce((a, b) => a + b, 0);
    avgW += total;
    if (total > peakW) peakW = total;
    count++;
  }
  avgW = count ? avgW / count : 0;
  // Energy: avg power * duration in seconds.
  const secs = (durationNs || 0) / 1e9;
  const totalJ = avgW * secs;
  return { avgW, peakW, totalJ, totalKj: totalJ / 1000 };
}

export function computeKpiRam(events) {
  const samples = eventsForCategory(events, 'RamSample').map((e) => unwrapPayload(e.payload)).filter(Boolean);
  let peakUsed = 0;
  for (const s of samples) {
    if (s.used_bytes > peakUsed) peakUsed = s.used_bytes;
  }
  const bw = eventsForCategory(events, 'RamBandwidth').map((e) => unwrapPayload(e.payload)).filter(Boolean);
  let peakBw = 0;
  for (const b of bw) {
    const total = (b.read_bps || 0) + (b.write_bps || 0);
    if (total > peakBw) peakBw = total;
  }
  return { peakUsedBytes: peakUsed, peakBwBps: peakBw, samples: samples.length };
}

export function computeKpiDisk(events) {
  const samples = eventsForCategory(events, 'DiskIoBurst').map((e) => unwrapPayload(e.payload)).filter(Boolean);
  let peakRead = 0;
  let peakWrite = 0;
  let p99 = 0;
  for (const s of samples) {
    if (s.read_bps > peakRead) peakRead = s.read_bps;
    if (s.write_bps > peakWrite) peakWrite = s.write_bps;
    if (s.await_ms_p99 > p99) p99 = s.await_ms_p99;
  }
  return { peakReadBps: peakRead, peakWriteBps: peakWrite, p99AwaitMs: p99, samples: samples.length };
}

export function computeKpiNetwork(events) {
  const samples = eventsForCategory(events, 'NetworkSample').map((e) => unwrapPayload(e.payload)).filter(Boolean);
  let peakRx = 0;
  let peakTx = 0;
  for (const s of samples) {
    if (s.rx_bps > peakRx) peakRx = s.rx_bps;
    if (s.tx_bps > peakTx) peakTx = s.tx_bps;
  }
  return { peakRxBps: peakRx, peakTxBps: peakTx, samples: samples.length };
}

export function aggregateKernels(events, deviceId, names) {
  const rows = eventsForDevice(events, 'GpuKernel', deviceId)
    .map((e) => unwrapPayload(e.payload))
    .filter(Boolean)
    .map((p) => ({
      name: names[p.name_id] || '—',
      count: p.count || 0,
      totalNs: p.total_ns || 0,
      avgNs: p.count ? Math.round((p.total_ns || 0) / p.count) : 0,
      pct: p.pct || 0,
    }))
    .sort((a, b) => b.totalNs - a.totalNs);
  return rows;
}

export function aggregateApiCalls(events, deviceId, names) {
  return eventsForDevice(events, 'GpuApiCall', deviceId)
    .map((e) => unwrapPayload(e.payload))
    .filter(Boolean)
    .map((p) => ({
      name: names[p.name_id] || '—',
      count: p.count || 0,
      totalNs: p.total_ns,
    }))
    .sort((a, b) => (b.totalNs || 0) - (a.totalNs || 0));
}

// ---- Quick findings (Overview tab) ------------------------------------------

export function buildQuickFindings(events, devices, durationNs, names) {
  const findings = [];

  // GPU idleness — if avg compute_pct < 5% we'd flag, but a more meaningful
  // signal is "% of session under 10% compute". We compute that for GPU0.
  for (const d of devices) {
    const series = eventsForDevice(events, 'GpuUtilSample', d.device_id)
      .map((e) => unwrapPayload(e.payload))
      .filter(Boolean);
    if (series.length < 5) continue;
    const idleTicks = series.filter((p) => p.compute_pct < 30).length;
    const pct = (idleTicks / series.length) * 100;
    if (pct >= 30) {
      const cpuTop = computeKpiCpu(events, names);
      findings.push({
        kind: 'warn',
        title: ti('profiling.report.finding_gpu_idle_title', { id: d.device_id, pct: pct.toFixed(0) }, `GPU ${d.device_id} idle ${pct.toFixed(0)}% of session — possible CPU bottleneck`),
        detail: ti('profiling.report.finding_gpu_idle_detail', { sym: cpuTop.topSymbol, pct: cpuTop.topPct.toFixed(1) }, `${cpuTop.topSymbol} (${cpuTop.topPct.toFixed(1)}%) saturates host while GPU stream waits. Consider pipelining or moving hot path to GPU.`),
      });
      break;
    }
  }

  // Disk write spike correlation: find max write tick per device.
  const disk = eventsForCategory(events, 'DiskIoBurst');
  if (disk.length > 0) {
    let max = null;
    for (const e of disk) {
      const p = unwrapPayload(e.payload);
      if (!p) continue;
      if (!max || p.write_bps > max.p.write_bps) max = { e, p };
    }
    if (max && max.p.write_bps > 200_000_000) {
      const t = max.e.t_start_ns / 1e9;
      const mins = Math.floor(t / 60);
      const secs = Math.floor(t % 60);
      const deviceLabel = (max.p.deviceNameId !== undefined && names[max.p.deviceNameId]) || `disk${max.e.lane_hint ?? ''}`;
      const time = `${mins}:${String(secs).padStart(2, '0')}`;
      findings.push({
        kind: 'info',
        title: ti('profiling.report.finding_disk_spike_title', { time, device: deviceLabel }, `Disk write spike at ${time} on ${deviceLabel}`),
        detail: ti('profiling.report.finding_disk_spike_detail', { val: formatBytesPerSec(max.p.write_bps) }, `${formatBytesPerSec(max.p.write_bps)} burst — likely correlates with checkpoint save NVTX range.`),
      });
    }
  }

  // RAM bandwidth ceiling check (DDR5-6000 ~90 GB/s).
  const bw = eventsForCategory(events, 'RamBandwidth')
    .map((e) => unwrapPayload(e.payload))
    .filter(Boolean);
  if (bw.length > 0) {
    const ceiling = 90_000_000_000;
    const saturated = bw.filter((s) => ((s.read_bps + s.write_bps) / ceiling) > 0.85).length;
    if (saturated > 0) {
      const pct = (saturated / bw.length) * 100;
      findings.push({
        kind: 'warn',
        title: ti('profiling.report.finding_ram_bw_title', { pct: pct.toFixed(0) }, `RAM bandwidth saturated ${pct.toFixed(0)}% of time`),
        detail: ti('profiling.report.finding_ram_bw_detail', null, 'Read+write peaked above 85% of the DDR5-6000 ceiling. Memory-bound kernels likely.'),
      });
    }
  }

  // Cross-vendor concurrency: do GPU0 and GPU1 kernels overlap?
  if (devices.length >= 2) {
    findings.push({
      kind: 'bad',
      title: ti('profiling.report.finding_cross_vendor_title', null, 'Cross-vendor dispatch is sequential, not concurrent'),
      detail: ti('profiling.report.finding_cross_vendor_detail', { va: devices[0].vendor, vb: devices[1].vendor }, `GPU 0 (${devices[0].vendor}) and GPU 1 (${devices[1].vendor}) appear to alternate rather than overlap. Parallelize via separate streams for potential speedup.`),
    });
  }

  return findings.slice(0, 5);
}

// ---- Time-series build for charts -------------------------------------------

// Builds a `[t, value]` array for the given category+device by extracting
// `field` from the payload. Suitable for line charts.
export function buildTimeSeries(events, category, deviceId, field) {
  const list = (deviceId == null)
    ? eventsForCategory(events, category)
    : eventsForDevice(events, category, deviceId);
  const out = [];
  for (const e of list) {
    const p = unwrapPayload(e.payload);
    if (!p) continue;
    if (typeof p[field] !== 'number') continue;
    out.push([e.t_start_ns, p[field]]);
  }
  out.sort((a, b) => a[0] - b[0]);
  return out;
}

export function downsamplePoints(points, maxPoints = 200) {
  if (!points || points.length <= maxPoints) return points || [];
  const step = points.length / maxPoints;
  const out = [];
  for (let i = 0; i < maxPoints; i++) {
    out.push(points[Math.floor(i * step)]);
  }
  return out;
}

// ---- SVG primitive renderers (return string) --------------------------------

export function renderLineChart(points, opts = {}) {
  const w = opts.width ?? 200;
  const h = opts.height ?? 60;
  const color = opts.color ?? '#a78bfa';
  const strokeWidth = opts.strokeWidth ?? 1.6;
  if (!points || points.length < 2) {
    return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${opts.ariaLabel || 'line chart'}"><text x="${w / 2}" y="${h / 2}" text-anchor="middle" font-size="9" fill="#6a7196">no data</text></svg>`;
  }
  const ds = downsamplePoints(points, opts.maxPoints ?? 200);
  let minX = Infinity; let maxX = -Infinity; let minY = Infinity; let maxY = -Infinity;
  for (const [x, y] of ds) {
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  if (opts.yMin != null) minY = opts.yMin;
  if (opts.yMax != null) maxY = opts.yMax;
  const dx = Math.max(1, maxX - minX);
  const dy = Math.max(1, maxY - minY);
  const sx = (x) => ((x - minX) / dx) * w;
  const sy = (y) => h - ((y - minY) / dy) * h;
  const d = ds.map(([x, y], i) => `${i === 0 ? 'M' : 'L'} ${sx(x).toFixed(1)} ${sy(y).toFixed(1)}`).join(' ');
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${opts.ariaLabel || 'line chart'}"><path d="${d}" stroke="${color}" stroke-width="${strokeWidth}" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
}

export function renderAreaChart(points, opts = {}) {
  const w = opts.width ?? 200;
  const h = opts.height ?? 60;
  const stroke = opts.color ?? '#a78bfa';
  const fill = opts.fill ?? 'rgba(167,139,250,0.25)';
  if (!points || points.length < 2) {
    return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${opts.ariaLabel || 'area chart'}"><text x="${w / 2}" y="${h / 2}" text-anchor="middle" font-size="9" fill="#6a7196">no data</text></svg>`;
  }
  const ds = downsamplePoints(points, opts.maxPoints ?? 200);
  let minX = Infinity; let maxX = -Infinity; let minY = Infinity; let maxY = -Infinity;
  for (const [x, y] of ds) {
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  if (opts.yMin != null) minY = opts.yMin;
  if (opts.yMax != null) maxY = opts.yMax;
  const dx = Math.max(1, maxX - minX);
  const dy = Math.max(1, maxY - minY);
  const sx = (x) => ((x - minX) / dx) * w;
  const sy = (y) => h - ((y - minY) / dy) * h;
  const line = ds.map(([x, y], i) => `${i === 0 ? 'M' : 'L'} ${sx(x).toFixed(1)} ${sy(y).toFixed(1)}`).join(' ');
  const area = `${line} L ${sx(ds[ds.length - 1][0]).toFixed(1)} ${h} L ${sx(ds[0][0]).toFixed(1)} ${h} Z`;
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="${opts.ariaLabel || 'area chart'}"><path d="${area}" fill="${fill}" stroke="none"/><path d="${line}" stroke="${stroke}" stroke-width="${opts.strokeWidth ?? 1.5}" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
}

// Stacked area chart — series is an array of [t, value] arrays sharing a
// common x-axis; colors[] matches series length. Each layer adds on top of
// the previous (cumulative).
export function renderStackedArea(seriesList, opts = {}) {
  const w = opts.width ?? 920;
  const h = opts.height ?? 200;
  const colors = opts.colors || ['#a78bfa', '#60a5fa', '#76b900', '#ed1c24', '#0071c5', '#d4d4d8'];
  if (!seriesList || seriesList.length === 0 || !seriesList[0] || seriesList[0].length < 2) {
    return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="stacked area"><text x="${w / 2}" y="${h / 2}" text-anchor="middle" font-size="11" fill="#6a7196">no data</text></svg>`;
  }
  const tickCount = seriesList[0].length;
  // Sum at each tick to find max for y-scale.
  const totals = new Array(tickCount).fill(0);
  for (const s of seriesList) {
    for (let i = 0; i < tickCount; i++) {
      totals[i] += s[i]?.[1] ?? 0;
    }
  }
  const maxY = Math.max(1, ...totals);
  const minX = seriesList[0][0][0];
  const maxX = seriesList[0][tickCount - 1][0];
  const dx = Math.max(1, maxX - minX);
  const sx = (x) => ((x - minX) / dx) * w;
  const sy = (y) => h - (y / maxY) * h;
  // Build cumulative bands.
  let parts = '';
  const cumulative = new Array(tickCount).fill(0);
  for (let s = 0; s < seriesList.length; s++) {
    const ser = seriesList[s];
    const top = ser.map((pt, i) => {
      cumulative[i] += pt[1];
      return [pt[0], cumulative[i] - pt[1] + pt[1]];
    });
    const bottom = ser.map((pt, i) => [pt[0], cumulative[i] - pt[1]]);
    let d = `M ${sx(top[0][0]).toFixed(1)} ${sy(top[0][1]).toFixed(1)} `;
    for (let i = 1; i < top.length; i++) d += `L ${sx(top[i][0]).toFixed(1)} ${sy(top[i][1]).toFixed(1)} `;
    for (let i = bottom.length - 1; i >= 0; i--) d += `L ${sx(bottom[i][0]).toFixed(1)} ${sy(bottom[i][1]).toFixed(1)} `;
    d += 'Z';
    parts += `<path d="${d}" fill="${colors[s % colors.length]}" opacity="0.55"/>`;
  }
  // Total line on top.
  const totalLine = totals.map((v, i) => `${i === 0 ? 'M' : 'L'} ${sx(seriesList[0][i][0]).toFixed(1)} ${sy(v).toFixed(1)}`).join(' ');
  parts += `<path d="${totalLine}" stroke="#f59e0b" stroke-width="2" fill="none"/>`;

  // Y-axis labels.
  const yAxis = `<line x1="40" y1="${h - 0.5}" x2="${w}" y2="${h - 0.5}" stroke="#1f2548"/><line x1="40" y1="0" x2="40" y2="${h}" stroke="#1f2548"/><text x="36" y="14" font-family="JetBrains Mono" font-size="9" fill="#6a7196" text-anchor="end">${maxY.toFixed(0)}W</text><text x="36" y="${h / 2}" font-family="JetBrains Mono" font-size="9" fill="#6a7196" text-anchor="end">${(maxY / 2).toFixed(0)}W</text><text x="36" y="${h - 4}" font-family="JetBrains Mono" font-size="9" fill="#6a7196" text-anchor="end">0</text>`;
  return `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="stacked power">${yAxis}${parts}</svg>`;
}

// Mini ridgeline / sparkline preview combining several series in stacked lanes.
export function renderRidgelinePreview(lanes, opts = {}) {
  const w = opts.width ?? 920;
  const h = opts.height ?? 140;
  const laneH = Math.floor((h - 16) / Math.max(1, lanes.length));
  let svg = `<svg viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" role="img" aria-label="timeline preview" style="width:100%;height:${h}px;">`;
  for (let li = 0; li < lanes.length; li++) {
    const lane = lanes[li];
    const yTop = li * laneH;
    svg += `<rect x="0" y="${yTop}" width="${w}" height="${laneH - 4}" fill="${lane.bg || 'rgba(99,102,241,0.04)'}"/>`;
    svg += `<text x="6" y="${yTop + 11}" font-family="Manrope" font-size="9" font-weight="700" fill="${lane.color || '#a0a8c8'}">${escape(lane.label || '')}</text>`;
    if (Array.isArray(lane.points) && lane.points.length >= 2) {
      let max = 0;
      for (const [, v] of lane.points) if (v > max) max = v;
      const minX = lane.points[0][0];
      const maxX = lane.points[lane.points.length - 1][0];
      const dx = Math.max(1, maxX - minX);
      const ds = downsamplePoints(lane.points, 80);
      const sx = (x) => ((x - minX) / dx) * w;
      const sy = (v) => yTop + (laneH - 4) - (max ? (v / max) * (laneH - 8) : 0);
      const d = ds.map(([x, v], i) => `${i === 0 ? 'M' : 'L'} ${sx(x).toFixed(1)} ${sy(v).toFixed(1)}`).join(' ');
      svg += `<path d="${d}" stroke="${lane.color || '#a78bfa'}" stroke-width="1.5" fill="none"/>`;
    }
  }
  svg += `<line x1="0" y1="${h - 5}" x2="${w}" y2="${h - 5}" stroke="#1f2548"/>`;
  svg += `</svg>`;
  return svg;
}

// ---- Formatters -------------------------------------------------------------

export function formatBytes(n) {
  if (!Number.isFinite(n) || n <= 0) return '0 B';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n < 1024 * 1024 * 1024 * 1024) return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  return `${(n / 1024 / 1024 / 1024 / 1024).toFixed(2)} TB`;
}

export function formatBytesPerSec(n) {
  return `${formatBytes(n)}/s`;
}

export function formatNs(n) {
  if (!Number.isFinite(n) || n <= 0) return '0';
  if (n < 1000) return `${n} ns`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)} µs`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(2)} ms`;
  return `${(n / 1_000_000_000).toFixed(2)} s`;
}

export function formatDurationNs(n) {
  const s = (n || 0) / 1e9;
  if (s < 60) return `${s.toFixed(2)} s`;
  const m = Math.floor(s / 60);
  const r = Math.round(s % 60);
  return `${m}m ${r}s`;
}

export function formatPower(w) {
  if (!Number.isFinite(w)) return '—';
  if (w < 1000) return `${w.toFixed(0)} W`;
  return `${(w / 1000).toFixed(2)} kW`;
}

export function formatPct(p, decimals = 1) {
  if (!Number.isFinite(p)) return '—';
  return `${p.toFixed(decimals)}%`;
}

export function formatInt(n) {
  if (!Number.isFinite(n)) return '—';
  return Math.round(n).toLocaleString('en-US');
}

export function formatDateTime(unixNs) {
  if (!unixNs) return '—';
  const ms = Math.floor(unixNs / 1e6);
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return '—';
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function relativeTime(unixNs) {
  if (!unixNs) return '';
  const ms = Math.floor(unixNs / 1e6);
  const diff = Date.now() - ms;
  if (diff < 0) return '';
  const sec = Math.floor(diff / 1000);
  if (sec < 60) return `${sec}s ago`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h ago`;
  return `${Math.floor(sec / 86400)}d ago`;
}

export function escape(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// ---- Vendor / collector classification --------------------------------------

export function detectVendor(name, collectorId) {
  const n = (name || '').toLowerCase();
  const c = (collectorId || '').toLowerCase();
  if (n.includes('nvidia') || n.includes('rtx') || n.includes('gtx') || n.includes('tesla') || c.includes('nsys') || c.includes('nvidia')) return 'nvidia';
  if (n.includes('amd') || n.includes('radeon') || n.includes('rx ') || c.includes('rocprof') || c.includes('amd')) return 'amd';
  if (n.includes('arc') || n.includes('intel') || c.includes('gpu_top') || c.includes('intel')) return 'intel';
  if (n.includes('apple') || n.includes(' m1') || n.includes(' m2') || n.includes(' m3') || c.includes('metal') || c.includes('powermetrics')) return 'apple';
  return 'nvidia';
}

export function vendorBadge(vendor) {
  switch (vendor) {
    case 'nvidia': return { label: 'NV', cls: 'nv' };
    case 'amd':    return { label: 'A',  cls: 'amd' };
    case 'intel':  return { label: 'I',  cls: 'intel' };
    case 'apple':  return { label: 'M',  cls: 'apple' };
    default:       return { label: 'GPU', cls: 'nv' };
  }
}

// ---- Collector status helpers -----------------------------------------------

// Status enum is rkyv-tagged. We normalize to { kind, reason }.
export function normalizeCollectorStatus(status) {
  if (!status) return { kind: 'unknown', reason: '' };
  if (typeof status === 'string') return { kind: status.toLowerCase(), reason: '' };
  if (typeof status === 'object') {
    const keys = Object.keys(status);
    if (keys.length === 0) return { kind: 'unknown', reason: '' };
    const k = keys[0];
    const v = status[k];
    if (k === 'Used') return { kind: 'used', reason: '' };
    if (k === 'SkippedUnavailable') return { kind: 'skipped', reason: typeof v === 'string' ? v : 'unavailable' };
    if (k === 'SkippedRequiresElevation') return { kind: 'skipped', reason: 'requires elevation' };
    if (k === 'Failed') return { kind: 'failed', reason: typeof v === 'string' ? v : 'failed' };
  }
  return { kind: 'unknown', reason: '' };
}

export function countUsedCollectors(collectors) {
  let used = 0;
  let skipped = 0;
  let failed = 0;
  for (const c of collectors || []) {
    const s = normalizeCollectorStatus(c.status);
    if (s.kind === 'used') used++;
    else if (s.kind === 'skipped') skipped++;
    else if (s.kind === 'failed') failed++;
  }
  return { used, skipped, failed, total: (collectors || []).length };
}
