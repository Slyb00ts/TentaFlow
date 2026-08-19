// =============================================================================
// File: modules/analytics.js — unified "Analytics" admin screen: overview,
// users & groups, models, nodes & services, token limits, billing. Reads the
// mesh-wide model_metrics_rollup over the binary protocol; sticky filters
// auto-reload every tab; row click drills into an entity in the same tab.
// Example: Router.register('analytics', AnalyticsScreen)
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import {
  byId, escapeHtml, escapeAttr, toast,
  fmtCompact, fmtExact, fmtCurrency, fmtPct, fmtMs, fmtDuration,
} from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-sparkline.js';
import '/js/components/tf-bar-chart.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-modal.js';
import '/js/components/tf-spinner.js';
import { TfModal } from '/js/components/tf-modal.js';

const T = (key, params) => I18n.t(`analytics.${key}`, params);
const lang = () => I18n.getLanguage();

// A node counts as online when the mesh saw it within this window.
const LIVE_WINDOW_MS = 120_000;
const RELOAD_DEBOUNCE_MS = 150;
const COUNT_UP_MS = 560;
const TABS = ['overview', 'users', 'models', 'nodes', 'limits', 'billing'];

// Screen state lives for the mount lifetime; filters persist across tabs.
const state = {
  me: null,
  tab: 'overview',
  filters: { period: 'monthly', periodKey: currentMonth(), hour: currentHour(), node: '', model: '' },
  usersSub: 'user',
  usersSearch: '',
  limitsPeriod: '',
  limitsScope: '',
  billingBy: 'user',
  // Drill-down target: { kind: user|group|model|node, id, name, sub }.
  drill: null,
  models: [],
  // Raw catalog entries (model × node × engine × endpoint) for service naming.
  catalog: [],
  nodes: [],
  users: [],
  groups: [],
  coordinatorId: null,
  // Model id → stable tile palette index (assigned in first-seen order, so one
  // model keeps its colour across every view of the mount).
  tileIndex: new Map(),
  cache: new Map(),
  reloadTimer: null,
  loadSeq: 0,
  exportCsv: null,
};

// ---------------------------------------------------------------------------
// Dates / periods.
// ---------------------------------------------------------------------------

function pad2(n) { return String(n).padStart(2, '0'); }
function todayIso() {
  const d = new Date();
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
}
function currentMonth() {
  const d = new Date();
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}`;
}
function currentHour() { return pad2(new Date().getHours()); }

function recentMonths(n = 12) {
  const out = [];
  const d = new Date();
  d.setDate(1);
  for (let i = 0; i < n; i += 1) {
    out.push(`${d.getFullYear()}-${pad2(d.getMonth() + 1)}`);
    d.setMonth(d.getMonth() - 1);
  }
  return out;
}

// Period key sent to Core: hourly = YYYY-MM-DDTHH, daily = YYYY-MM-DD, monthly = YYYY-MM.
function effectivePeriodKey(f = state.filters) {
  if (f.period === 'hourly') return `${f.periodKey}T${f.hour}`;
  return f.periodKey;
}

// The n-th previous period key of the current granularity (n = 1 → previous).
function shiftPeriodKey(n) {
  const f = state.filters;
  if (f.period === 'monthly') {
    const [y, m] = f.periodKey.split('-').map(Number);
    const d = new Date(Date.UTC(y, m - 1 - n, 1));
    return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}`;
  }
  const [y, m, dd] = f.periodKey.split('-').map(Number);
  if (f.period === 'daily') {
    const d = new Date(Date.UTC(y, m - 1, dd - n));
    return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;
  }
  const d = new Date(Date.UTC(y, m - 1, dd, Number(f.hour) - n));
  return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}T${pad2(d.getUTCHours())}`;
}

// Bucket dimension for the activity chart of the current period.
function bucketGroupBy() {
  if (state.filters.period === 'monthly') return 'day';
  return 'hour';
}

// Human period label: "sierpień 2026" / "19.08.2026" / "19.08.2026, 11:00".
function periodLabel() {
  const f = state.filters;
  const [y, m, d] = f.periodKey.split('-').map(Number);
  if (f.period === 'monthly') {
    return new Intl.DateTimeFormat(lang(), { month: 'long', year: 'numeric', timeZone: 'UTC' }).format(new Date(Date.UTC(y, m - 1, 1)));
  }
  const day = new Intl.DateTimeFormat(lang(), { day: '2-digit', month: '2-digit', year: 'numeric', timeZone: 'UTC' }).format(new Date(Date.UTC(y, m - 1, d || 1)));
  return f.period === 'hourly' ? `${day}, ${f.hour}:00` : day;
}

// Every bucket key of a period (day keys for a month, hour keys for a day),
// capped at the current bucket when the period is still running, so charts
// and sparklines render gaps as empty columns at the right x instead of
// collapsing to the buckets that happen to have rows.
function periodBucketKeys(periodKey = effectivePeriodKey()) {
  const f = state.filters;
  const now = new Date();
  const keys = [];
  if (f.period === 'monthly') {
    const [y, m] = periodKey.split('-').map(Number);
    const days = new Date(Date.UTC(y, m, 0)).getUTCDate();
    const current = periodKey === currentMonth();
    const last = current ? now.getDate() : days;
    for (let d = 1; d <= last; d += 1) keys.push(`${periodKey}-${pad2(d)}`);
    return keys;
  }
  if (f.period === 'daily') {
    const current = periodKey === todayIso();
    const last = current ? now.getHours() : 23;
    for (let h = 0; h <= last; h += 1) keys.push(`${periodKey}T${pad2(h)}:00:00Z`);
    return keys;
  }
  return [`${periodKey}:00:00Z`];
}

// Rollup bucket rows merged onto the full bucket key list (zero rows where the
// period has no data). Hour keys arrive as full RFC3339 stamps, day keys as
// YYYY-MM-DD.
function zeroFillBuckets(rows, periodKey) {
  const byKey = new Map(rows.map((r) => [String(r.key), r]));
  return periodBucketKeys(periodKey).map((key) => byKey.get(key) || { key, promptTokens: 0, completionTokens: 0, totalTokens: 0, requestCount: 0, errorCount: 0, cost: 0 });
}

function bucketLabel(key) {
  const s = String(key);
  if (state.filters.period === 'monthly') {
    const m = s.match(/^\d{4}-(\d{2})-(\d{2})$/);
    return m ? `${m[2]}.${m[1]}` : s;
  }
  const m = s.match(/T(\d{2})/);
  if (m) return `${m[1]}:00`;
  return s.length > 10 ? s.slice(-5) : s;
}

function relTime(iso) {
  if (!iso) return T('rel_never');
  const ts = new Date(iso).getTime();
  if (!Number.isFinite(ts)) return T('rel_never');
  const diff = Date.now() - ts;
  const abs = Math.abs(diff);
  const future = diff < 0;
  let n; let unit;
  if (abs < 60_000) { n = Math.round(abs / 1000); unit = 's'; }
  else if (abs < 3_600_000) { n = Math.round(abs / 60_000); unit = 'min'; }
  else if (abs < 86_400_000) { n = Math.round(abs / 3_600_000); unit = 'h'; }
  else { n = Math.round(abs / 86_400_000); unit = 'd'; }
  return T(future ? `rel_in_${unit}` : `rel_ago_${unit}`, { n });
}

function isLive(iso) {
  if (!iso) return false;
  const ts = new Date(iso).getTime();
  return Number.isFinite(ts) && Date.now() - ts < LIVE_WINDOW_MS;
}

// ---------------------------------------------------------------------------
// Formatting.
// ---------------------------------------------------------------------------

function shortId(id) {
  const s = String(id || '');
  return s.length > 14 ? `${s.slice(0, 8)}…${s.slice(-4)}` : s;
}
// Middle-truncated long id for card heads (node ids are 64 hex chars).
function midId(id) {
  const s = String(id || '');
  return s.length > 28 ? `${s.slice(0, 18)}…${s.slice(-5)}` : s;
}
function num(v) { return Number(v || 0); }
function compact(v) { return fmtCompact(num(v), lang()); }
function exact(v) { return fmtExact(num(v), lang()); }
function money(v) { return fmtCurrency(num(v), 'PLN', lang()); }
// Whole-currency amount for compact contexts (tables/KPI): "1 044 zł".
function moneyShort(v) {
  return new Intl.NumberFormat(lang(), { style: 'currency', currency: 'PLN', maximumFractionDigits: 0 }).format(num(v));
}
function pct(fraction, digits = 1) { return fmtPct(num(fraction), digits, lang()); }
function ms(v) { return v == null ? '—' : fmtMs(Number(v), lang()); }
function tokS(v) { return v == null ? '—' : `${fmtExact(Math.round(Number(v)), lang())} tok/s`; }
function audio(v) { return num(v) > 0 ? fmtDuration(num(v), lang()) : '—'; }

// Aggregated cost where part of the usage has no pricing: "~1 044 zł ⚠" with
// the explanation in the tooltip; the card footer repeats it once. Usage
// entirely without pricing is not "0 zł" but unknown: "—" + "brak cennika".
function costCell(cost, missing, exactMode = false) {
  const base = exactMode ? money(cost) : moneyShort(cost);
  if (!missing) return escapeHtml(base);
  if (num(cost) === 0) return `<div class="tf-table__perf" title="${escapeAttr(T('missing_pricing_hint'))}"><b>—</b><span>${escapeHtml(T('missing_pricing'))}</span></div>`;
  return `<span class="tf-table__partial" title="${escapeAttr(T('missing_pricing_hint'))}">~${escapeHtml(base)}<svg class="tf-table__warn-ico" aria-hidden="true"><use href="#i-alert"></use></svg></span>`;
}
// Footer hint shown once per card when any row carries a partial cost.
function partialHint(missing) { return missing ? ` · ${escapeHtml(T('partial_cost_note'))}` : ''; }

function numTitle(v) { return `title="${escapeAttr(exact(v))}"`; }
function compactCell(v, bold = false) {
  const txt = escapeHtml(compact(v));
  return `<span ${numTitle(v)}>${bold ? `<b>${txt}</b>` : txt}</span>`;
}

// ---------------------------------------------------------------------------
// Cell builders (controls.css classes only — they render inside tf-table).
// ---------------------------------------------------------------------------

function chip(status, label, dot = false) {
  const tone = status === 'ok' ? 'success' : status === 'warn' ? 'warning' : status === 'err' ? 'critical' : 'muted';
  const dotHtml = dot ? `<span class="tf-chip-dot tf-chip-dot--tone-${tone}"></span>` : '';
  return `<span class="tf-chip tf-chip--outline ${escapeAttr(status)}">${dotHtml}${escapeHtml(label)}</span>`;
}
function tfChip(status, label, dot = false, dotTone = null) {
  const tone = dotTone || (status === 'ok' ? 'success' : status === 'warn' ? 'warning' : status === 'err' ? 'critical' : 'muted');
  return `<tf-chip variant="outline" status="${escapeAttr(status)}"${dot ? ` dot dot-tone="${tone}"` : ''}>${escapeHtml(label)}</tf-chip>`;
}

// Human backend names; the rollup stores engine ids.
const BACKEND_NAMES = { vllm: 'vLLM', llamacpp: 'llama.cpp', llama_cpp: 'llama.cpp', 'llama-cpp': 'llama.cpp', whisper: 'whisper', mlx: 'MLX', sglang: 'SGLang', http: 'HTTP', ollama: 'Ollama', sherpa: 'sherpa-onnx', embedded: 'embedded' };
function backendLabel(id) {
  const key = String(id || '').toLowerCase();
  return BACKEND_NAMES[key] || String(id || '—');
}
// Backend chip with the engine detail when the catalog knows more than the
// rollup id (engine id differing from the backend, e.g. vllm vs vllm-dspark).
function backendChip(row) {
  const backend = row.backend || '';
  const m = state.models.find((x) => x.value === row.modelId);
  const engine = m?.engineId && m.engineId.toLowerCase() !== String(backend).toLowerCase() ? m.engineId : '';
  return chip('accent', engine ? `${backendLabel(backend)} · ${engine}` : backendLabel(backend));
}
// Modality of a service row: the catalog category of its model, else the
// backend (whisper = speech-to-text), else an LLM.
function rowModality(row) {
  const cat = state.models.find((m) => m.value === row.modelId)?.category;
  if (cat) return cat;
  return /whisper|sherpa/i.test(String(row.backend || '')) ? 'stt' : 'llm';
}
function isSttRow(row) { return rowModality(row) === 'stt'; }
// Two-line service cell named after the catalog deployment of this model on
// this node: "vllm-5000 / llm · port 5000" for an HTTP endpoint,
// "llamacpp-embedded / llm · in-proc" for an in-process engine; without a
// catalog match the backend name + modality · the service key tail (the part
// after "backend:"), so two deployments of one backend stay distinguishable.
// The model id never repeats here (it has its own column).
function serviceCell(row) {
  const modality = rowModality(row);
  const cat = state.catalog.find((m) => m.modelName === row.modelId && m.nodeId === row.nodeId)
    || state.catalog.find((m) => m.modelName === row.modelId);
  const engine = (cat?.engineId || row.backend || '').toLowerCase();
  const port = cat?.endpointUrl ? (String(cat.endpointUrl).match(/:(\d{2,5})(?:\/|$)/) || [])[1] : null;
  if (port) return entCell({ title: `${engine}-${port}`, sub: `${modality} · ${T('port_n', { port })}` });
  if (/embedded|in[-_]?proc|native/i.test(String(cat?.transport || ''))) return entCell({ title: `${engine}-embedded`, sub: `${modality} · ${T('in_proc')}` });
  const key = String(row.serviceKey || '');
  const tail = key.replace(new RegExp(`^${String(row.backend || '').replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}[:/]`, 'i'), '');
  return entCell({ title: backendLabel(row.backend), sub: tail ? `${modality} · ${tail}` : modality });
}

function entCell({ title, sub, tile, tileCls, dot, dotTitle, mono = true }) {
  let lead = '';
  if (dot != null) {
    lead = `<span class="tf-table__dot${dot ? ' tf-table__dot--live' : ''}"${dotTitle ? ` title="${escapeAttr(dotTitle)}"` : ''}></span>`;
  } else if (tile) {
    lead = `<span class="tf-table__tile ${tileCls || ''}">${escapeHtml(tile)}</span>`;
  }
  const subHtml = sub ? `<div class="tf-table__cell-sub${mono ? ' tf-table__cell-sub--mono' : ''}" title="${escapeAttr(sub)}">${escapeHtml(sub)}</div>` : '';
  return `<div class="tf-table__ent">${lead}<div><div class="tf-table__cell-title" title="${escapeAttr(title)}"><b>${escapeHtml(title)}</b></div>${subHtml}</div></div>`;
}

function shareCell(fraction, idx = 0) {
  const p = Math.max(0, Math.min(100, num(fraction) * 100));
  const delay = (idx * 0.08).toFixed(2);
  return `<div class="tf-table__share"><div class="tf-table__bar"><i class="tf-table__bar-fill" style="width:${p.toFixed(1)}%;animation-delay:${delay}s"></i></div><span class="tf-table__share-pct">${escapeHtml(pct(fraction, 0))}</span></div>`;
}

function perfCell(main, sub) {
  return `<div class="tf-table__perf"><b>${escapeHtml(main)}</b>${sub ? `<span>${escapeHtml(sub)}</span>` : ''}</div>`;
}

// Error badges graded by rate: none = ok, < 1 % neutral, ≥ 1 % warn, ≥ 5 % err —
// green is reserved for "no errors at all".
function errorStatus(rate) {
  const r = num(rate);
  return r >= 0.05 ? 'err' : r >= 0.01 ? 'warn' : r > 0 ? 'neutral' : 'ok';
}
// Error-rate chips always carry two decimals ("0,02%"), unlike share percentages.
function pctFixed2(fraction) { return `${(num(fraction) * 100).toLocaleString(lang(), { minimumFractionDigits: 2, maximumFractionDigits: 2 })}%`; }
function errorsChip(rate) { return chip(errorStatus(rate), pctFixed2(rate)); }
function errorCountChip(count, requests) {
  const c = num(count);
  const rate = num(requests) ? c / num(requests) : 0;
  return chip(errorStatus(rate), exact(c));
}

// Letter tile palette assigned per model in first-seen order.
const TILE_CLASSES = ['tf-table__tile--t1', 'tf-table__tile--t2', 'tf-table__tile--t3', 'tf-table__tile--t4', 'tf-table__tile--t5'];
function tileClassFor(modelId) {
  const id = String(modelId || '');
  if (!state.tileIndex.has(id)) state.tileIndex.set(id, state.tileIndex.size);
  return TILE_CLASSES[state.tileIndex.get(id) % TILE_CLASSES.length];
}

// Display helpers over wire rows.
function rowName(r) { return r.displayName || r.display_name || String(r.key || ''); }
function rowSub(r, kind) {
  if (kind === 'model') return r.displayName ? String(r.key) : '';
  if (kind === 'user') return r.subtitle || '';
  if (kind === 'group') return r.memberCount != null ? T('members_count', { count: num(r.memberCount) }) : '';
  if (kind === 'node') return shortId(r.key);
  return '';
}
function modelName(id) {
  const m = state.models.find((x) => x.value === id);
  return m ? m.label : String(id || '');
}
function nodeName(id, display) {
  if (display) return display;
  const n = state.nodes.find((x) => x.value === id);
  return n && n.label !== n.value ? n.label : shortId(id);
}
// Sub-line of a model row: "whisper · STT · 17,8 h" for a speech model (the
// slug would only repeat the title), otherwise the model id + engine.
function modelSub(r, withEngine = false) {
  const id = String(r.key || '');
  const engine = backendLabel(state.catalog.find((m) => m.modelName === id)?.engineId || state.models.find((m) => m.value === id)?.engineId || (num(r.audioMs) > 0 ? 'whisper' : ''));
  const stt = num(r.audioMs) > 0 || isSttRow({ modelId: id, backend: '' });
  if (stt) return [engine, 'STT', num(r.audioMs) > 0 ? audio(r.audioMs) : ''].filter(Boolean).join(' · ');
  if (!withEngine) return r.displayName ? id : '';
  return engine && engine !== '—' ? `${id} · ${engine}` : id;
}
function modelCell(id, display, sub) {
  const name = display || modelName(id);
  return entCell({ title: name, sub: sub ?? (name !== id ? id : ''), tile: name.charAt(0).toUpperCase(), tileCls: tileClassFor(id) });
}
// Node cell: liveness dot + name + short id; an offline node states when it
// was last seen right in the sub-line, so a grey dot is never unexplained.
function nodeCell(id, display, lastSeen, plain = false, offlineNote = true) {
  const live = isLive(lastSeen);
  const sub = !plain && offlineNote && !live ? `${shortId(id)} · ${T('status_offline_since', { rel: relTime(lastSeen) })}` : shortId(id);
  return entCell({ title: nodeName(id, display), sub, dot: plain ? null : live, dotTitle: live ? T('status_online') : T('last_seen', { rel: relTime(lastSeen) }) });
}
// Status chip of a node: green "online" within the liveness window, otherwise
// a grey chip carrying the relative last-contact time.
function nodeStatusChip(lastSeen) {
  return isLive(lastSeen) ? chip('ok', T('status_online'), true) : chip('neutral', relTime(lastSeen), true);
}
function humanRole(role) {
  const key = String(role || '').toLowerCase().replace(/-/g, '_');
  const known = { admin: 'role_admin', org_admin: 'role_admin', power_user: 'role_power_user', user: 'role_user', viewer: 'role_viewer' };
  return known[key] ? T(known[key]) : String(role);
}

// ---------------------------------------------------------------------------
// Protocol layer (response cache per (request, payload) for the mount lifetime).
// ---------------------------------------------------------------------------

// Responses are reused across tabs for CACHE_TTL_MS — long enough to make tab
// switches instant, short enough that node liveness (120 s window) and lease
// expiries stay truthful while the screen is open.
const CACHE_TTL_MS = 60_000;
async function cached(kind, payload) {
  const key = `${kind}:${JSON.stringify(payload || {})}`;
  const hit = state.cache.get(key);
  if (hit && Date.now() - hit.at < CACHE_TTL_MS) return hit.p;
  const p = (payload === undefined ? ApiBinary.one(kind) : ApiBinary.one(kind, payload)).catch((err) => {
    state.cache.delete(key);
    throw err;
  });
  state.cache.set(key, { p, at: Date.now() });
  return p;
}

// Summary request honouring the global node/model filters plus per-call extras
// (entity drill filters or an explicit period key).
async function fetchSummary(groupBy, extra = {}) {
  const f = state.filters;
  const payload = {
    period: f.period,
    periodKey: extra.periodKey || effectivePeriodKey(),
    groupBy,
    filterModel: extra.filterModel ?? (f.model || undefined),
    filterNode: extra.filterNode ?? (f.node || undefined),
    filterUser: extra.filterUser,
    filterGroup: extra.filterGroup,
  };
  const resp = await cached('modelMetricsSummaryRequest', payload);
  return {
    rows: Array.isArray(resp?.rows) ? resp.rows : [],
    grandTotal: resp?.grandTotal ?? resp?.grand_total ?? null,
  };
}

async function fetchNodeService(periodKey) {
  const resp = await cached('modelMetricsNodeServiceRequest', {
    period: state.filters.period,
    periodKey: periodKey || effectivePeriodKey(),
  });
  const rows = Array.isArray(resp?.rows) ? resp.rows : [];
  // Every fresh node×service response also refreshes the node list's liveness.
  for (const r of rows) {
    if (!r.nodeId) continue;
    const n = state.nodes.find((x) => x.value === r.nodeId);
    if (n) { if (r.nodeLastSeenAt) n.lastSeen = r.nodeLastSeenAt; }
    else state.nodes.push({ value: r.nodeId, label: r.nodeDisplayName || shortId(r.nodeId), lastSeen: r.nodeLastSeenAt || null });
  }
  return rows;
}

function sumRows(rows) {
  return rows.reduce((acc, r) => {
    acc.promptTokens += num(r.promptTokens);
    acc.completionTokens += num(r.completionTokens);
    acc.totalTokens += num(r.totalTokens);
    acc.audioMs += num(r.audioMs);
    acc.images += num(r.images);
    acc.requestCount += num(r.requestCount);
    acc.errorCount += num(r.errorCount);
    acc.cost += num(r.cost);
    acc.missingPricing = acc.missingPricing || !!r.missingPricing;
    return acc;
  }, {
    promptTokens: 0, completionTokens: 0, totalTokens: 0, audioMs: 0, images: 0,
    requestCount: 0, errorCount: 0, cost: 0, missingPricing: false,
  });
}

function sortByTokens(rows) {
  return [...rows].sort((a, b) => num(b.totalTokens) - num(a.totalTokens));
}

async function loadSubjects() {
  const [models, users, groups] = await Promise.all([
    ApiBinary.list('modelListRequest', { arrayKey: 'models' }).catch(() => []),
    ApiBinary.action('iamListUsersRequest').then((r) => r?.users ?? []).catch(() => []),
    ApiBinary.action('iamListGroupsRequest').then((r) => r?.groups ?? []).catch(() => []),
  ]);
  state.catalog = (Array.isArray(models) ? models : []).map((m) => ({
    modelName: m.modelName || m.model_name || '',
    nodeId: m.nodeId || m.node_id || '',
    engineId: m.engineId || m.engine_id || '',
    transport: m.transport || '',
    endpointUrl: m.endpointUrl || m.endpoint_url || '',
  }));
  const seen = new Map();
  for (const m of Array.isArray(models) ? models : []) {
    const value = m.modelName || m.model_name || '';
    if (!value || seen.has(value)) continue;
    seen.set(value, { value, label: m.displayName || m.display_name || value, engineId: m.engineId || m.engine_id || '', category: m.category || '' });
  }
  state.models = [...seen.values()].sort((a, b) => a.label.localeCompare(b.label));
  for (const m of state.models) tileClassFor(m.value);
  try {
    const c = await cached('tokenCoordinatorStatusRequest');
    state.coordinatorId = c?.coordinatorNodeId ?? c?.coordinator_node_id ?? null;
  } catch {
    state.coordinatorId = null;
  }
  state.users = Array.isArray(users) ? users : [];
  state.groups = Array.isArray(groups) ? groups : [];
  state.nodes = [];
  await fetchNodeService().catch(() => []);
}

// Models seen in rollup rows but missing from the catalog still need a filter option.
function mergeModelOptions(rows) {
  let changed = false;
  for (const r of rows) {
    const id = String(r.key || '');
    if (id && !state.models.some((m) => m.value === id)) {
      state.models.push({ value: id, label: r.displayName || id });
      changed = true;
    }
  }
  if (changed) {
    const sel = byId('an-f-model');
    if (sel) sel.setOptions(modelOptions(), state.filters.model);
  }
}
function modelOptions() {
  return [{ value: '', label: T('all_models') }, ...state.models];
}
function nodeOptions() {
  return [{ value: '', label: T('all_nodes') }, ...state.nodes.map((n) => ({ value: n.value, label: n.label }))];
}

// ---------------------------------------------------------------------------
// Screen shell.
// ---------------------------------------------------------------------------

const AnalyticsScreen = {
  get title() { return T('title'); },

  render() {
    return '<div id="an-root" class="an-root"></div>';
  },

  async mount() {
    try {
      state.me = await ApiBinary.one('authMeRequest');
    } catch {
      state.me = null;
    }
    const root = byId('an-root');
    if (!root) return;
    if (!state.me || (state.me.role !== 'admin' && !state.me.isAdmin)) {
      root.innerHTML = `<div class="an-card an-empty">${escapeHtml(T('admin_only'))}</div>`;
      return;
    }
    root.innerHTML = shellHtml();
    byId('an-tabs')?.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id && TABS.includes(id)) setTab(id);
    });
    byId('an-export')?.addEventListener('click', () => {
      if (state.exportCsv) state.exportCsv();
      else toast(T('export_empty'), 'info');
    });
    byId('an-new-quota')?.addEventListener('click', () => openQuotaEditor(null));
    await loadSubjects();
    renderToolbar();
    await renderTab();
  },

  unmount() {
    clearTimeout(state.reloadTimer);
    state.reloadTimer = null;
    state.me = null;
    state.drill = null;
    state.cache = new Map();
    state.models = [];
    state.catalog = [];
    state.nodes = [];
    state.users = [];
    state.groups = [];
    state.exportCsv = null;
    state.coordinatorId = null;
    state.tileIndex = new Map();
  },
};

function shellHtml() {
  return `
    <div class="an-head">
      <div>
        <h1 class="an-title">${escapeHtml(T('title'))}</h1>
        <div class="an-sub">${escapeHtml(T('subtitle'))}</div>
      </div>
      <div class="an-head-actions">
        <tf-button variant="outline" icon="download" id="an-export">${escapeHtml(T('export_csv'))}</tf-button>
        <tf-button variant="primary" icon="plus" id="an-new-quota" hidden>${escapeHtml(T('new_quota'))}</tf-button>
      </div>
    </div>
    <tf-tabs id="an-tabs" value="${escapeAttr(state.tab)}" variant="solid">
      ${TABS.map((id) => `<tf-tab id="${id}">${escapeHtml(T(`tab_${id}`))}</tf-tab>`).join('')}
    </tf-tabs>
    <div class="an-toolbar" id="an-toolbar"></div>
    <div id="an-panel" class="an-panel"></div>
  `;
}

function setTab(id) {
  if (state.tab === id) return;
  state.tab = id;
  state.drill = null;
  state.exportCsv = null;
  const btn = byId('an-new-quota');
  if (btn) btn.hidden = id !== 'limits';
  renderToolbar();
  renderTab();
}

function scheduleReload() {
  clearTimeout(state.reloadTimer);
  state.reloadTimer = setTimeout(() => {
    state.reloadTimer = null;
    renderTab();
  }, RELOAD_DEBOUNCE_MS);
}

// ---------------------------------------------------------------------------
// Sticky filter toolbar (shared; controls depend on the tab).
// ---------------------------------------------------------------------------

function renderToolbar() {
  const bar = byId('an-toolbar');
  if (!bar) return;
  const f = state.filters;
  const tab = state.tab;
  const parts = [];
  // A label and its control form one wrapping unit, so a narrow toolbar never
  // orphans "MODEL" on one row and its select on the next.
  const group = (k, control, cls = '') => `<span class="an-fg ${cls}"><span class="an-fl">${escapeHtml(T(k))}</span>${control}</span>`;
  const sep = '<span class="an-fl-sep"></span>';

  if (tab === 'limits') {
    parts.push(group('period', `<tf-segmented id="an-f-limits-period" value="${escapeAttr(state.limitsPeriod)}" size="md">
      <option value="">${escapeHtml(T('all_periods'))}</option>
      <option value="daily">${escapeHtml(T('period_daily_adj_cap'))}</option>
      <option value="monthly">${escapeHtml(T('period_monthly_adj_cap'))}</option>
    </tf-segmented>`, 'an-fg--seg'));
    parts.push(sep, group('scope', `<tf-select id="an-f-limits-scope" value="${escapeAttr(state.limitsScope)}">
      <option value="">${escapeHtml(T('all_scopes'))}</option>
      <option value="user">${escapeHtml(T('scope_user'))}</option>
      <option value="group">${escapeHtml(T('scope_group'))}</option>
      <option value="model">${escapeHtml(T('scope_model'))}</option>
      <option value="org">${escapeHtml(T('scope_org'))}</option>
    </tf-select>`));
    parts.push('<span class="an-spacer"></span>');
    parts.push(tfChip('accent', T('enforcement_live'), true, 'success'));
  } else {
    const periods = tab === 'billing' ? ['daily', 'monthly'] : ['hourly', 'daily', 'monthly'];
    parts.push(group('period', `<tf-segmented id="an-f-period" value="${escapeAttr(f.period)}" size="md">
      ${periods.map((p) => `<option value="${p}">${escapeHtml(T(`period_${p}`))}</option>`).join('')}
    </tf-segmented>`, 'an-fg--seg'));
    parts.push(`<span id="an-f-key-host" class="an-key-host">${periodKeyHtml()}</span>`);
    if (tab === 'billing') {
      parts.push(sep, group('bill_by', `<tf-segmented id="an-f-bill-by" value="${escapeAttr(state.billingBy)}" size="md">
        <option value="user">${escapeHtml(T('by_user'))}</option>
        <option value="group">${escapeHtml(T('by_group'))}</option>
      </tf-segmented>`, 'an-fg--seg'));
      parts.push('<span class="an-spacer"></span><span id="an-billing-note"></span>');
    } else {
      parts.push(sep);
      if (tab !== 'nodes') parts.push(group('node', '<tf-select id="an-f-node"></tf-select>'));
      if (tab !== 'models') parts.push(group('model', '<tf-select id="an-f-model"></tf-select>'));
      parts.push('<span class="an-spacer"></span>');
      if (tab === 'overview') parts.push('<span id="an-mesh-chip"></span>');
    }
  }
  bar.innerHTML = parts.join('');

  byId('an-f-node')?.setOptions(nodeOptions(), f.node);
  byId('an-f-model')?.setOptions(modelOptions(), f.model);
  renderMeshChip();

  byId('an-f-period')?.addEventListener('change', (e) => {
    const next = e.detail?.value || 'monthly';
    if (next === f.period) return;
    f.period = next;
    f.periodKey = next === 'monthly' ? currentMonth() : todayIso();
    const host = byId('an-f-key-host');
    if (host) {
      host.innerHTML = periodKeyHtml();
      wirePeriodKey();
    }
    scheduleReload();
  });
  wirePeriodKey();
  byId('an-f-node')?.addEventListener('change', (e) => { f.node = e.detail?.value || ''; scheduleReload(); });
  byId('an-f-model')?.addEventListener('change', (e) => { f.model = e.detail?.value || ''; scheduleReload(); });
  byId('an-f-bill-by')?.addEventListener('change', (e) => { state.billingBy = e.detail?.value || 'user'; scheduleReload(); });
  byId('an-f-limits-period')?.addEventListener('change', (e) => { state.limitsPeriod = e.detail?.value || ''; scheduleReload(); });
  byId('an-f-limits-scope')?.addEventListener('change', (e) => { state.limitsScope = e.detail?.value || ''; scheduleReload(); });
}

function periodKeyHtml() {
  const f = state.filters;
  if (f.period === 'monthly') {
    const months = recentMonths();
    if (!months.includes(f.periodKey)) months.unshift(f.periodKey);
    return `<tf-select id="an-f-key" value="${escapeAttr(f.periodKey)}">
      ${months.map((mo) => `<option value="${escapeAttr(mo)}">${escapeHtml(mo)}</option>`).join('')}
    </tf-select>`;
  }
  const picker = `<tf-input id="an-f-key" type="date" value="${escapeAttr(f.periodKey)}" max="${escapeAttr(todayIso())}"></tf-input>`;
  if (f.period === 'hourly') {
    const hours = Array.from({ length: 24 }, (_, i) => pad2(i));
    return `${picker}<tf-select id="an-f-hour" value="${escapeAttr(f.hour)}">
      ${hours.map((h) => `<option value="${h}">${h}:00</option>`).join('')}
    </tf-select>`;
  }
  return picker;
}

function wirePeriodKey() {
  byId('an-f-key')?.addEventListener('change', (e) => {
    const v = e.detail?.value;
    if (v && v !== state.filters.periodKey) { state.filters.periodKey = v; scheduleReload(); }
  });
  byId('an-f-hour')?.addEventListener('change', (e) => {
    const v = e.detail?.value;
    if (v && v !== state.filters.hour) { state.filters.hour = v; scheduleReload(); }
  });
}

function renderMeshChip() {
  const host = byId('an-mesh-chip');
  if (!host) return;
  const live = state.nodes.filter((n) => isLive(n.lastSeen)).length;
  host.innerHTML = live > 0
    ? tfChip('ok', T('mesh_chip_live', { count: state.nodes.length }), true)
    : tfChip('neutral', T('mesh_chip_offline', { count: state.nodes.length }), true);
}

// ---------------------------------------------------------------------------
// Shared renderers: cards, tables, KPI, chart, states.
// ---------------------------------------------------------------------------

function cardHtml({ id, title, hint, headExtra = '', body = '', cls = '', foot = true }) {
  const headHtml = title
    ? `<div class="an-c-head"><h3>${escapeHtml(title)}</h3>${headExtra}${hint ? `<div class="an-hint">${escapeHtml(hint)}</div>` : ''}</div>`
    : '';
  return `<section class="an-card ${cls}" ${id ? `id="${escapeAttr(id)}"` : ''}>${headHtml}${body}${foot && id ? `<div class="an-tfoot" id="${escapeAttr(id)}-foot"></div>` : ''}</section>`;
}

// One column absorbs the free width (`fill`, capped at 40 % by the flush
// variant); the first one unless a column asks for it explicitly. `w` pins a
// column width so stacked tables of one type share a template, `lo` marks a
// column the phone layout drops, `narrow` a percentage template that fits a
// ~280px top-list card.
function tableHtml(id, cols, { narrow = false } = {}) {
  const fillIdx = Math.max(0, cols.findIndex((c) => c.fill));
  return `<div class="an-twrap"><tf-table id="${escapeAttr(id)}" variant="flush"${narrow ? ' narrow' : ''}>
    ${cols.map((c, i) => `<tf-column key="${escapeAttr(c.key)}" label="${escapeAttr(c.label)}"${c.num ? ' align="num"' : ''}${c.renderer ? ` renderer="${c.renderer}"` : ''}${c.sortable ? ' sortable' : ''}${i === fillIdx ? ' fill' : ''}${c.w ? ` width="${escapeAttr(c.w)}"` : ''}${c.lo ? ' priority="low"' : ''}></tf-column>`).join('')}
  </tf-table></div>`;
}

function loadingHtml() {
  return `<div class="an-state"><tf-spinner size="sm" tone="primary"></tf-spinner><span>${escapeHtml(T('loading'))}</span></div>`;
}
function emptyHtml(text) {
  return `<div class="an-state an-empty">${escapeHtml(text || T('no_data'))}</div>`;
}

function setFoot(id, left, right = '') {
  const el = byId(`${id}-foot`);
  if (!el) return;
  el.innerHTML = `<span>${left}</span><span>${right}</span>`;
}

// Flush tables scroll horizontally inside their card; a right-edge mask plus
// a chevron on the card signal hidden columns until the user reaches the end.
// At rest the mask covers the partially visible column up to the last full
// column boundary (at most 45 % of the view), so the visible cut falls on cell
// padding, never mid-glyph; while the user drags, a thin fade replaces it so
// the mask does not jump column by column under the finger.
function watchTableScroll(table) {
  const card = table.closest('.an-twrap');
  const wrap = table.shadowRoot?.querySelector('.tf-table-wrap');
  if (!card || !wrap) return;
  let settle = null;
  const update = () => {
    const more = wrap.scrollWidth - wrap.clientWidth - wrap.scrollLeft > 4;
    card.classList.toggle('an-twrap--more', more);
    if (!more) return;
    const edge = wrap.getBoundingClientRect().right;
    let lastFull = edge;
    for (const th of wrap.querySelectorAll('thead th')) {
      const r = th.getBoundingClientRect();
      if (r.width === 0) continue;
      if (r.right <= edge + 1) lastFull = r.right;
      else if (r.left < edge) { lastFull = r.left; break; }
    }
    card.style.setProperty('--tf-table-mask-w', `${Math.max(0, Math.min(wrap.clientWidth * 0.45, edge - lastFull))}px`);
  };
  const onScroll = () => {
    card.classList.add('an-twrap--scrolling');
    clearTimeout(settle);
    settle = setTimeout(() => { card.classList.remove('an-twrap--scrolling'); update(); }, 140);
    card.classList.toggle('an-twrap--more', wrap.scrollWidth - wrap.clientWidth - wrap.scrollLeft > 4);
  };
  if (!table.dataset.scrollWatched) {
    table.dataset.scrollWatched = '1';
    wrap.addEventListener('scroll', onScroll, { passive: true });
    if ('ResizeObserver' in window) new ResizeObserver(update).observe(wrap);
  }
  requestAnimationFrame(update);
}

function setRows(id, rows, emptyText) {
  const table = byId(id);
  if (!table) return;
  table.rows = rows;
  watchTableScroll(table);
  const host = table.parentElement;
  const marker = host?.querySelector(`[data-empty-for="${id}"]`);
  if (!rows.length) {
    if (!marker && host) {
      table.insertAdjacentHTML('afterend', `<div class="an-state an-empty" data-empty-for="${escapeAttr(id)}">${escapeHtml(emptyText || T('no_data'))}</div>`);
    }
    table.hidden = true;
  } else {
    marker?.remove();
    table.hidden = false;
  }
}

function onRowClick(id, handler) {
  byId(id)?.addEventListener('row-click', (e) => {
    const row = e.detail?.row;
    if (row) handler(row);
  });
}

// KPI card: tf-stat-card + optional sparkline child; the value counts up via
// rAF (easeOutCubic) while the exact value sits in the title attribute.
function kpiHtml({ id, icon, label, suffix, delta, deltaType, spark, chipSlot }) {
  return `<tf-stat-card id="${escapeAttr(id)}" icon="${escapeAttr(icon)}" label="${escapeAttr(label)}" value="—"
    ${suffix ? `suffix="${escapeAttr(suffix)}"` : ''}
    ${delta ? `delta="${escapeAttr(delta)}"` : ''}
    ${deltaType ? `delta-type="${escapeAttr(deltaType)}"` : ''}>${spark ? '<tf-sparkline class="an-kpi-spark"></tf-sparkline>' : ''}${chipSlot ? `<span class="an-kpi-chip" id="${escapeAttr(id)}-chip"></span>` : ''}</tf-stat-card>`;
}

function countUp(el, target, fmt) {
  if (!el) return;
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  el.title = exact(target);
  if (reduce || !Number.isFinite(target) || target === 0) {
    el.setAttribute('value', fmt(target));
    return;
  }
  const t0 = performance.now();
  const tick = (t) => {
    const p = Math.min(1, (t - t0) / COUNT_UP_MS);
    const eased = 1 - (1 - p) ** 3;
    el.setAttribute('value', fmt(target * eased));
    if (p < 1 && el.isConnected) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

// Trend line of a bucket series: centred moving average (a 7-day window on a
// month of day buckets cancels the weekly cycle, 3 buckets otherwise), then
// downsampled to at most 10 points, so a 60 px sparkline reads as a trend
// rather than saw-tooth noise.
function trendPoints(values, maxPoints = 10) {
  const v = values.map(num);
  if (v.length < 4) return v;
  const half = v.length >= 21 ? 3 : 1;
  const avg = v.map((_, i) => {
    let sum = 0; let n = 0;
    for (let j = i - half; j <= i + half; j += 1) { if (j >= 0 && j < v.length) { sum += v[j]; n += 1; } }
    return sum / n;
  });
  if (avg.length <= maxPoints) return avg;
  const out = [];
  const step = (avg.length - 1) / (maxPoints - 1);
  for (let i = 0; i < maxPoints; i += 1) {
    const pos = i * step;
    const lo = Math.floor(pos);
    const hi = Math.min(avg.length - 1, lo + 1);
    out.push(avg[lo] + (avg[hi] - avg[lo]) * (pos - lo));
  }
  return out;
}

function applySpark(sp, points, color, height) {
  sp.height = height;
  sp.color = color;
  sp.smooth = true;
  sp.lineWidth = 1.8;
  sp.points = trendPoints(points);
}

function setSpark(cardId, points, color = 'accent') {
  const sp = byId(cardId)?.querySelector('tf-sparkline');
  if (sp) applySpark(sp, points, color, 26);
}

// Stacked prompt/completion bar chart over the zero-filled period buckets
// (completion at the base, prompt on top — the mockup pairing).
function mountTokenChart(hostId, bucketRows, height = 250) {
  const host = byId(hostId);
  if (!host) return;
  const rows = zeroFillBuckets(bucketRows);
  if (!rows.some((r) => num(r.totalTokens) > 0)) {
    host.innerHTML = emptyHtml();
    return;
  }
  host.innerHTML = '';
  const chart = document.createElement('tf-bar-chart');
  chart.xAxis = { scale: 'category' };
  chart.yAxis = { scale: 'linear' };
  chart.stacking = 'stacked';
  chart.height = height;
  chart.maxBarWidth = 36;
  chart.locale = lang();
  chart.tooltip = { enabled: true };
  chart.series = [
    { id: 'completion', name: T('legend_completion'), tone: 'primary', showInLegend: false, points: rows.map((r) => ({ x: bucketLabel(r.key), y: num(r.completionTokens) })) },
    { id: 'prompt', name: T('legend_prompt'), tone: 'accent', showInLegend: false, points: rows.map((r) => ({ x: bucketLabel(r.key), y: num(r.promptTokens) })) },
  ];
  host.appendChild(chart);
}

function legendHtml() {
  return `<div class="an-legend">
    <span><i class="an-sw an-sw--prompt"></i>${escapeHtml(T('legend_prompt'))}</span>
    <span><i class="an-sw an-sw--completion"></i>${escapeHtml(T('legend_completion'))}</span>
  </div>`;
}

function sparkCell(points) {
  const sp = document.createElement('tf-sparkline');
  sp.style.width = '80px';
  sp.style.display = 'inline-block';
  applySpark(sp, points, 'accent', 22);
  return sp;
}

// Period chart title: "Aktywność: sierpień 2026".
function activityTitle() { return T('activity_in', { period: periodLabel() }); }

// ---------------------------------------------------------------------------
// Tab dispatch.
// ---------------------------------------------------------------------------

async function renderTab() {
  const panel = byId('an-panel');
  if (!panel) return;
  const seq = ++state.loadSeq;
  state.exportCsv = null;
  try {
    if (state.drill) await renderDrill(panel, seq);
    else if (state.tab === 'overview') await renderOverview(panel, seq);
    else if (state.tab === 'users') await renderUsers(panel, seq);
    else if (state.tab === 'models') await renderModels(panel, seq);
    else if (state.tab === 'nodes') await renderNodes(panel, seq);
    else if (state.tab === 'limits') await renderLimits(panel, seq);
    else if (state.tab === 'billing') await renderBilling(panel, seq);
  } catch (err) {
    if (seq !== state.loadSeq) return;
    panel.innerHTML = `<section class="an-card">${emptyHtml(T('load_failed'))}</section>`;
    toast(err?.message || T('load_failed'), 'error');
  }
}

const stale = (seq) => seq !== state.loadSeq;

function openDrill(kind, id, name, sub) {
  state.drill = { kind, id: String(id), name: name || String(id), sub: sub || '' };
  renderTab();
  byId('an-panel')?.scrollIntoView({ block: 'start', behavior: 'smooth' });
}
function closeDrill() {
  state.drill = null;
  renderTab();
}

// ---------------------------------------------------------------------------
// Tab: Overview.
// ---------------------------------------------------------------------------

async function renderOverview(panel, seq) {
  panel.innerHTML = `
    <div class="an-kpi-grid" id="an-kpis">
      ${kpiHtml({ id: 'an-kpi-tokens', icon: 'database', label: T('kpi_total_tokens'), spark: true })}
      ${kpiHtml({ id: 'an-kpi-requests', icon: 'bar-chart', label: T('kpi_requests'), spark: true })}
      ${kpiHtml({ id: 'an-kpi-ttft', icon: 'clock-glance', label: T('kpi_ttft_p50'), suffix: 'ms' })}
      ${kpiHtml({ id: 'an-kpi-decode', icon: 'zap', label: T('kpi_decode_p50'), suffix: 'tok/s' })}
      ${kpiHtml({ id: 'an-kpi-errors', icon: 'alert', label: T('kpi_errors'), suffix: '%' })}
      ${kpiHtml({ id: 'an-kpi-cost', icon: 'bolt', label: T('kpi_cost'), suffix: 'zł', chipSlot: true })}
    </div>
    ${cardHtml({ id: 'an-chart-card', title: T('chart_tokens_title'), hint: T('chart_hover_hint'), headExtra: legendHtml(), body: `<div class="an-c-body"><div id="an-chart">${loadingHtml()}</div></div>`, foot: false, cls: 'an-d2' })}
    <div class="an-grid an-cols-3">
      ${cardHtml({ id: 'an-top-models', title: T('top_models'), hint: T('hint_token_share'), body: tableHtml('an-top-models-table', [
        { key: 'name', label: T('col_model'), renderer: 'html', w: '50%' },
        { key: 'total', label: T('col_tokens'), renderer: 'html', num: true, w: '22%' },
        { key: 'share', label: T('col_share'), renderer: 'html', w: '28%' },
      ], { narrow: true }), cls: 'an-d3' })}
      ${cardHtml({ id: 'an-top-users', title: T('top_users'), hint: T('hint_click_details'), body: tableHtml('an-top-users-table', [
        { key: 'name', label: T('col_user'), renderer: 'html', w: '50%' },
        { key: 'total', label: T('col_tokens'), renderer: 'html', num: true, w: '25%' },
        { key: 'cost', label: T('col_cost'), renderer: 'html', num: true, w: '25%' },
      ], { narrow: true }), cls: 'an-d4' })}
      ${cardHtml({ id: 'an-top-nodes', title: T('top_nodes'), hint: T('hint_token_production'), body: tableHtml('an-top-nodes-table', [
        { key: 'name', label: T('col_node'), renderer: 'html', w: '50%' },
        { key: 'total', label: T('col_tokens'), renderer: 'html', num: true, w: '22%' },
        { key: 'status', label: T('col_status'), renderer: 'html', w: '28%' },
      ], { narrow: true }), cls: 'an-d5' })}
    </div>`;

  const prevKey = shiftPeriodKey(1);
  const [byGroup, buckets, byModel, byUser, byNode, prev] = await Promise.all([
    fetchSummary('group'),
    fetchSummary(bucketGroupBy()),
    fetchSummary('model'),
    fetchSummary('user'),
    fetchSummary('node'),
    fetchSummary('group', { periodKey: prevKey }).catch(() => ({ rows: [], grandTotal: null })),
  ]);
  if (stale(seq)) return;
  mergeModelOptions(byModel.rows);

  // grand_total (unique per-user sum) carries org-wide percentiles.
  const g = byGroup.grandTotal;
  const totals = g || sumRows(byModel.rows);
  const prevTotals = prev.grandTotal || sumRows(prev.rows);
  const errRate = g ? num(g.errorRate) : (totals.requestCount ? totals.errorCount / totals.requestCount : 0);
  const missingModels = byModel.rows.filter((r) => r.missingPricing).length;
  const sortedBuckets = zeroFillBuckets(buckets.rows);

  const tokensCard = byId('an-kpi-tokens');
  countUp(tokensCard, num(totals.totalTokens), compact);
  if (prevTotals.totalTokens > 0) {
    const d = (num(totals.totalTokens) - prevTotals.totalTokens) / prevTotals.totalTokens;
    tokensCard?.setAttribute('delta', T('vs_prev', { pct: pct(Math.abs(d), 0), period: prevKey }));
    tokensCard?.setAttribute('delta-type', d >= 0 ? 'up' : 'down');
  } else {
    tokensCard?.setAttribute('delta', T('no_prev_period'));
  }
  setSpark('an-kpi-tokens', sortedBuckets.map((r) => num(r.totalTokens)));

  const reqCard = byId('an-kpi-requests');
  countUp(reqCard, num(totals.requestCount), compact);
  reqCard?.setAttribute('delta', T('requests_sub', { errors: compact(totals.errorCount), ok: pct(1 - errRate, 1) }));
  setSpark('an-kpi-requests', sortedBuckets.map((r) => num(r.requestCount)));

  const ttft = byId('an-kpi-ttft');
  if (g && g.ttftP50 != null) {
    countUp(ttft, Math.round(num(g.ttftP50)), (v) => exact(Math.round(v)));
    ttft?.setAttribute('delta', `p90 ${ms(g.ttftP90)} · p99 ${ms(g.ttftP99)}`);
  } else {
    ttft?.setAttribute('value', '—');
    ttft?.setAttribute('delta', T('no_percentiles'));
  }
  const dec = byId('an-kpi-decode');
  if (g && g.decodeP50 != null) {
    countUp(dec, Math.round(num(g.decodeP50)), (v) => exact(Math.round(v)));
    dec?.setAttribute('delta', `p90 ${exact(Math.round(num(g.decodeP90)))} · p99 ${exact(Math.round(num(g.decodeP99)))}`);
  } else {
    dec?.setAttribute('value', '—');
    dec?.setAttribute('delta', T('no_percentiles'));
  }
  const errCard = byId('an-kpi-errors');
  countUp(errCard, errRate * 100, (v) => v.toLocaleString(lang(), { maximumFractionDigits: 2, minimumFractionDigits: 2 }));
  errCard?.setAttribute('delta', T('errors_sub', { errors: compact(totals.errorCount), count: num(totals.requestCount), requests: compact(totals.requestCount) }));
  const costCard = byId('an-kpi-cost');
  countUp(costCard, num(totals.cost), (v) => exact(Math.round(v)));
  if (costCard) costCard.title = money(totals.cost);
  const costChip = byId('an-kpi-cost-chip');
  if (costChip) {
    costChip.innerHTML = missingModels > 0
      ? tfChip('warn', T('models_without_pricing', { count: missingModels }))
      : tfChip('ok', T('pricing_complete'));
  }

  mountTokenChart('an-chart', buckets.rows);

  const modelTotal = byModel.rows.reduce((s, r) => s + num(r.totalTokens), 0) || 1;
  const topModels = sortByTokens(byModel.rows).slice(0, 5);
  setRows('an-top-models-table', topModels.map((r, i) => ({
    _row: r,
    name: modelCell(r.key, r.displayName, modelSub(r)),
    total: compactCell(r.totalTokens),
    share: shareCell(num(r.totalTokens) / modelTotal, i),
  })));
  setFoot('an-top-models', escapeHtml(T('models_count', { count: byModel.rows.length })), escapeHtml(T('sum_tokens', { value: compact(modelTotal) })));
  onRowClick('an-top-models-table', (row) => openDrill('model', row._row.key, rowName(row._row), rowSub(row._row, 'model')));

  const topUsers = sortByTokens(byUser.rows).slice(0, 5);
  setRows('an-top-users-table', topUsers.map((r) => ({
    _row: r,
    name: entCell({ title: rowName(r), sub: rowSub(r, 'user') }),
    total: compactCell(r.totalTokens),
    cost: `<span class="tf-table__muted">${costCell(r.cost, r.missingPricing)}</span>`,
  })));
  setFoot('an-top-users', escapeHtml(T('users_count', { count: byUser.rows.length })), escapeHtml(T('full_list_tab')) + (topUsers.some((r) => r.missingPricing) ? ` · ${escapeHtml(T('partial_cost_note'))}` : ''));
  onRowClick('an-top-users-table', (row) => openDrill('user', row._row.key, rowName(row._row), rowSub(row._row, 'user')));

  const topNodes = sortByTokens(byNode.rows).slice(0, 5);
  setRows('an-top-nodes-table', topNodes.map((r) => ({
    _row: r,
    // The status chip next to it already states the last contact.
    name: nodeCell(r.key, r.displayName, r.lastSeenAt, false, false),
    total: compactCell(r.totalTokens),
    status: nodeStatusChip(r.lastSeenAt),
  })));
  setFoot('an-top-nodes', escapeHtml(T('nodes_count', { count: byNode.rows.length })), escapeHtml(T('liveness_source')));
  onRowClick('an-top-nodes-table', (row) => openDrill('node', row._row.key, rowName(row._row), shortId(row._row.key)));

  state.exportCsv = () => downloadCsv(`analytics-overview-${effectivePeriodKey()}.csv`,
    [T('col_model'), T('col_prompt'), T('col_completion'), T('col_tokens'), T('col_requests'), T('col_cost')],
    sortByTokens(byModel.rows).map((r) => [rowName(r), r.promptTokens, r.completionTokens, r.totalTokens, r.requestCount, num(r.cost).toFixed(2)]));
}

// ---------------------------------------------------------------------------
// Tab: Users & groups.
// ---------------------------------------------------------------------------

async function renderUsers(panel, seq) {
  const isGroup = state.usersSub === 'group';
  panel.innerHTML = `
    <div class="an-subbar tf-toolbar">
      <tf-segmented id="an-users-sub" value="${escapeAttr(state.usersSub)}" size="md">
        <option value="user">${escapeHtml(T('subtab_users'))}</option>
        <option value="group">${escapeHtml(T('subtab_groups'))}</option>
      </tf-segmented>
      <span class="an-spacer"></span>
      <tf-searchbox id="an-users-search" placeholder="${escapeAttr(T('search_subject'))}" value="${escapeAttr(state.usersSearch)}"></tf-searchbox>
    </div>
    ${cardHtml({ id: 'an-users-card', title: T(isGroup ? 'subtab_groups' : 'subtab_users'), hint: T('hint_click_details'), body: tableHtml('an-users-table', [
      { key: 'name', label: T(isGroup ? 'col_group' : 'col_user'), renderer: 'html' },
      { key: 'prompt', label: T('col_prompt'), renderer: 'html', num: true },
      { key: 'completion', label: T('col_completion'), renderer: 'html', num: true },
      { key: 'total', label: T('col_tokens'), renderer: 'html', num: true },
      { key: 'requests', label: T('col_requests'), renderer: 'html', num: true },
      { key: 'audio', label: T('col_audio'), num: true, lo: true },
      { key: 'cost', label: T('col_cost'), renderer: 'html', num: true },
      { key: 'share', label: T('col_share'), renderer: 'html', lo: true },
    ]), cls: 'an-d1' })}`;

  byId('an-users-sub')?.addEventListener('change', (e) => {
    state.usersSub = e.detail?.value || 'user';
    scheduleReload();
  });
  byId('an-users-search')?.addEventListener('search', (e) => {
    state.usersSearch = e.detail?.value ?? e.detail?.query ?? '';
    paint();
  });

  const data = await fetchSummary(state.usersSub);
  if (stale(seq)) return;
  const rows = sortByTokens(data.rows);
  const total = data.grandTotal ? num(data.grandTotal.totalTokens) : rows.reduce((s, r) => s + num(r.totalTokens), 0);
  const totals = data.grandTotal || sumRows(rows);

  function paint() {
    const q = state.usersSearch.trim().toLowerCase();
    const visible = rows.filter((r) => !q || rowName(r).toLowerCase().includes(q) || String(r.key).toLowerCase().includes(q));
    setRows('an-users-table', visible.map((r, i) => ({
      _row: r,
      name: entCell({ title: rowName(r), sub: rowSub(r, state.usersSub) }),
      prompt: compactCell(r.promptTokens),
      completion: compactCell(r.completionTokens),
      total: compactCell(r.totalTokens, true),
      requests: compactCell(r.requestCount),
      audio: audio(r.audioMs),
      cost: costCell(r.cost, r.missingPricing),
      share: shareCell(total ? num(r.totalTokens) / total : 0, i),
    })), T('no_data'));
    const left = T(isGroup ? 'groups_count' : 'users_count', { count: visible.length });
    const right = `${T('sum_tokens', { value: compact(totals.totalTokens) })} · ${costCell(totals.cost, totals.missingPricing)}`
      + partialHint(totals.missingPricing)
      + (isGroup && data.grandTotal ? ` · ${escapeHtml(T('group_overlap_note'))}` : '');
    setFoot('an-users-card', escapeHtml(left), right);
  }
  paint();
  onRowClick('an-users-table', (row) => openDrill(state.usersSub, row._row.key, rowName(row._row), rowSub(row._row, state.usersSub)));

  state.exportCsv = () => downloadCsv(`analytics-${state.usersSub}-${effectivePeriodKey()}.csv`,
    [T(isGroup ? 'col_group' : 'col_user'), T('col_prompt'), T('col_completion'), T('col_tokens'), T('col_requests'), T('col_audio'), T('col_cost')],
    rows.map((r) => [rowName(r), r.promptTokens, r.completionTokens, r.totalTokens, r.requestCount, Math.round(num(r.audioMs) / 1000), r.missingPricing ? T('missing_pricing') : num(r.cost).toFixed(2)]));
}

// ---------------------------------------------------------------------------
// Tab: Models.
// ---------------------------------------------------------------------------

async function renderModels(panel, seq) {
  panel.innerHTML = `
    ${cardHtml({ id: 'an-models-card', title: T('tab_models'), hint: T('hint_click_model'), body: tableHtml('an-models-table', [
      { key: 'name', label: T('col_model'), renderer: 'html' },
      { key: 'total', label: T('col_tokens'), renderer: 'html', num: true },
      { key: 'requests', label: T('col_requests'), renderer: 'html', num: true },
      { key: 'ttft', label: T('col_ttft'), renderer: 'html', num: true },
      { key: 'decode', label: T('col_decode'), renderer: 'html', num: true, lo: true },
      { key: 'errors', label: T('col_errors'), renderer: 'html', num: true, lo: true },
      { key: 'cost', label: T('col_cost'), renderer: 'html', num: true },
      { key: 'share', label: T('col_share'), renderer: 'html', lo: true },
    ]), cls: 'an-d1' })}
    ${cardHtml({ id: 'an-compare-card', title: T('compare_title'), hint: T('compare_hint'), body: tableHtml('an-compare-table', [
      { key: 'model', label: T('col_model'), renderer: 'html', w: '24%' },
      { key: 'node', label: T('col_node'), renderer: 'html', w: '18%' },
      { key: 'backend', label: T('col_backend'), renderer: 'html', w: '13%' },
      { key: 'requests', label: T('col_requests'), renderer: 'html', num: true, w: '12%' },
      { key: 'ttft', label: T('col_ttft_p50_p99'), num: true, w: '13%' },
      { key: 'decode', label: T('col_decode_p50'), num: true, w: '11%', lo: true },
      { key: 'errors', label: T('col_errors'), renderer: 'html', num: true, w: '9%', lo: true },
    ]), cls: 'an-d3' })}`;

  const [byModel, ns] = await Promise.all([fetchSummary('model'), fetchNodeService()]);
  if (stale(seq)) return;
  mergeModelOptions(byModel.rows);
  const rows = sortByTokens(byModel.rows);
  const total = rows.reduce((s, r) => s + num(r.totalTokens), 0) || 1;
  // Engine per model from the node×service rows (first backend wins).
  const engineOf = (id) => backendLabel(ns.find((x) => x.modelId === id)?.backend || state.models.find((m) => m.value === id)?.engineId || '');
  setRows('an-models-table', rows.map((r, i) => {
    const stt = num(r.audioMs) > 0 || isSttRow({ modelId: r.key, backend: '' });
    return {
      _row: r,
      name: modelCell(r.key, r.displayName, stt ? modelSub(r) : `${r.key} · ${engineOf(r.key)}`),
      total: compactCell(r.totalTokens, true),
      requests: compactCell(r.requestCount),
      ttft: r.ttftP50 != null && !stt ? perfCell(ms(r.ttftP50), `p99 ${ms(r.ttftP99)}`) : perfCell('—', stt ? 'STT' : ''),
      decode: r.decodeP50 != null && !stt ? perfCell(tokS(r.decodeP50), `p90 ${exact(Math.round(num(r.decodeP90)))}`) : perfCell('—', stt ? 'STT' : ''),
      errors: errorsChip(r.errorRate),
      cost: costCell(r.cost, r.missingPricing),
      share: shareCell(num(r.totalTokens) / total, i),
    };
  }));
  setFoot('an-models-card', escapeHtml(T('models_count', { count: rows.length })), escapeHtml(`${T('sum_tokens', { value: compact(total) })} · ${T('percentiles_note')}`) + partialHint(rows.some((r) => r.missingPricing)));
  onRowClick('an-models-table', (row) => openDrill('model', row._row.key, rowName(row._row), rowSub(row._row, 'model')));

  const nodeFilter = state.filters.node;
  const compare = ns
    .filter((r) => !nodeFilter || r.nodeId === nodeFilter)
    .sort((a, b) => String(a.modelId).localeCompare(String(b.modelId)) || num(b.requestCount) - num(a.requestCount));
  setRows('an-compare-table', compare.map((r) => ({
    _row: r,
    model: entCell({ title: r.modelDisplayName || modelName(r.modelId) }),
    node: nodeCell(r.nodeId, r.nodeDisplayName, r.nodeLastSeenAt),
    backend: backendChip(r),
    requests: compactCell(r.requestCount),
    ttft: r.ttftP50 != null && !isSttRow(r) ? `${ms(r.ttftP50)} / ${ms(r.ttftP99)}` : '—',
    decode: r.decodeP50 != null && !isSttRow(r) ? tokS(r.decodeP50) : '—',
    errors: errorCountChip(r.errorCount, r.requestCount),
  })));
  setFoot('an-compare-card', escapeHtml(T('combos_count', { count: compare.length })), escapeHtml(T('compare_foot')));
  onRowClick('an-compare-table', (row) => openDrill('model', row._row.modelId, row._row.modelDisplayName || modelName(row._row.modelId), row._row.modelId));

  state.exportCsv = () => downloadCsv(`analytics-models-${effectivePeriodKey()}.csv`,
    [T('col_model'), T('col_tokens'), T('col_requests'), 'TTFT p50 (ms)', 'TTFT p99 (ms)', 'Decode p50 (tok/s)', T('col_errors'), T('col_cost')],
    rows.map((r) => [rowName(r), r.totalTokens, r.requestCount, r.ttftP50 ?? '', r.ttftP99 ?? '', r.decodeP50 ?? '', num(r.errorRate), r.missingPricing ? T('missing_pricing') : num(r.cost).toFixed(2)]));
}

// ---------------------------------------------------------------------------
// Tab: Nodes & services (one card per node).
// ---------------------------------------------------------------------------

function serviceCols() {
  return [
    { key: 'service', label: T('col_service'), renderer: 'html', w: '24%' },
    { key: 'backend', label: T('col_backend'), renderer: 'html', w: '13%' },
    { key: 'model', label: T('col_model'), renderer: 'html', w: '18%' },
    { key: 'requests', label: T('col_requests'), renderer: 'html', num: true, w: '12%' },
    { key: 'ttft', label: T('col_ttft_p50_p99'), num: true, w: '13%' },
    { key: 'decode', label: T('col_decode_p50'), num: true, w: '11%', lo: true },
    { key: 'errors', label: T('col_errors'), renderer: 'html', num: true, w: '9%', lo: true },
  ];
}

function serviceRows(services) {
  return services.map((s) => ({
    _row: s,
    service: serviceCell(s),
    backend: backendChip(s),
    model: entCell({ title: s.modelDisplayName || modelName(s.modelId) }),
    requests: compactCell(s.requestCount),
    ttft: s.ttftP50 != null && !isSttRow(s) ? `${ms(s.ttftP50)} / ${ms(s.ttftP99)}` : '—',
    decode: s.decodeP50 != null && !isSttRow(s) ? tokS(s.decodeP50) : '—',
    errors: errorCountChip(s.errorCount, s.requestCount),
  }));
}

function groupByNode(rows) {
  const byNode = new Map();
  for (const r of rows) {
    const id = r.nodeId || '';
    if (!byNode.has(id)) byNode.set(id, { nodeId: id, name: r.nodeDisplayName || '', lastSeen: r.nodeLastSeenAt || null, services: [] });
    const n = byNode.get(id);
    if (!n.name && r.nodeDisplayName) n.name = r.nodeDisplayName;
    if (!n.lastSeen && r.nodeLastSeenAt) n.lastSeen = r.nodeLastSeenAt;
    n.services.push(r);
  }
  return [...byNode.values()].map((n) => {
    const agg = n.services.reduce((acc, s) => {
      acc.total += num(s.totalTokens);
      acc.requests += num(s.requestCount);
      acc.errors += num(s.errorCount);
      return acc;
    }, { total: 0, requests: 0, errors: 0 });
    return { ...n, agg, errRate: agg.requests ? agg.errors / agg.requests : 0 };
  }).sort((a, b) => b.agg.total - a.agg.total);
}

function nodeCardHtml(n, idx) {
  const live = isLive(n.lastSeen);
  const tableId = `an-node-svc-${idx}`;
  const chips = [];
  if (n.nodeId && n.nodeId === state.coordinatorId) chips.push(tfChip('accent', T('chip_coordinator')));
  if (!live) chips.push(tfChip('neutral', T('status_offline_since', { rel: relTime(n.lastSeen) })));
  return `<section class="an-card an-node-card an-d${Math.min(6, idx + 1)}" data-node="${escapeAttr(n.nodeId)}">
    <div class="an-c-head an-node-head">
      <span class="an-dot${live ? ' an-dot--live' : ''}"></span>
      <div class="an-node-ident"><div class="an-node-name">${escapeHtml(nodeName(n.nodeId, n.name))}</div><div class="an-node-id" title="${escapeAttr(n.nodeId)}">${escapeHtml(midId(n.nodeId))}</div></div>
      ${chips.length ? `<div class="an-node-chips">${chips.join('')}</div>` : ''}
      <div class="an-node-stats">
        <div class="an-ns"><b title="${escapeAttr(exact(n.agg.total))}">${escapeHtml(compact(n.agg.total))}</b><span>${escapeHtml(T('ns_production'))}</span></div>
        <div class="an-ns"><b title="${escapeAttr(exact(n.agg.requests))}">${escapeHtml(compact(n.agg.requests))}</b><span>${escapeHtml(T('ns_requests'))}</span></div>
        <div class="an-ns"><b>${escapeHtml(pctFixed2(n.errRate))}</b><span>${escapeHtml(T('ns_errors'))}</span></div>
      </div>
    </div>
    ${tableHtml(tableId, serviceCols())}
    <div class="an-tfoot"><span>${escapeHtml(T('services_count', { count: n.services.length }))}</span><span>${escapeHtml(T('last_seen', { rel: live ? T('rel_now') : relTime(n.lastSeen) }))}</span></div>
  </section>`;
}

async function renderNodes(panel, seq) {
  panel.innerHTML = `<div id="an-nodes-list">${loadingHtml()}</div>`;
  let rows = await fetchNodeService();
  if (stale(seq)) return;
  if (state.filters.model) rows = rows.filter((r) => r.modelId === state.filters.model);
  const nodes = groupByNode(rows);
  const list = byId('an-nodes-list');
  if (!list) return;
  if (!nodes.length) {
    list.innerHTML = `<section class="an-card">${emptyHtml()}</section>`;
    return;
  }
  list.innerHTML = nodes.map((n, i) => nodeCardHtml(n, i)).join('');
  nodes.forEach((n, i) => {
    const id = `an-node-svc-${i}`;
    setRows(id, serviceRows(n.services));
    onRowClick(id, (row) => openDrill('model', row._row.modelId, row._row.modelDisplayName || modelName(row._row.modelId), row._row.modelId));
  });
  list.querySelectorAll('.an-node-name').forEach((el) => {
    el.addEventListener('click', () => {
      const card = el.closest('.an-node-card');
      const n = nodes.find((x) => x.nodeId === card?.dataset.node);
      if (n) openDrill('node', n.nodeId, nodeName(n.nodeId, n.name), shortId(n.nodeId));
    });
  });

  state.exportCsv = () => downloadCsv(`analytics-nodes-${effectivePeriodKey()}.csv`,
    [T('col_node'), T('col_service'), T('col_backend'), T('col_model'), T('col_requests'), 'TTFT p50 (ms)', 'TTFT p99 (ms)', 'Decode p50 (tok/s)', T('col_errors')],
    rows.map((r) => [r.nodeDisplayName || r.nodeId, r.serviceKey, r.backend, r.modelDisplayName || r.modelId, r.requestCount, r.ttftP50 ?? '', r.ttftP99 ?? '', r.decodeP50 ?? '', r.errorCount]));
}

// ---------------------------------------------------------------------------
// Drill-down (user / group / model / node) — same tab, breadcrumb back.
// ---------------------------------------------------------------------------

function drillFilter() {
  const d = state.drill;
  if (d.kind === 'user') return { filterUser: d.id };
  if (d.kind === 'group') return { filterGroup: d.id };
  if (d.kind === 'model') return { filterModel: d.id };
  return { filterNode: d.id };
}

// Breakdown dimensions per entity kind: [groupBy, title, hint].
function drillBreakdowns() {
  const k = state.drill.kind;
  if (k === 'user') return [['model', T('bd_model'), T('hint_click_model')], ['node', T('bd_node'), T('hint_where_computed')]];
  if (k === 'group') return [['user', T('bd_user'), T('hint_click_details')], ['model', T('bd_model'), T('hint_click_model')]];
  if (k === 'model') return [['user', T('bd_user'), T('hint_click_details')], ['node', T('bd_node'), T('hint_where_computed')]];
  return [['model', T('bd_model'), T('hint_click_model')], ['service', T('bd_service'), T('hint_services_on_node')]];
}

function breakdownCols(dim) {
  if (dim === 'node') {
    return [
      { key: 'name', label: T('col_node'), renderer: 'html' },
      { key: 'total', label: T('col_tokens'), renderer: 'html', num: true },
      { key: 'ttft', label: T('col_ttft_p50'), num: true },
      { key: 'decode', label: T('col_decode_p50'), num: true, lo: true },
      { key: 'errors', label: T('col_errors'), renderer: 'html', num: true, lo: true },
    ];
  }
  if (dim === 'service') return serviceCols();
  return [
    { key: 'name', label: T(dim === 'user' ? 'col_user' : 'col_model'), renderer: 'html' },
    { key: 'total', label: T('col_tokens'), renderer: 'html', num: true },
    { key: 'requests', label: T('col_requests'), renderer: 'html', num: true },
    { key: 'cost', label: T('col_cost'), renderer: 'html', num: true },
    { key: 'share', label: T('col_share'), renderer: 'html', lo: true },
  ];
}

function breakdownRows(dim, rows, total) {
  if (dim === 'service') return serviceRows(rows);
  return rows.map((r, i) => {
    const base = { _row: r, total: compactCell(r.totalTokens) };
    if (dim === 'node') {
      return {
        ...base,
        name: nodeCell(r.key, r.displayName, r.lastSeenAt),
        ttft: ms(r.ttftP50),
        decode: r.decodeP50 != null ? tokS(r.decodeP50) : '—',
        errors: errorCountChip(r.errorCount, r.requestCount),
      };
    }
    return {
      ...base,
      name: dim === 'model'
        ? modelCell(r.key, r.displayName, modelSub(r))
        : entCell({ title: rowName(r), sub: rowSub(r, 'user') }),
      requests: compactCell(r.requestCount),
      cost: costCell(r.cost, r.missingPricing),
      share: shareCell(total ? num(r.totalTokens) / total : 0, i),
    };
  });
}

// Facet chips of the drilled entity (groups/role, member count, backends and
// modality, coordinator/offline). `services` = node×service rows of the period.
function heroChips(d, row, services = []) {
  const chips = [];
  if (d.kind === 'user') {
    const u = state.users.find((x) => x.id === d.id);
    const groupIds = Array.isArray(u?.groupIds) ? u.groupIds : [];
    for (const gid of groupIds) {
      const g = state.groups.find((x) => x.id === gid);
      if (g) chips.push(tfChip('accent', T('chip_group', { name: g.name || gid })));
    }
    if (u?.role) chips.push(tfChip('neutral', T('chip_role', { role: humanRole(u.role) })));
  } else if (d.kind === 'group') {
    const g = state.groups.find((x) => x.id === d.id);
    const count = row?.memberCount ?? g?.memberCount;
    if (count != null) chips.push(tfChip('accent', T('members_count', { count: num(count) })));
  } else if (d.kind === 'model') {
    const backends = [...new Set(services.filter((s) => s.modelId === d.id).map((s) => backendLabel(s.backend)))];
    for (const b of backends) chips.push(tfChip('accent', b));
    const modality = services.find((s) => s.modelId === d.id)?.modality || state.models.find((m) => m.value === d.id)?.category || '';
    if (modality) chips.push(tfChip('neutral', modality.toUpperCase()));
    if (row?.missingPricing) chips.push(tfChip('warn', T('missing_pricing')));
  } else if (d.kind === 'node') {
    if (d.id === state.coordinatorId) chips.push(tfChip('accent', T('chip_coordinator')));
    const live = isLive(row?.lastSeenAt);
    chips.push(tfChip(live ? 'ok' : 'neutral', T('last_seen', { rel: live ? T('rel_now') : relTime(row?.lastSeenAt) }), true));
  }
  // Partial cost is flagged next to the hero KPI (the big number carries no tilde).
  if (d.kind !== 'model' && row?.missingPricing) chips.push(tfChip('warn', T('partial_cost_note')));
  return chips.join('');
}

// Breakdown cards of a drill: two columns, except for a node where the
// services table (7 columns) needs the whole row to render without clipping.
function bdCardsHtml(dims) {
  const cards = dims.map(([dim, title, hint], i) => cardHtml({ id: `an-bd-${dim}`, title, hint, body: tableHtml(`an-bd-${dim}-table`, breakdownCols(dim)), cls: `an-d${i + 3}` }));
  if (state.drill.kind === 'node') return cards.join('');
  return `<div class="an-grid an-cols-2">${cards.join('')}</div>`;
}

async function renderDrill(panel, seq) {
  const d = state.drill;
  const dims = drillBreakdowns();
  const tabName = T(`tab_${state.tab}`);
  panel.innerHTML = `
    <tf-breadcrumb class="an-crumbs">
      <tf-breadcrumb-item href="#">${escapeHtml(tabName)}</tf-breadcrumb-item>
      <tf-breadcrumb-item current>${escapeHtml(d.name)}</tf-breadcrumb-item>
    </tf-breadcrumb>
    <section class="an-card an-d1"><div class="an-c-body an-hero">
      <div class="an-hero-ident">
        <div class="an-hero-name">${d.kind === 'node' ? '<span class="an-dot" id="an-hero-dot"></span>' : ''}${escapeHtml(d.name)}</div>
        <div class="an-hero-sub">${escapeHtml([d.kind === 'node' ? midId(d.id) : d.sub, d.kind === 'user' || d.kind === 'group' ? shortId(d.id) : ''].filter((s) => s && s !== d.name).join(' · '))}</div>
        <div class="an-hero-meta" id="an-hero-chips"></div>
      </div>
      <div class="an-mini-kpis" id="an-mini-kpis">
        <div class="an-mk"><b id="an-mk-tokens">—</b><span>${escapeHtml(T('mk_tokens'))}</span></div>
        <div class="an-mk"><b id="an-mk-requests">—</b><span>${escapeHtml(T('mk_requests'))}</span></div>
        <div class="an-mk"><b id="an-mk-cost">—</b><span>${escapeHtml(T('mk_cost'))}</span></div>
        <div class="an-mk"><b id="an-mk-errors">—</b><span>${escapeHtml(T('mk_errors'))}</span></div>
      </div>
    </div></section>
    ${cardHtml({ id: 'an-drill-chart-card', title: activityTitle(), headExtra: legendHtml(), body: `<div class="an-c-body"><div id="an-drill-chart">${loadingHtml()}</div></div>`, foot: false, cls: 'an-d2' })}
    ${bdCardsHtml(dims)}
    ${cardHtml({ id: 'an-periods', title: T('last_periods'), hint: T(`period_by_${state.filters.period}`), body: '<div id="an-periods-body"></div>', cls: 'an-d5' })}`;

  panel.querySelector('.an-crumbs')?.addEventListener('click', (e) => {
    const a = e.target.closest('a');
    if (!a) return;
    e.preventDefault();
    closeDrill();
  });

  const ef = drillFilter();
  const periodKeys = [effectivePeriodKey(), shiftPeriodKey(1), shiftPeriodKey(2)];
  const bucketDim = bucketGroupBy();
  const selfDim = d.kind;
  const loads = [
    fetchSummary(selfDim, ef),
    fetchSummary(bucketDim, ef),
    ...dims.map(([dim]) => (dim === 'service' ? fetchNodeService() : fetchSummary(dim, ef))),
    ...periodKeys.slice(1).map((pk) => fetchSummary(selfDim, { ...ef, periodKey: pk }).catch(() => ({ rows: [] }))),
    ...periodKeys.slice(1).map((pk) => fetchSummary(bucketDim, { ...ef, periodKey: pk }).catch(() => ({ rows: [] }))),
  ];
  const res = await Promise.all(loads);
  if (stale(seq)) return;
  const self = res[0];
  const buckets = res[1];
  const bdRes = res.slice(2, 2 + dims.length);
  const prevSelf = res.slice(2 + dims.length, 4 + dims.length);
  const prevBuckets = res.slice(4 + dims.length);

  const selfRow = self.rows.find((r) => String(r.key) === d.id) || self.grandTotal || sumRows(self.rows);
  const serviceIdx = dims.findIndex(([dim]) => dim === 'service');
  const nsRows = serviceIdx >= 0 ? (bdRes[serviceIdx].rows ?? bdRes[serviceIdx]) : (d.kind === 'model' ? await fetchNodeService().catch(() => []) : []);
  if (stale(seq)) return;
  const chipsHost = byId('an-hero-chips');
  if (chipsHost) chipsHost.innerHTML = heroChips(d, selfRow, nsRows);
  const heroDot = byId('an-hero-dot');
  if (heroDot) heroDot.classList.toggle('an-dot--live', isLive(selfRow.lastSeenAt));
  const mk = (id, text, title) => {
    const el = byId(id);
    if (!el) return;
    el.textContent = text;
    if (title) el.title = title;
  };
  mk('an-mk-tokens', compact(selfRow.totalTokens), exact(selfRow.totalTokens));
  mk('an-mk-requests', compact(selfRow.requestCount), exact(selfRow.requestCount));
  mk('an-mk-cost', moneyShort(selfRow.cost), selfRow.missingPricing ? T('missing_pricing_hint') : money(selfRow.cost));
  const er = selfRow.errorRate != null ? num(selfRow.errorRate) : (num(selfRow.requestCount) ? num(selfRow.errorCount) / num(selfRow.requestCount) : 0);
  mk('an-mk-errors', pctFixed2(er));

  mountTokenChart('an-drill-chart', buckets.rows, 210);

  dims.forEach(([dim], i) => {
    let rows = bdRes[i].rows ?? bdRes[i];
    if (dim === 'service') rows = rows.filter((r) => r.nodeId === d.id && (!state.filters.model || r.modelId === state.filters.model));
    else rows = sortByTokens(rows);
    const total = rows.reduce((s, r) => s + num(r.totalTokens), 0);
    const cost = rows.reduce((s, r) => s + num(r.cost), 0);
    const missing = rows.some((r) => r.missingPricing);
    setRows(`an-bd-${dim}-table`, breakdownRows(dim, rows, total));
    const countKey = { model: 'models_count', node: 'nodes_count', user: 'users_count', service: 'services_count' }[dim];
    const right = dim === 'node' ? escapeHtml(T('percentiles_note')) : dim === 'service' ? escapeHtml(T('percentiles_note')) : `${escapeHtml(T('sum_tokens', { value: compact(total) }))} · ${costCell(cost, missing)}${partialHint(missing)}`;
    setFoot(`an-bd-${dim}`, escapeHtml(T(countKey, { count: rows.length })), right);
    onRowClick(`an-bd-${dim}-table`, (row) => {
      const r = row._row;
      if (dim === 'service') openDrill('model', r.modelId, r.modelDisplayName || modelName(r.modelId), r.modelId);
      else openDrill(dim, r.key, rowName(r), rowSub(r, dim));
    });
  });

  const periodRows = periodKeys.map((pk, i) => {
    const src = i === 0 ? self : prevSelf[i - 1];
    const row = (src.rows || []).find((r) => String(r.key) === d.id) || sumRows(src.rows || []);
    const bk = i === 0 ? buckets : prevBuckets[i - 1];
    const trend = zeroFillBuckets(bk.rows || [], pk).map((r) => num(r.totalTokens));
    return { pk, row, trend };
  });
  // The AUDIO column only exists for entities that actually transcribed.
  const hasAudio = periodRows.some((p) => num(p.row.audioMs) > 0);
  const pBody = byId('an-periods-body');
  if (pBody) {
    pBody.innerHTML = tableHtml('an-periods-table', [
      { key: 'period', label: T('col_period'), renderer: 'html', w: '16%' },
      { key: 'prompt', label: T('col_prompt'), renderer: 'html', num: true },
      { key: 'completion', label: T('col_completion'), renderer: 'html', num: true },
      { key: 'total', label: T('col_total'), renderer: 'html', num: true },
      { key: 'requests', label: T('col_requests'), renderer: 'html', num: true },
      ...(hasAudio ? [{ key: 'audio', label: T('col_audio'), num: true, lo: true }] : []),
      { key: 'cost', label: T('col_cost'), renderer: 'html', num: true },
      { key: 'trend', label: T('col_trend'), renderer: 'html', lo: true },
    ]);
  }
  const pTable = byId('an-periods-table');
  if (pTable) {
    setRows('an-periods-table', periodRows.map((p) => ({
      period: `<div class="tf-table__cell-title"><b>${escapeHtml(p.pk.replace('T', ' ') + (state.filters.period === 'hourly' ? ':00' : ''))}</b></div>`,
      prompt: compactCell(p.row.promptTokens),
      completion: compactCell(p.row.completionTokens),
      total: compactCell(p.row.totalTokens, true),
      requests: compactCell(p.row.requestCount),
      audio: audio(p.row.audioMs),
      cost: costCell(p.row.cost, p.row.missingPricing),
      trend: '',
    })));
    // Sparklines are live elements — attach after the html cells are in place.
    const trs = pTable.shadowRoot?.querySelectorAll('tbody tr') || [];
    trs.forEach((tr, i) => {
      const td = tr.querySelector('td:last-child');
      const pts = periodRows[i]?.trend || [];
      if (td) td.replaceChildren(pts.some((v) => v > 0) ? sparkCell(pts) : document.createTextNode('—'));
    });
  }
  setFoot('an-periods', escapeHtml(T('periods_count', { count: periodRows.length })), partialHint(periodRows.some((p) => p.row.missingPricing)).replace(/^ · /, ''));

  state.exportCsv = () => downloadCsv(`analytics-${d.kind}-${d.id.slice(0, 12)}-${effectivePeriodKey()}.csv`,
    [T('col_period'), T('col_prompt'), T('col_completion'), T('col_total'), T('col_requests'), T('col_cost')],
    periodRows.map((p) => [p.pk, p.row.promptTokens, p.row.completionTokens, p.row.totalTokens, p.row.requestCount, num(p.row.cost).toFixed(2)]));
}

// ---------------------------------------------------------------------------
// Tab: Limits (quotas + lease coordinator).
// ---------------------------------------------------------------------------

function quotaStatus(q) {
  const used = num(q.usedTokens ?? q.used_tokens);
  const max = num(q.maxTotalTokens ?? q.max_total_tokens);
  const ratio = max > 0 ? used / max : 0;
  const active = !!(q.isActive ?? q.is_active);
  if (!active) return { ratio, tone: 'ok', chip: chip('neutral', T('quota_disabled')) };
  if (ratio >= 0.95) return { ratio, tone: 'err', chip: chip('err', T('quota_alarm', { pct: pct(ratio, 0) })) };
  if (ratio >= 0.75) return { ratio, tone: 'warn', chip: chip('warn', T('quota_near')) };
  return { ratio, tone: 'ok', chip: chip('ok', T('quota_active')) };
}

function quotaSubjectCell(q) {
  const scope = q.scopeType ?? q.scope_type;
  if (scope === 'org') return `<div class="tf-table__cell-title"><b>${escapeHtml(T('whole_org'))}</b></div>`;
  if (scope === 'model') return '<div class="tf-table__cell-title">—</div>';
  const name = q.subjectDisplayName ?? q.subject_display_name;
  const id = q.subjectId ?? q.subject_id ?? '';
  let sub = q.subjectSubtitle ?? q.subject_subtitle ?? '';
  const members = q.subjectMemberCount ?? q.subject_member_count;
  if (scope === 'group' && members != null) sub = T('members_count', { count: num(members) });
  return entCell({ title: name || subjectLabel(scope, id), sub, mono: scope !== 'group' });
}

function subjectLabel(scopeType, subjectId) {
  if (!subjectId) return '—';
  if (scopeType === 'user') {
    const u = state.users.find((x) => x.id === subjectId);
    return u ? (u.displayName || u.username || u.email || shortId(subjectId)) : shortId(subjectId);
  }
  if (scopeType === 'group') {
    const g = state.groups.find((x) => x.id === subjectId);
    return g ? (g.name || shortId(subjectId)) : shortId(subjectId);
  }
  if (scopeType === 'model') return modelName(subjectId);
  return shortId(subjectId);
}

// Readable scope of a quota: "Marketing · miesięczny · Qwen 3.8 27B AWQ".
function quotaLabel(q) {
  const scope = q.scopeType ?? q.scope_type;
  const period = q.period === 'monthly' ? T('period_monthly_adj') : T('period_daily_adj');
  const model = (q.modelDisplayName ?? q.model_display_name) || ((q.modelId ?? q.model_id) ? modelName(q.modelId ?? q.model_id) : '');
  let who = T(`scope_${scope}`);
  if (scope === 'user' || scope === 'group') who = (q.subjectDisplayName ?? q.subject_display_name) || subjectLabel(scope, q.subjectId ?? q.subject_id);
  else if (scope === 'model') return `${model || modelName(q.subjectId ?? q.subject_id)} · ${period}`;
  return [who, period, model].filter(Boolean).join(' · ');
}

async function renderLimits(panel, seq) {
  panel.innerHTML = `
    ${cardHtml({ id: 'an-quotas-card', title: T('quotas_title'), hint: T('quotas_hint'), body: tableHtml('an-quotas-table', [
      { key: 'scope', label: T('col_scope'), renderer: 'html', w: '10%' },
      { key: 'subject', label: T('col_subject'), renderer: 'html', fill: true, w: '19%' },
      { key: 'model', label: T('col_model'), renderer: 'html', w: '17%' },
      { key: 'period', label: T('col_period'), w: '11%' },
      { key: 'usage', label: T('col_usage_limit'), renderer: 'html', w: '27%' },
      { key: 'status', label: T('col_status'), renderer: 'html', w: '12%' },
    ]), cls: 'an-d1' })}
    ${cardHtml({ id: 'an-coord-card', title: T('coordinator_title'), hint: T('coordinator_hint'), headExtra: '<span id="an-coord-chips" class="an-head-chips"></span>', body: tableHtml('an-leases-table', [
      { key: 'quota', label: T('col_limit'), renderer: 'html', w: '30%' },
      { key: 'node', label: T('col_node'), renderer: 'html', w: '18%' },
      { key: 'period', label: T('col_period'), w: '13%' },
      { key: 'base', label: T('col_base_used'), renderer: 'html', num: true, w: '13%', lo: true },
      { key: 'granted', label: T('col_granted'), renderer: 'html', num: true, w: '13%' },
      { key: 'expires', label: T('col_expires'), num: true, w: '13%' },
    ]), cls: 'an-d3' })}`;

  // Leases expire within seconds — always re-read the coordinator status here.
  state.cache.delete('tokenCoordinatorStatusRequest:{}');
  const [qResp, cResp] = await Promise.all([
    cached('tokenListQuotasRequest'),
    cached('tokenCoordinatorStatusRequest').catch(() => null),
  ]);
  if (stale(seq)) return;
  let quotas = Array.isArray(qResp?.quotas) ? qResp.quotas : [];
  const all = quotas;
  if (state.limitsPeriod) quotas = quotas.filter((q) => q.period === state.limitsPeriod);
  if (state.limitsScope) quotas = quotas.filter((q) => (q.scopeType ?? q.scope_type) === state.limitsScope);
  const byId2 = new Map(all.map((q) => [q.id, q]));

  const qTable = byId('an-quotas-table');
  if (qTable) {
    qTable.rowActions = (row) => {
      const wrap = document.createElement('div');
      wrap.className = 'an-row-actions';
      const edit = document.createElement('tf-button');
      edit.setAttribute('variant', 'ghost');
      edit.setAttribute('size', 'sm');
      edit.setAttribute('icon', 'edit');
      edit.title = T('edit');
      edit.addEventListener('click', () => openQuotaEditor(row._q));
      wrap.appendChild(edit);
      return wrap;
    };
  }
  setRows('an-quotas-table', quotas.map((q, i) => {
    const scope = q.scopeType ?? q.scope_type ?? '';
    const active = !!(q.isActive ?? q.is_active);
    const st = quotaStatus(q);
    const used = num(q.usedTokens ?? q.used_tokens);
    const max = num(q.maxTotalTokens ?? q.max_total_tokens);
    // A model-scoped quota carries the model as its subject.
    const modelId = (q.modelId ?? q.model_id) || (scope === 'model' ? (q.subjectId ?? q.subject_id) : null);
    const dim = active ? '' : ' tf-table__dim';
    return {
      _q: q,
      scope: `<span class="${dim}">${chip(active ? 'accent' : 'neutral', T(`scope_${scope}`))}</span>`,
      subject: `<div class="${dim}">${quotaSubjectCell(q)}</div>`,
      model: `<div class="tf-table__cell-title${dim}"><b>${escapeHtml(modelId ? ((q.modelDisplayName ?? q.model_display_name) || modelName(modelId)) : T('all_models_short'))}</b></div>`,
      period: q.period === 'monthly' ? T('period_monthly_adj') : T('period_daily_adj'),
      usage: `<div class="tf-table__share${dim}"><div class="tf-table__bar tf-table__bar--thick"><i class="tf-table__bar-fill tf-table__bar-fill--${st.tone}" style="width:${(Math.min(1, st.ratio) * 100).toFixed(1)}%;animation-delay:${(i * 0.08).toFixed(2)}s"></i></div><span class="tf-table__share-nums" title="${escapeAttr(`${exact(used)} / ${exact(max)}`)}">${escapeHtml(`${compact(used)} / ${compact(max)} · ${pct(st.ratio, 0)}`)}</span></div>`,
      status: `<span class="${dim}">${st.chip}</span>`,
    };
  }), T('no_quotas'));
  const activeCount = quotas.filter((q) => q.isActive ?? q.is_active).length;
  setFoot('an-quotas-card', escapeHtml(`${T('quotas_count', { count: quotas.length })} · ${T('active_count', { count: activeCount })}`), escapeHtml(T('quotas_foot')));

  const coordId = cResp?.coordinatorNodeId ?? cResp?.coordinator_node_id ?? null;
  const coordName = cResp?.coordinatorDisplayName ?? cResp?.coordinator_display_name ?? null;
  const leases = Array.isArray(cResp?.leases) ? cResp.leases : [];
  const chips = byId('an-coord-chips');
  if (chips) {
    const coordLive = isLive(state.nodes.find((n) => n.value === coordId)?.lastSeen);
    chips.innerHTML = coordId
      ? `${tfChip(coordLive ? 'ok' : 'neutral', nodeName(coordId, coordName), true)}${tfChip('neutral', T('coordinator_params'))}`
      : tfChip('neutral', T('coordinator_none'));
  }
  const nowTs = Date.now();
  const isExpired = (l) => {
    const ts = new Date(l.expiresAt ?? l.expires_at ?? 0).getTime();
    return Number.isFinite(ts) && ts < nowTs;
  };
  setRows('an-leases-table', leases.map((l) => {
    const qid = l.quotaId ?? l.quota_id ?? '';
    const q = byId2.get(qid);
    const nodeId = l.nodeId ?? l.node_id ?? '';
    const expired = isExpired(l);
    const dim = expired ? ' tf-table__dim' : '';
    const expiresAt = l.expiresAt ?? l.expires_at;
    return {
      quota: `<div class="${dim}">${entCell({ title: q ? quotaLabel(q) : T('quota_unknown', { id: String(qid).slice(-6) }), sub: q && (q.scopeType ?? q.scope_type) !== 'org' ? T(`scope_${q.scopeType ?? q.scope_type}`) : '' })}</div>`,
      node: `<div class="${dim}">${nodeCell(nodeId, l.nodeDisplayName ?? l.node_display_name, l.nodeLastSeenAt ?? l.node_last_seen_at, true)}</div>`,
      period: l.periodKey ?? l.period_key ?? '',
      base: `<span class="${dim}">${compactCell(l.baseUsed ?? l.base_used)}</span>`,
      granted: `<span class="${dim}">${compactCell(l.grantedTokens ?? l.granted_tokens)}</span>`,
      expires: expired ? T('lease_expired_rel', { rel: relTime(expiresAt) }) : relTime(expiresAt),
    };
  }), T('no_leases'));
  setFoot('an-coord-card', escapeHtml(T('leases_count', { count: leases.filter((l) => !isExpired(l)).length })), leases.some(isExpired) ? escapeHtml(T('leases_foot')) : '');

  state.exportCsv = () => downloadCsv('analytics-quotas.csv',
    [T('col_scope'), T('col_subject'), T('col_model'), T('col_period'), T('col_used'), T('col_limit'), T('col_status')],
    quotas.map((q) => [q.scopeType ?? q.scope_type, (q.subjectDisplayName ?? q.subject_display_name) || (q.subjectId ?? q.subject_id) || '', (q.modelId ?? q.model_id) || '', q.period, num(q.usedTokens ?? q.used_tokens), num(q.maxTotalTokens ?? q.max_total_tokens), (q.isActive ?? q.is_active) ? T('quota_active') : T('quota_disabled')]));
}

// Quota editor modal (create / edit / delete).
function openQuotaEditor(quota) {
  const isEdit = !!quota;
  const editor = {
    id: quota?.id ?? null,
    scopeType: quota?.scopeType ?? quota?.scope_type ?? 'user',
    subjectId: quota?.subjectId ?? quota?.subject_id ?? '',
    modelId: quota?.modelId ?? quota?.model_id ?? '',
    period: quota?.period ?? 'daily',
    maxTokens: num(quota?.maxTotalTokens ?? quota?.max_total_tokens),
    isActive: quota ? !!(quota.isActive ?? quota.is_active) : true,
  };

  const body = document.createElement('div');
  body.className = 'an-quota-form';
  body.innerHTML = `
    <tf-select id="an-q-scope" label="${escapeAttr(T('field_scope'))}" value="${escapeAttr(editor.scopeType)}">
      <option value="user">${escapeHtml(T('scope_user'))}</option>
      <option value="group">${escapeHtml(T('scope_group'))}</option>
      <option value="model">${escapeHtml(T('scope_model'))}</option>
      <option value="org">${escapeHtml(T('scope_org'))}</option>
    </tf-select>
    <div id="an-q-subject-host"></div>
    <tf-select id="an-q-model" label="${escapeAttr(T('field_model'))}"></tf-select>
    <div class="an-q-period tf-input-group">
      <span class="tf-label">${escapeHtml(T('field_period'))}</span>
      <tf-segmented id="an-q-period" value="${escapeAttr(editor.period)}" size="md">
        <option value="daily">${escapeHtml(T('period_daily_adj_cap'))}</option>
        <option value="monthly">${escapeHtml(T('period_monthly_adj_cap'))}</option>
      </tf-segmented>
    </div>
    <tf-input id="an-q-max" type="text" inputmode="numeric" label="${escapeAttr(T('field_max_tokens'))}" value="${escapeAttr(editor.maxTokens ? exact(editor.maxTokens) : '')}" hint="${escapeAttr(maxTokensHint(editor.maxTokens, quota))}"></tf-input>
    <div class="an-q-active tf-input-group"><span class="tf-label">${escapeHtml(T('field_active'))}</span><tf-toggle id="an-q-active" ${editor.isActive ? 'checked' : ''}></tf-toggle></div>
  `;

  const modal = document.createElement('tf-modal');
  modal.setAttribute('title', T(isEdit ? 'quota_edit_title' : 'quota_new_title'));
  modal.setAttribute('variant', 'modal');
  modal.setAttribute('size', 'md');
  const bodySlot = document.createElement('div');
  bodySlot.setAttribute('slot', 'body');
  bodySlot.appendChild(body);
  modal.appendChild(bodySlot);

  const footer = document.createElement('div');
  footer.setAttribute('slot', 'footer');
  footer.className = 'an-modal-footer';
  if (isEdit) {
    const del = document.createElement('tf-button');
    del.setAttribute('variant', 'danger');
    del.setAttribute('icon', 'trash');
    del.textContent = T('delete');
    del.addEventListener('click', async () => {
      const ok = await TfModal.open({
        title: T('delete_title'),
        body: T('delete_confirm'),
        actions: [{ label: T('cancel'), value: false }, { label: T('delete'), value: true, primary: true }],
      });
      if (!ok) return;
      try {
        await ApiBinary.one('tokenDeleteQuotaRequest', { id: editor.id });
        toast(T('deleted'), 'success');
        closeModal(modal);
        invalidateLimits();
      } catch (err) {
        toast(err?.message || T('delete_failed'), 'error');
      }
    });
    footer.appendChild(del);
  }
  const spacer = document.createElement('span');
  spacer.className = 'an-spacer';
  footer.appendChild(spacer);
  const cancel = document.createElement('tf-button');
  cancel.setAttribute('variant', 'secondary');
  cancel.textContent = T('cancel');
  cancel.addEventListener('click', () => closeModal(modal));
  const save = document.createElement('tf-button');
  save.setAttribute('variant', 'primary');
  save.setAttribute('icon', 'save');
  save.textContent = T('save');
  save.addEventListener('click', () => saveQuota(modal, editor, body));
  footer.append(cancel, save);
  modal.appendChild(footer);

  document.body.appendChild(modal);
  modal.setAttribute('open', '');

  const modelSel = body.querySelector('#an-q-model');
  modelSel?.setOptions([{ value: '', label: T('all_models') }, ...state.models], editor.modelId);
  modelSel?.addEventListener('change', (e) => { editor.modelId = e.detail?.value || ''; });
  body.querySelector('#an-q-scope')?.addEventListener('change', (e) => {
    editor.scopeType = e.detail?.value || 'user';
    editor.subjectId = '';
    renderSubjectControl(body, editor);
  });
  body.querySelector('#an-q-period')?.addEventListener('change', (e) => { editor.period = e.detail?.value || 'daily'; });
  const maxInput = body.querySelector('#an-q-max');
  maxInput?.addEventListener('input', () => maxInput.setAttribute('hint', maxTokensHint(parseTokens(maxInput.value), quota)));
  // Digits are typed freely; the value regroups into thousands on commit.
  maxInput?.addEventListener('change', () => {
    const n = parseTokens(maxInput.value);
    if (n > 0) maxInput.value = exact(n);
  });
  renderSubjectControl(body, editor);
  modal.addEventListener('close', () => closeModal(modal), { once: true });
}

// Digits of a grouped token count ("2 400 000" / "2,400,000") → number.
function parseTokens(text) {
  return Number(String(text || '').replace(/[^\d]/g, '')) || 0;
}

// Live helper under the max-tokens field: "≈ 2,4 mln tok"; an existing quota
// also states its usage in the current period.
function maxTokensHint(v, quota = null) {
  const n = num(v);
  const parts = [n > 0 ? T('approx_tokens', { value: compact(n) }) : T('max_tokens_hint')];
  if (quota) {
    const used = num(quota.usedTokens ?? quota.used_tokens);
    const ratio = n > 0 ? used / n : 0;
    parts.push(T('usage_current_period', { value: compact(used), pct: pct(ratio, 0) }));
  }
  return parts.join(' · ');
}

function renderSubjectControl(body, editor) {
  const host = body.querySelector('#an-q-subject-host');
  if (!host) return;
  if (editor.scopeType === 'org') {
    host.innerHTML = '';
    return;
  }
  let options = [];
  if (editor.scopeType === 'user') {
    options = state.users.map((u) => ({ value: u.id, label: `${u.displayName || u.username || u.email || u.id}${u.email ? ` · ${u.email}` : ''}` }));
  } else if (editor.scopeType === 'group') {
    options = state.groups.map((g) => ({ value: g.id, label: g.name || g.id }));
  } else {
    options = state.models;
  }
  host.innerHTML = `<tf-select id="an-q-subject" label="${escapeAttr(T('field_subject'))}"></tf-select>`;
  const sel = host.querySelector('#an-q-subject');
  sel?.setOptions([{ value: '', label: T('field_subject_hint') }, ...options], editor.subjectId);
  sel?.addEventListener('change', (e) => { editor.subjectId = e.detail?.value || ''; });
}

async function saveQuota(modal, editor, body) {
  const maxTokens = parseTokens(body.querySelector('#an-q-max')?.value);
  const isActive = !!body.querySelector('#an-q-active')?.checked;
  if (editor.scopeType !== 'org' && !editor.subjectId) {
    toast(T('subject_required'), 'error');
    return;
  }
  if (!Number.isFinite(maxTokens) || maxTokens <= 0) {
    toast(T('max_tokens_invalid'), 'error');
    return;
  }
  const quota = {
    id: editor.id,
    scopeType: editor.scopeType,
    subjectId: editor.scopeType === 'org' ? null : editor.subjectId,
    modelId: editor.modelId || null,
    period: editor.period,
    maxTotalTokens: maxTokens,
    isActive,
  };
  try {
    await ApiBinary.one('tokenUpsertQuotaRequest', { quota });
    toast(T('saved'), 'success');
    closeModal(modal);
    invalidateLimits();
  } catch (err) {
    toast(err?.message || T('save_failed'), 'error');
  }
}

function invalidateLimits() {
  for (const key of [...state.cache.keys()]) {
    if (key.startsWith('tokenListQuotasRequest') || key.startsWith('tokenCoordinatorStatusRequest')) state.cache.delete(key);
  }
  if (state.tab === 'limits') renderTab();
}

function closeModal(modal) {
  modal.removeAttribute('open');
  setTimeout(() => modal.remove(), 300);
}

// ---------------------------------------------------------------------------
// Tab: Billing — exact amounts, cost structure, pricing editor.
// ---------------------------------------------------------------------------

async function renderBilling(panel, seq) {
  const isGroup = state.billingBy === 'group';
  panel.innerHTML = `
    <div class="an-grid an-cols-21">
      ${cardHtml({ id: 'an-bill-card', title: T(isGroup ? 'costs_by_group' : 'costs_by_user', { period: periodLabel() }), hint: T('exact_values'), body: tableHtml('an-bill-table', [
        { key: 'name', label: T(isGroup ? 'col_group' : 'col_user'), renderer: 'html' },
        { key: 'total', label: T('col_tokens'), num: true },
        { key: 'requests', label: T('col_requests'), num: true },
        { key: 'cost', label: T('col_cost'), renderer: 'html', num: true },
        { key: 'share', label: T('col_cost_share'), renderer: 'html', lo: true },
      ]), cls: 'an-d1' })}
      ${cardHtml({ id: 'an-struct-card', title: T('cost_structure'), hint: T('by_model_hint'), body: `<div class="an-c-body an-c-body--table">${tableHtml('an-struct-table', [
        { key: 'name', label: T('col_model'), renderer: 'html' },
        { key: 'cost', label: T('col_cost'), renderer: 'html', num: true },
        { key: 'share', label: T('col_share'), renderer: 'html', lo: true },
      ])}</div>`, cls: 'an-d2' })}
    </div>
    ${cardHtml({ id: 'an-pricing-card', title: T('pricing_title'), hint: T('pricing_hint'), body: tableHtml('an-pricing-table', [
      { key: 'model', label: T('col_model'), renderer: 'html' },
      { key: 'prompt', label: T('col_price_prompt'), renderer: 'html', num: true },
      { key: 'completion', label: T('col_price_completion'), renderer: 'html', num: true },
      { key: 'audio', label: T('col_price_audio'), renderer: 'html', num: true },
      { key: 'image', label: T('col_price_image'), renderer: 'html', num: true },
    ]), cls: 'an-d3' })}`;

  const [data, byModel, pricingResp] = await Promise.all([
    fetchSummary(state.billingBy, { filterModel: undefined, filterNode: undefined }),
    fetchSummary('model', { filterModel: undefined, filterNode: undefined }),
    cached('modelMetricsPricingGet').catch(() => null),
  ]);
  if (stale(seq)) return;
  mergeModelOptions(byModel.rows);

  const rows = [...data.rows].sort((a, b) => num(b.cost) - num(a.cost));
  const totals = data.grandTotal || sumRows(rows);
  const totalCost = num(totals.cost) || rows.reduce((s, r) => s + num(r.cost), 0);
  const missingModels = byModel.rows.filter((r) => r.missingPricing).length;
  const note = byId('an-billing-note');
  if (note) {
    note.innerHTML = missingModels > 0
      ? tfChip('warn', T('billing_partial_note', { count: missingModels }))
      : tfChip('ok', T('pricing_complete'));
  }

  setRows('an-bill-table', rows.map((r, i) => ({
    _row: r,
    name: entCell({ title: rowName(r), sub: rowSub(r, state.billingBy) }),
    total: exact(r.totalTokens),
    requests: exact(r.requestCount),
    cost: `<b>${costCell(r.cost, r.missingPricing, true)}</b>`,
    share: shareCell(totalCost ? num(r.cost) / totalCost : 0, i),
  })));
  const sumText = `<b>${escapeHtml(T('sum_cost', { value: money(totalCost) }))}</b>`
    + partialHint(missingModels > 0)
    + (isGroup && data.grandTotal ? ` · ${escapeHtml(T('group_overlap_note'))}` : '');
  setFoot('an-bill-card', escapeHtml(T(isGroup ? 'groups_count' : 'users_count', { count: rows.length })), sumText);
  onRowClick('an-bill-table', (row) => openDrill(state.billingBy, row._row.key, rowName(row._row), rowSub(row._row, state.billingBy)));

  const structRows = [...byModel.rows].sort((a, b) => (a.missingPricing - b.missingPricing) || num(b.cost) - num(a.cost));
  const structTotal = structRows.filter((r) => !r.missingPricing).reduce((s, r) => s + num(r.cost), 0);
  setRows('an-struct-table', structRows.map((r, i) => (r.missingPricing
    ? {
      name: `<div class="tf-table__dim"><div class="tf-table__cell-title"><b>${escapeHtml(rowName(r))}</b></div><div class="tf-table__cell-sub">${escapeHtml(T('missing_pricing'))}</div></div>`,
      cost: `<span class="tf-table__dim" title="${escapeAttr(T('missing_pricing_hint'))}">—</span>`,
      share: '',
    }
    : {
      name: `<div class="tf-table__cell-title"><b>${escapeHtml(rowName(r))}</b></div>`,
      cost: escapeHtml(money(r.cost)),
      share: shareCell(structTotal ? num(r.cost) / structTotal : 0, i),
    })));
  setFoot('an-struct-card', escapeHtml(T('models_count', { count: structRows.length })), escapeHtml(T('sum_cost', { value: money(structTotal) })) + partialHint(structRows.some((r) => r.missingPricing)));

  const pricing = Array.isArray(pricingResp?.rows) ? pricingResp.rows : [];
  renderPricing(pricing, byModel.rows);

  state.exportCsv = () => downloadCsv(`analytics-billing-${state.billingBy}-${effectivePeriodKey()}.csv`,
    [T(isGroup ? 'col_group' : 'col_user'), T('col_tokens'), T('col_requests'), T('col_cost')],
    rows.map((r) => [rowName(r), r.totalTokens, r.requestCount, num(r.cost).toFixed(2)]));
}

function renderPricing(pricing, modelRows) {
  const table = byId('an-pricing-table');
  if (!table) return;
  // Every model seen in the period is editable, even without a pricing row yet.
  const ids = new Map();
  for (const p of pricing) ids.set(p.modelId ?? p.model_id, p);
  for (const r of modelRows) if (!ids.has(String(r.key))) ids.set(String(r.key), null);
  const list = [...ids.entries()];
  // Rates that do not apply to a modality (audio for an LLM, tokens/images for
  // STT) render as a disabled "—" instead of an editable 0,0000.
  const applies = (modelId, field) => {
    const stt = isSttRow({ modelId, backend: '' });
    return stt ? field === 'audio' : field !== 'audio';
  };
  const priceInput = (modelId, field, value) => (applies(modelId, field)
    ? `<span class="tf-table__price tf-table__price--suffix"><tf-input type="number" step="0.0001" min="0" suffix="zł" data-model="${escapeAttr(modelId)}" data-field="${field}" value="${escapeAttr(num(value).toFixed(4))}"></tf-input></span>`
    : `<span class="tf-table__price tf-table__price--suffix"><tf-input type="text" disabled value="—" title="${escapeAttr(T('rate_not_applicable'))}"></tf-input></span>`);
  table.rowActions = (row) => {
    const btn = document.createElement('tf-button');
    btn.setAttribute('variant', row._missing ? 'primary' : 'secondary');
    btn.setAttribute('size', 'sm');
    btn.textContent = T('save');
    btn.addEventListener('click', () => savePricingRow(row._model));
    return btn;
  };
  const missingSet = new Set(modelRows.filter((r) => r.missingPricing).map((r) => String(r.key)));
  setRows('an-pricing-table', list.map(([modelId, p]) => {
    const missing = !p || missingSet.has(modelId);
    const name = modelName(modelId);
    return {
      _model: modelId,
      _missing: missing,
      _class: missing ? 'tf-table__warn-row' : '',
      model: `<div class="tf-table__cell-title"><b>${escapeHtml(name)}</b>${missing ? ` ${chip('warn', T('missing_pricing'))}` : ''}</div>${name !== modelId ? `<div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(modelId)}</div>` : ''}`,
      prompt: priceInput(modelId, 'prompt', p?.promptPer1k ?? p?.prompt_per_1k),
      completion: priceInput(modelId, 'completion', p?.completionPer1k ?? p?.completion_per_1k),
      audio: priceInput(modelId, 'audio', p?.audioPerMin ?? p?.audio_per_min),
      image: priceInput(modelId, 'image', p?.imageEach ?? p?.image_each),
    };
  }), T('no_pricing'));
  setFoot('an-pricing-card', escapeHtml(T('models_in_catalog', { count: list.length })), escapeHtml(T('pricing_formula')));
}

async function savePricingRow(modelId) {
  const table = byId('an-pricing-table');
  if (!table) return;
  const vals = { prompt: 0, completion: 0, audio: 0, image: 0 };
  table.shadowRoot?.querySelectorAll('tf-input[data-model]').forEach((el) => {
    if (el.getAttribute('data-model') !== modelId) return;
    const field = el.getAttribute('data-field');
    if (field in vals) vals[field] = Number(el.value || 0);
  });
  try {
    const resp = await ApiBinary.one('modelMetricsPricingSet', {
      modelId,
      promptPer1k: vals.prompt,
      completionPer1k: vals.completion,
      audioPerMin: vals.audio,
      imageEach: vals.image,
    });
    if (resp && resp.ok === false) {
      toast(resp.error || T('pricing_invalid'), 'error');
      return;
    }
    toast(T('pricing_saved'), 'success');
    // Costs are derived from pricing — every cached summary is stale now.
    state.cache = new Map();
    renderTab();
  } catch (err) {
    toast(err?.message || T('save_failed'), 'error');
  }
}

// ---------------------------------------------------------------------------
// CSV export (client-side).
// ---------------------------------------------------------------------------

function downloadCsv(filename, header, rows) {
  const esc = (v) => {
    const s = String(v ?? '');
    return /[",\n;]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const lines = [header.map(esc).join(';'), ...rows.map((r) => r.map(esc).join(';'))];
  const blob = new Blob([`﻿${lines.join('\n')}`], { type: 'text/csv;charset=utf-8;' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

export default AnalyticsScreen;
