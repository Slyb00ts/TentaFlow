// =============================================================================
// Plik: modules/model-metrics.js
// Opis: Ekran admina analityki metryk modeli (mesh-wide rollup): przegląd,
//       użytkownicy/grupy, modele, nody i serwisy, rozliczenia + cennik.
//       Żywe dane przez binary CBOR protokół (ModelMetricsBody).
// Przykład: Router.register('model-metrics', ModelMetricsScreen)
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-tabs.js';
import '/js/components/tf-table.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-datepicker.js';

let me = null;

// Wspólne filtry wszystkich ekranów. `period` steruje granularnością `periodKey`.
const filters = {
  period: 'monthly',
  periodKey: currentMonth(),
  hour: currentHour(),
  filterModel: '',
  filterNode: '',
};

// Listy dla selectów filtrów (ładowane raz przy montażu).
let modelsList = [];
let nodesList = [];

// Sub-tab ekranu "Użytkownicy i grupy" oraz przełącznik rozliczeń.
let usersSubTab = 'user';
let billingBy = 'user';
let usersSearch = '';

// Bufory danych do eksportu CSV.
let lastUsersRows = [];
let lastBillingRows = [];
let pricingRows = [];

const T = (key, params) => I18n.t(`model_metrics.${key}`, params);

// ---------------------------------------------------------------------------
// Pomocnicze — daty / okresy.
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

function currentHour() {
  return pad2(new Date().getHours());
}

function recentMonths() {
  const out = [];
  const d = new Date();
  for (let i = 0; i < 12; i += 1) {
    out.push(`${d.getFullYear()}-${pad2(d.getMonth() + 1)}`);
    d.setMonth(d.getMonth() - 1);
  }
  return out;
}

// Klucz okresu wysyłany do Core: hourly = YYYY-MM-DDTHH, daily = YYYY-MM-DD,
// monthly = YYYY-MM.
function effectivePeriodKey() {
  if (filters.period === 'hourly') return `${filters.periodKey}T${filters.hour}`;
  return filters.periodKey;
}

function defaultPeriodKey(period) {
  if (period === 'monthly') return currentMonth();
  return todayIso();
}

// ---------------------------------------------------------------------------
// Pomocnicze — formatowanie liczb.
// ---------------------------------------------------------------------------

function lang() { return I18n.getLanguage(); }

function fmtInt(n) {
  return Number(n || 0).toLocaleString(lang());
}

// Zwarty zapis tokenów: M / k dla KPI i mini-tabel.
function fmtTokensCompact(n) {
  const v = Number(n || 0);
  if (Math.abs(v) >= 1e6) return `${(v / 1e6).toLocaleString(lang(), { maximumFractionDigits: 1 })} M`;
  if (Math.abs(v) >= 1e3) return `${(v / 1e3).toLocaleString(lang(), { maximumFractionDigits: 1 })} k`;
  return fmtInt(v);
}

function fmtCost(n) {
  return Number(n || 0).toLocaleString(lang(), { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

// Koszt z uwzględnieniem braku cennika — UI odróżnia "0 zł" od "brak cennika".
function fmtCostCell(cost, missingPricing) {
  if (missingPricing) return `<span class="mm-missing">${escapeHtml(T('missing_pricing'))}</span>`;
  return `${escapeHtml(fmtCost(cost))} ${escapeHtml(T('currency'))}`;
}

// Zagregowany koszt (KPI / total / grandTotal): gdy część modeli w agregacie nie
// ma cennika, kwota jest niepełna — oznacz "~…⚠", żeby nie wyglądała jak "0 zł"
// (darmowe). Zwraca czysty tekst (bez HTML) — do atrybutów i i18n params.
function fmtCostAggregate(cost, missingPricing) {
  const base = `${fmtCost(cost)} ${T('currency')}`;
  return missingPricing ? `~${base} ⚠` : base;
}

function fmtMs(v) {
  if (v == null) return '—';
  return `${Math.round(v).toLocaleString(lang())} ms`;
}

function fmtTokS(v) {
  if (v == null) return '—';
  return Math.round(v).toLocaleString(lang());
}

function fmtPct(fraction) {
  return `${(Number(fraction || 0) * 100).toLocaleString(lang(), { minimumFractionDigits: 1, maximumFractionDigits: 1 })} %`;
}

// ---------------------------------------------------------------------------
// Warstwa protokołu.
// ---------------------------------------------------------------------------

// Filtry model/node wysyłamy TYLKO gdy ekran faktycznie pokazuje daną kontrolkę.
// Inaczej ukryty filtr z innego ekranu cicho zawęża totale (np. grandTotal na
// Rozliczeniach). Domyślnie oba aktywne (Przegląd/Userzy); ekran przekazuje
// `{ model:false }` (Modele) lub `{ model:false, node:false }` (Rozliczenia).
async function fetchSummary(groupBy, { model = true, node = true, extra = {} } = {}) {
  const resp = await ApiBinary.one('modelMetricsSummaryRequest', {
    period: filters.period,
    periodKey: effectivePeriodKey(),
    groupBy,
    filterModel: (model && filters.filterModel) ? filters.filterModel : undefined,
    filterNode: (node && filters.filterNode) ? filters.filterNode : undefined,
    ...extra,
  });
  return {
    rows: Array.isArray(resp?.rows) ? resp.rows : [],
    grandTotal: resp?.grandTotal ?? null,
  };
}

async function fetchNodeService() {
  const resp = await ApiBinary.one('modelMetricsNodeServiceRequest', {
    period: filters.period,
    periodKey: effectivePeriodKey(),
  });
  return Array.isArray(resp?.rows) ? resp.rows : [];
}

// Suma rozłącznych wierszy (dla wymiarów bez grand_total). Percentyli NIE da się
// łączyć z wierszy — zwracamy tylko sumowalne pola.
function sumRows(rows) {
  return rows.reduce((acc, r) => {
    acc.promptTokens += Number(r.promptTokens || 0);
    acc.completionTokens += Number(r.completionTokens || 0);
    acc.totalTokens += Number(r.totalTokens || 0);
    acc.embeddingTokens += Number(r.embeddingTokens || 0);
    acc.audioMs += Number(r.audioMs || 0);
    acc.images += Number(r.images || 0);
    acc.requestCount += Number(r.requestCount || 0);
    acc.errorCount += Number(r.errorCount || 0);
    acc.cost += Number(r.cost || 0);
    acc.missingPricing = acc.missingPricing || !!r.missingPricing;
    return acc;
  }, {
    promptTokens: 0, completionTokens: 0, totalTokens: 0, embeddingTokens: 0,
    audioMs: 0, images: 0, requestCount: 0, errorCount: 0, cost: 0, missingPricing: false,
  });
}

// ---------------------------------------------------------------------------
// Szkielet ekranu.
// ---------------------------------------------------------------------------

const ModelMetricsScreen = {
  get title() { return T('title'); },

  render() {
    return `<div id="mm-root"></div>`;
  },

  async mount() {
    try {
      me = await ApiBinary.one('authMeRequest');
    } catch {
      me = null;
    }
    const root = byId('mm-root');
    if (!root) return;
    if (!me || (me.role !== 'admin' && !me.isAdmin)) {
      root.innerHTML = `<div class="card"><p>${escapeHtml(T('admin_only'))}</p></div>`;
      return;
    }
    root.innerHTML = shellHtml();
    byId('mm-tabs')?.addEventListener('change', (e) => {
      const id = e.detail?.value;
      if (id) switchTab(id);
    });
    await loadFilterSubjects();
    await switchTab('overview');
  },

  unmount() {
    me = null;
    modelsList = [];
    nodesList = [];
    lastUsersRows = [];
    lastBillingRows = [];
    pricingRows = [];
  },
};

function shellHtml() {
  return `
    <div class="page-header">
      <div>
        <h1>${escapeHtml(T('title'))}</h1>
        <div class="sub">${escapeHtml(T('subtitle'))}</div>
      </div>
    </div>
    <tf-tabs variant="solid" value="overview" id="mm-tabs">
      <tf-tab id="overview">${escapeHtml(T('tab_overview'))}</tf-tab>
      <tf-tab id="users">${escapeHtml(T('tab_users'))}</tf-tab>
      <tf-tab id="models">${escapeHtml(T('tab_models'))}</tf-tab>
      <tf-tab id="nodes">${escapeHtml(T('tab_nodes'))}</tf-tab>
      <tf-tab id="billing">${escapeHtml(T('tab_billing'))}</tf-tab>
    </tf-tabs>
    <div id="mm-panel"></div>
  `;
}

async function switchTab(tab) {
  if (tab === 'overview') await renderOverview();
  else if (tab === 'users') await renderUsers();
  else if (tab === 'models') await renderModels();
  else if (tab === 'nodes') await renderNodes();
  else if (tab === 'billing') await renderBilling();
}

async function loadFilterSubjects() {
  try {
    const models = await ApiBinary.list('modelListRequest', { arrayKey: 'models' });
    modelsList = (Array.isArray(models) ? models : [])
      .map((row) => {
        const value = row.model_name || row.modelName || '';
        const label = row.display_name || row.displayName || value;
        return { value, label };
      })
      .filter((o) => o.value);
  } catch {
    modelsList = [];
  }
  try {
    const rows = await fetchNodeService();
    const seen = new Map();
    for (const r of rows) {
      const id = r.nodeId || '';
      if (id && !seen.has(id)) seen.set(id, id);
    }
    nodesList = [...seen.keys()].map((id) => ({ value: id, label: id }));
  } catch {
    nodesList = [];
  }
}

// ---------------------------------------------------------------------------
// Wspólny pasek filtrów (okres + node + model). `opts` steruje widocznością.
// ---------------------------------------------------------------------------

function filterbarHtml({ node = true, model = true } = {}) {
  return `
    <div class="mm-filters">
      <span class="mm-fl">${escapeHtml(T('period'))}</span>
      <tf-segmented id="mm-period" value="${escapeAttr(filters.period)}" size="sm">
        <option value="hourly">${escapeHtml(T('period_hourly'))}</option>
        <option value="daily">${escapeHtml(T('period_daily'))}</option>
        <option value="monthly">${escapeHtml(T('period_monthly'))}</option>
      </tf-segmented>
      <span id="mm-period-key-host">${periodKeyHtml()}</span>
      ${node ? `
        <span class="mm-fl">${escapeHtml(T('node'))}</span>
        <tf-select id="mm-node" value="${escapeAttr(filters.filterNode)}">
          <option value="">${escapeHtml(T('all_nodes'))}</option>
          ${nodesList.map((n) => `<option value="${escapeAttr(n.value)}">${escapeHtml(n.label)}</option>`).join('')}
        </tf-select>` : ''}
      ${model ? `
        <span class="mm-fl">${escapeHtml(T('model'))}</span>
        <tf-select id="mm-model" value="${escapeAttr(filters.filterModel)}">
          <option value="">${escapeHtml(T('all_models'))}</option>
          ${modelsList.map((m) => `<option value="${escapeAttr(m.value)}">${escapeHtml(m.label)}</option>`).join('')}
        </tf-select>` : ''}
      <span class="mm-spacer"></span>
      <tf-button variant="secondary" icon="refresh" id="mm-refresh">${escapeHtml(T('refresh'))}</tf-button>
    </div>
  `;
}

function periodKeyHtml() {
  if (filters.period === 'monthly') {
    const months = recentMonths();
    return `
      <tf-select id="mm-period-key" value="${escapeAttr(filters.periodKey)}">
        ${months.map((mo) => `<option value="${escapeAttr(mo)}">${escapeHtml(mo)}</option>`).join('')}
      </tf-select>`;
  }
  if (filters.period === 'hourly') {
    const hours = Array.from({ length: 24 }, (_, i) => pad2(i));
    return `
      <tf-datepicker id="mm-period-key" value="${escapeAttr(filters.periodKey)}"></tf-datepicker>
      <tf-select id="mm-hour" value="${escapeAttr(filters.hour)}">
        ${hours.map((h) => `<option value="${escapeAttr(h)}">${escapeHtml(h)}:00</option>`).join('')}
      </tf-select>`;
  }
  return `<tf-datepicker id="mm-period-key" value="${escapeAttr(filters.periodKey)}"></tf-datepicker>`;
}

function wireFilterbar(reload) {
  byId('mm-period')?.addEventListener('change', (e) => {
    filters.period = e.detail?.value || 'monthly';
    filters.periodKey = defaultPeriodKey(filters.period);
    const host = byId('mm-period-key-host');
    if (host) {
      host.innerHTML = periodKeyHtml();
      wirePeriodKey();
    }
    reload();
  });
  byId('mm-node')?.addEventListener('change', (e) => {
    filters.filterNode = e.detail?.value || '';
    reload();
  });
  byId('mm-model')?.addEventListener('change', (e) => {
    filters.filterModel = e.detail?.value || '';
    reload();
  });
  byId('mm-refresh')?.addEventListener('click', reload);
  wirePeriodKey(reload);
}

function wirePeriodKey(reload) {
  byId('mm-period-key')?.addEventListener('change', (e) => {
    filters.periodKey = e.detail?.value || filters.periodKey;
    if (reload) reload();
  });
  byId('mm-hour')?.addEventListener('change', (e) => {
    filters.hour = e.detail?.value || filters.hour;
    if (reload) reload();
  });
}

function meshBannerHtml(nodeCount, extra = '') {
  const nodes = nodesList.map((n) => {
    const full = n.label || '';
    // Node id to 64-znakowy hex — skracamy w chipie (pełny w tooltipie), inaczej
    // nierozbijalny ciag przelewa banner poza ekran na mobile.
    const short = full.length > 14 ? `${full.slice(0, 10)}…${full.slice(-4)}` : full;
    return `<tf-chip variant="success" title="${escapeAttr(full)}">${escapeHtml(short)}</tf-chip>`;
  }).join('');
  return `
    <section class="card mm-banner">
      <span>${escapeHtml(T('mesh_note', { count: nodeCount }))}${extra ? ` • ${extra}` : ''}</span>
      <span class="mm-nodes">${nodes}</span>
    </section>`;
}

// ---------------------------------------------------------------------------
// Ekran: Przegląd (m01).
// ---------------------------------------------------------------------------

async function renderOverview() {
  const panel = byId('mm-panel');
  if (!panel) return;
  panel.innerHTML = `
    <section class="card">${filterbarHtml()}</section>
    <div id="mm-ov-banner"></div>
    <div class="mm-kpis" id="mm-kpis"></div>
    <section class="card mm-chart-card">
      <div class="mm-chart-head">
        <div class="mm-chart-title">${escapeHtml(T('chart_tokens_title'))}</div>
        <div class="mm-legend">
          <span class="mm-lg"><span class="mm-sw" style="background:var(--accent-1)"></span>${escapeHtml(T('legend_prompt'))}</span>
          <span class="mm-lg"><span class="mm-sw" style="background:var(--accent-2)"></span>${escapeHtml(T('legend_completion'))}</span>
        </div>
      </div>
      <div id="mm-chart"></div>
    </section>
    <div class="mm-grid-3">
      <section class="card">
        <div class="mm-tbl-title">${escapeHtml(T('top_models'))}</div>
        <tf-table id="mm-top-models">
          <tf-column key="name" label="${escapeAttr(T('col_model'))}" renderer="html"></tf-column>
          <tf-column key="total" label="${escapeAttr(T('col_total_tok'))}" renderer="num"></tf-column>
          <tf-column key="share" label="${escapeAttr(T('col_share'))}" renderer="html"></tf-column>
        </tf-table>
      </section>
      <section class="card">
        <div class="mm-tbl-title">${escapeHtml(T('top_users'))}</div>
        <tf-table id="mm-top-users">
          <tf-column key="name" label="${escapeAttr(T('col_user'))}"></tf-column>
          <tf-column key="total" label="${escapeAttr(T('col_total_tok'))}" renderer="num"></tf-column>
          <tf-column key="cost" label="${escapeAttr(T('col_cost'))}" renderer="html"></tf-column>
        </tf-table>
      </section>
      <section class="card">
        <div class="mm-tbl-title">${escapeHtml(T('top_nodes'))}</div>
        <tf-table id="mm-top-nodes">
          <tf-column key="name" label="${escapeAttr(T('col_node'))}"></tf-column>
          <tf-column key="total" label="${escapeAttr(T('col_production_tok'))}" renderer="num"></tf-column>
        </tf-table>
      </section>
    </div>
  `;
  wireFilterbar(renderOverview);
  await loadOverviewData();
}

async function loadOverviewData() {
  try {
    const [byGroup, byDay, byModel, byUser, byNode] = await Promise.all([
      fetchSummary('group'),
      fetchSummary('day'),
      fetchSummary('model'),
      fetchSummary('user'),
      fetchSummary('node'),
    ]);

    renderOverviewKpis(byGroup, byModel);
    renderTokenChart(byDay.rows);

    const modelTotal = byModel.rows.reduce((s, r) => s + Number(r.totalTokens || 0), 0) || 1;
    const topModels = [...byModel.rows]
      .sort((a, b) => Number(b.totalTokens) - Number(a.totalTokens))
      .slice(0, 5)
      .map((r) => {
        const share = Math.round((Number(r.totalTokens) / modelTotal) * 100);
        return {
          name: `<div class="mm-ent"><span class="mm-nm">${escapeHtml(r.key)}</span></div>`,
          total: Number(r.totalTokens || 0),
          share: `<div class="mm-mono">${share}%</div><div class="mm-share-bar"><span style="width:${share}%"></span></div>`,
        };
      });
    setTableRows('mm-top-models', topModels);

    const topUsers = [...byUser.rows]
      .sort((a, b) => Number(b.totalTokens) - Number(a.totalTokens))
      .slice(0, 5)
      .map((r) => ({
        name: r.key,
        total: Number(r.totalTokens || 0),
        cost: fmtCostCell(r.cost, r.missingPricing),
      }));
    setTableRows('mm-top-users', topUsers);

    const topNodes = [...byNode.rows]
      .sort((a, b) => Number(b.totalTokens) - Number(a.totalTokens))
      .slice(0, 5)
      .map((r) => ({ name: r.key, total: Number(r.totalTokens || 0) }));
    setTableRows('mm-top-nodes', topNodes);

    const banner = byId('mm-ov-banner');
    if (banner) banner.innerHTML = meshBannerHtml(nodesList.length);
  } catch (err) {
    toast(err.message || T('load_failed'), 'error');
  }
}

function renderOverviewKpis(byGroup, byModel) {
  const host = byId('mm-kpis');
  if (!host) return;
  // grand_total (unikalna suma per user) niesie zagregowane percentyle całej org.
  const g = byGroup.grandTotal;
  const totals = g || sumRows(byModel.rows);
  const errRate = g ? Number(g.errorRate || 0)
    : (totals.requestCount ? totals.errorCount / totals.requestCount : 0);

  const cards = [
    { icon: 'database', label: T('kpi_total_tokens'), value: fmtTokensCompact(totals.totalTokens) },
    { icon: 'bar-chart', label: T('kpi_requests'), value: fmtInt(totals.requestCount) },
    {
      icon: 'clock-glance', label: T('kpi_ttft_p50'),
      value: g ? fmtMs(g.ttftP50) : '—',
      delta: g ? `p90 ${fmtMs(g.ttftP90)} · p99 ${fmtMs(g.ttftP99)}` : '',
    },
    {
      icon: 'zap', label: T('kpi_decode_p50'),
      value: g && g.decodeP50 != null ? `${fmtTokS(g.decodeP50)} tok/s` : '—',
      delta: g ? `p90 ${fmtTokS(g.decodeP90)} · p99 ${fmtTokS(g.decodeP99)}` : '',
    },
    {
      icon: 'alert', label: T('kpi_errors'),
      value: fmtPct(errRate),
      delta: `${fmtInt(totals.errorCount)} / ${fmtInt(totals.requestCount)}`,
      deltaType: 'negative',
    },
    {
      icon: 'bolt', label: T('kpi_cost'),
      value: fmtCostAggregate(totals.cost, totals.missingPricing),
      delta: totals.missingPricing ? T('cost_partial_note') : '',
    },
  ];

  host.innerHTML = cards.map((c) => `
    <tf-stat-card
      icon="${escapeAttr(c.icon)}"
      label="${escapeAttr(c.label)}"
      value="${escapeAttr(c.value)}"
      ${c.delta ? `delta="${escapeAttr(c.delta)}"` : ''}
      ${c.deltaType ? `delta-type="${escapeAttr(c.deltaType)}"` : ''}></tf-stat-card>
  `).join('');
}

// Inline SVG — stackowane słupki prompt (accent-1) + completion (accent-2).
function renderTokenChart(dayRows) {
  const host = byId('mm-chart');
  if (!host) return;
  if (!dayRows.length) {
    host.innerHTML = `<div class="mm-empty">${escapeHtml(T('no_data'))}</div>`;
    return;
  }
  const rows = [...dayRows].sort((a, b) => String(a.key).localeCompare(String(b.key)));
  const maxTotal = Math.max(1, ...rows.map((r) => Number(r.totalTokens || 0)));

  const vbW = 960;
  const vbH = 220;
  const top = 10;
  const baseline = 190;
  const usableH = baseline - top;
  const leftPad = 34;
  const rightPad = 6;
  const plotW = vbW - leftPad - rightPad;
  const step = plotW / rows.length;
  const barW = Math.max(2, Math.min(24, step * 0.7));

  // Linie siatki + etykiety osi Y (4 poziomy).
  const gridlines = [];
  for (let i = 0; i <= 4; i += 1) {
    const y = top + (usableH * i) / 4;
    const val = maxTotal * (1 - i / 4);
    gridlines.push(`<line class="mm-gridline" x1="${leftPad}" y1="${y.toFixed(1)}" x2="${vbW - rightPad}" y2="${y.toFixed(1)}"/>`);
    gridlines.push(`<text x="${leftPad - 4}" y="${(y + 3).toFixed(1)}" text-anchor="end">${escapeHtml(fmtTokensCompact(val))}</text>`);
  }

  const bars = rows.map((r, i) => {
    const x = leftPad + step * i + (step - barW) / 2;
    const prompt = Number(r.promptTokens || 0);
    const completion = Number(r.completionTokens || 0);
    const promptH = (prompt / maxTotal) * usableH;
    const complH = (completion / maxTotal) * usableH;
    const promptY = baseline - promptH;
    const complY = promptY - complH;
    return `<rect x="${x.toFixed(1)}" y="${promptY.toFixed(1)}" width="${barW.toFixed(1)}" height="${promptH.toFixed(1)}" fill="var(--accent-1)"><title>${escapeHtml(r.key)}: ${escapeHtml(fmtInt(prompt))} prompt</title></rect>`
      + `<rect x="${x.toFixed(1)}" y="${complY.toFixed(1)}" width="${barW.toFixed(1)}" height="${complH.toFixed(1)}" fill="var(--accent-2)"><title>${escapeHtml(r.key)}: ${escapeHtml(fmtInt(completion))} completion</title></rect>`;
  }).join('');

  // Etykiety osi X — co kilka słupków, żeby nie zlewały się.
  const labelEvery = Math.ceil(rows.length / 8);
  const xLabels = rows.map((r, i) => {
    if (i % labelEvery !== 0 && i !== rows.length - 1) return '';
    const x = leftPad + step * i + step / 2;
    const key = String(r.key);
    const short = key.length > 10 ? key.slice(-5) : key;
    return `<text x="${x.toFixed(1)}" y="${vbH - 4}" text-anchor="middle">${escapeHtml(short)}</text>`;
  }).join('');

  host.innerHTML = `
    <svg class="mm-chart-svg" viewBox="0 0 ${vbW} ${vbH}" preserveAspectRatio="none" role="img">
      <g>${gridlines.join('')}</g>
      <g>${bars}</g>
      <g>${xLabels}</g>
    </svg>`;
}

// ---------------------------------------------------------------------------
// Ekran: Użytkownicy i grupy (m02).
// ---------------------------------------------------------------------------

async function renderUsers() {
  const panel = byId('mm-panel');
  if (!panel) return;
  panel.innerHTML = `
    <section class="card">${filterbarHtml()}</section>
    <div id="mm-users-banner"></div>
    <section class="card">
      <div class="mm-filters">
        <tf-segmented id="mm-users-subtab" value="${escapeAttr(usersSubTab)}" size="sm">
          <option value="user">${escapeHtml(T('subtab_users'))}</option>
          <option value="group">${escapeHtml(T('subtab_groups'))}</option>
        </tf-segmented>
        <span class="mm-spacer"></span>
        <tf-searchbox id="mm-users-search" placeholder="${escapeAttr(T('search_subject'))}" value="${escapeAttr(usersSearch)}"></tf-searchbox>
        <tf-button variant="secondary" icon="download" id="mm-users-export">${escapeHtml(T('export_csv'))}</tf-button>
      </div>
      <div id="mm-users-overlap"></div>
      <tf-table id="mm-users-table">
        <tf-column key="subject" label="${escapeAttr(T('col_subject'))}"></tf-column>
        <tf-column key="prompt" label="${escapeAttr(T('col_prompt_tok'))}" renderer="num" sortable></tf-column>
        <tf-column key="completion" label="${escapeAttr(T('col_completion_tok'))}" renderer="num" sortable></tf-column>
        <tf-column key="total" label="${escapeAttr(T('col_total_tok'))}" renderer="num" sortable></tf-column>
        <tf-column key="requests" label="${escapeAttr(T('col_requests'))}" renderer="num" sortable></tf-column>
        <tf-column key="audio" label="${escapeAttr(T('col_audio_sec'))}" renderer="num" sortable></tf-column>
        <tf-column key="images" label="${escapeAttr(T('col_images'))}" renderer="num" sortable></tf-column>
        <tf-column key="cost" label="${escapeAttr(T('col_cost'))}" renderer="html" sortable></tf-column>
      </tf-table>
    </section>
  `;
  wireFilterbar(loadUsersData);
  byId('mm-users-subtab')?.addEventListener('change', (e) => {
    usersSubTab = e.detail?.value || 'user';
    loadUsersData();
  });
  byId('mm-users-search')?.addEventListener('change', (e) => {
    usersSearch = e.detail?.value || '';
    paintUsersTable();
  });
  byId('mm-users-export')?.addEventListener('click', exportUsersCsv);
  await loadUsersData();
}

async function loadUsersData() {
  try {
    const data = await fetchSummary(usersSubTab);
    lastUsersRows = data.rows;
    const overlap = byId('mm-users-overlap');
    if (overlap) {
      // Grupy mogą się nakładać (user w wielu grupach) → suma wierszy ≠ realny
      // total. grand_total niesie unikalną sumę.
      if (usersSubTab === 'group' && data.grandTotal) {
        overlap.innerHTML = `<div class="mm-note">${escapeHtml(T('group_overlap_note', {
          total: fmtTokensCompact(data.grandTotal.totalTokens),
          cost: fmtCostAggregate(data.grandTotal.cost, data.grandTotal.missingPricing),
        }))}</div>`;
      } else {
        overlap.innerHTML = '';
      }
    }
    paintUsersTable();
    const banner = byId('mm-users-banner');
    if (banner) banner.innerHTML = meshBannerHtml(nodesList.length);
  } catch (err) {
    lastUsersRows = [];
    paintUsersTable();
    toast(err.message || T('load_failed'), 'error');
  }
}

function paintUsersTable() {
  const q = usersSearch.trim().toLowerCase();
  const rows = lastUsersRows
    .filter((r) => !q || String(r.key).toLowerCase().includes(q))
    .map((r) => ({
      subject: r.key,
      prompt: Number(r.promptTokens || 0),
      completion: Number(r.completionTokens || 0),
      total: Number(r.totalTokens || 0),
      requests: Number(r.requestCount || 0),
      audio: Math.round(Number(r.audioMs || 0) / 1000),
      images: Number(r.images || 0),
      cost: fmtCostCell(r.cost, r.missingPricing),
    }));
  setTableRows('mm-users-table', rows);
}

// ---------------------------------------------------------------------------
// Ekran: Modele (m03).
// ---------------------------------------------------------------------------

async function renderModels() {
  const panel = byId('mm-panel');
  if (!panel) return;
  panel.innerHTML = `
    <section class="card">${filterbarHtml({ node: true, model: false })}</section>
    <div id="mm-models-banner"></div>
    <section class="card">
      <div class="mm-tbl-title">${escapeHtml(T('models_table_title'))}</div>
      <tf-table id="mm-models-table">
        <tf-column key="model" label="${escapeAttr(T('col_model'))}"></tf-column>
        <tf-column key="total" label="${escapeAttr(T('col_total_tok'))}" renderer="num" sortable></tf-column>
        <tf-column key="requests" label="${escapeAttr(T('col_requests'))}" renderer="num" sortable></tf-column>
        <tf-column key="decodeP50" label="${escapeAttr(T('col_decode_p50'))}"></tf-column>
        <tf-column key="decodeP90" label="${escapeAttr(T('col_decode_p90'))}"></tf-column>
        <tf-column key="ttftP50" label="${escapeAttr(T('col_ttft_p50'))}"></tf-column>
        <tf-column key="ttftP99" label="${escapeAttr(T('col_ttft_p99'))}"></tf-column>
        <tf-column key="errors" label="${escapeAttr(T('col_errors'))}"></tf-column>
        <tf-column key="cost" label="${escapeAttr(T('col_cost'))}" renderer="html"></tf-column>
      </tf-table>
    </section>
    <section class="card">
      <div class="mm-tbl-title">${escapeHtml(T('models_compare_title'))}</div>
      <div class="mm-note">${escapeHtml(T('models_compare_hint'))}</div>
      <tf-table id="mm-models-compare">
        <tf-column key="model" label="${escapeAttr(T('col_model'))}"></tf-column>
        <tf-column key="node" label="${escapeAttr(T('col_node'))}"></tf-column>
        <tf-column key="backend" label="${escapeAttr(T('col_backend'))}"></tf-column>
        <tf-column key="requests" label="${escapeAttr(T('col_requests'))}" renderer="num" sortable></tf-column>
        <tf-column key="decodeP50" label="${escapeAttr(T('col_decode_p50'))}"></tf-column>
        <tf-column key="ttftP50" label="${escapeAttr(T('col_ttft_p50'))}"></tf-column>
        <tf-column key="ttftP99" label="${escapeAttr(T('col_ttft_p99'))}"></tf-column>
        <tf-column key="errors" label="${escapeAttr(T('col_errors'))}"></tf-column>
      </tf-table>
    </section>
  `;
  wireFilterbar(loadModelsData);
  await loadModelsData();
}

async function loadModelsData() {
  try {
    const [byModel, ns] = await Promise.all([fetchSummary('model', { model: false }), fetchNodeService()]);
    const rows = [...byModel.rows]
      .sort((a, b) => Number(b.totalTokens) - Number(a.totalTokens))
      .map((r) => ({
        model: r.key,
        total: Number(r.totalTokens || 0),
        requests: Number(r.requestCount || 0),
        decodeP50: fmtTokS(r.decodeP50),
        decodeP90: fmtTokS(r.decodeP90),
        ttftP50: fmtMs(r.ttftP50),
        ttftP99: fmtMs(r.ttftP99),
        errors: fmtPct(r.errorRate),
        cost: fmtCostCell(r.cost, r.missingPricing),
      }));
    setTableRows('mm-models-table', rows);

    const nodeFilter = filters.filterNode;
    const compare = ns
      .filter((r) => !nodeFilter || r.nodeId === nodeFilter)
      .sort((a, b) => String(a.modelId).localeCompare(String(b.modelId)) || Number(b.requestCount) - Number(a.requestCount))
      .map((r) => ({
        model: r.modelId,
        node: r.nodeId,
        backend: r.backend,
        requests: Number(r.requestCount || 0),
        decodeP50: fmtTokS(r.decodeP50),
        ttftP50: fmtMs(r.ttftP50),
        ttftP99: fmtMs(r.ttftP99),
        errors: fmtPct(r.errorRate),
      }));
    setTableRows('mm-models-compare', compare);

    const banner = byId('mm-models-banner');
    if (banner) banner.innerHTML = meshBannerHtml(nodesList.length);
  } catch (err) {
    toast(err.message || T('load_failed'), 'error');
  }
}

// ---------------------------------------------------------------------------
// Ekran: Nody i serwisy (m04).
// ---------------------------------------------------------------------------

async function renderNodes() {
  const panel = byId('mm-panel');
  if (!panel) return;
  panel.innerHTML = `
    <section class="card">${filterbarHtml({ node: false, model: true })}</section>
    <div id="mm-nodes-banner"></div>
    <div id="mm-nodes-list"></div>
  `;
  wireFilterbar(loadNodesData);
  await loadNodesData();
}

async function loadNodesData() {
  const list = byId('mm-nodes-list');
  if (!list) return;
  try {
    let rows = await fetchNodeService();
    if (filters.filterModel) rows = rows.filter((r) => r.modelId === filters.filterModel);

    // Grupuj serwisy per node.
    const byNode = new Map();
    for (const r of rows) {
      const id = r.nodeId || '';
      if (!byNode.has(id)) byNode.set(id, []);
      byNode.get(id).push(r);
    }
    if (!byNode.size) {
      list.innerHTML = `<section class="card"><div class="mm-empty">${escapeHtml(T('no_data'))}</div></section>`;
      return;
    }

    const cards = [...byNode.entries()].map(([nodeId, services]) => {
      const agg = services.reduce((acc, s) => {
        acc.total += Number(s.totalTokens || 0);
        acc.requests += Number(s.requestCount || 0);
        acc.errors += Number(s.errorCount || 0);
        return acc;
      }, { total: 0, requests: 0, errors: 0 });
      const errRate = agg.requests ? agg.errors / agg.requests : 0;
      const tableId = `mm-node-svc-${cssId(nodeId)}`;
      return { nodeId, services, agg, errRate, tableId };
    }).sort((a, b) => b.agg.total - a.agg.total);

    list.innerHTML = cards.map((c) => `
      <section class="card mm-node-card">
        <div class="mm-node-head">
          <span class="mm-node-name">${escapeHtml(c.nodeId)}</span>
          <tf-chip variant="info">${escapeHtml(T('services_count', { n: c.services.length }))}</tf-chip>
          <span class="mm-spacer"></span>
          <tf-chip variant="success">${escapeHtml(T('status_online'))}</tf-chip>
        </div>
        <div class="mm-node-kpis">
          <tf-stat-card icon="database" label="${escapeAttr(T('kpi_production'))}" value="${escapeAttr(fmtTokensCompact(c.agg.total))}"></tf-stat-card>
          <tf-stat-card icon="bar-chart" label="${escapeAttr(T('kpi_requests'))}" value="${escapeAttr(fmtInt(c.agg.requests))}"></tf-stat-card>
          <tf-stat-card icon="alert" label="${escapeAttr(T('kpi_errors'))}" value="${escapeAttr(fmtPct(c.errRate))}"></tf-stat-card>
        </div>
        <tf-table id="${escapeAttr(c.tableId)}">
          <tf-column key="service" label="${escapeAttr(T('col_service'))}"></tf-column>
          <tf-column key="backend" label="${escapeAttr(T('col_backend'))}"></tf-column>
          <tf-column key="model" label="${escapeAttr(T('col_model'))}"></tf-column>
          <tf-column key="requests" label="${escapeAttr(T('col_requests'))}" renderer="num"></tf-column>
          <tf-column key="ttftP50" label="${escapeAttr(T('col_ttft_p50'))}"></tf-column>
          <tf-column key="ttftP99" label="${escapeAttr(T('col_ttft_p99'))}"></tf-column>
          <tf-column key="decodeP50" label="${escapeAttr(T('col_decode_p50'))}"></tf-column>
          <tf-column key="errors" label="${escapeAttr(T('col_errors'))}"></tf-column>
        </tf-table>
      </section>
    `).join('');

    for (const c of cards) {
      setTableRows(c.tableId, c.services.map((s) => ({
        service: s.serviceKey,
        backend: s.backend,
        model: s.modelId,
        requests: Number(s.requestCount || 0),
        ttftP50: fmtMs(s.ttftP50),
        ttftP99: fmtMs(s.ttftP99),
        decodeP50: fmtTokS(s.decodeP50),
        errors: fmtPct(s.errorRate),
      })));
    }

    const banner = byId('mm-nodes-banner');
    if (banner) banner.innerHTML = meshBannerHtml(byNode.size);
  } catch (err) {
    list.innerHTML = `<section class="card"><div class="mm-empty">${escapeHtml(T('load_failed'))}</div></section>`;
    toast(err.message || T('load_failed'), 'error');
  }
}

// ---------------------------------------------------------------------------
// Ekran: Rozliczenia (m06).
// ---------------------------------------------------------------------------

async function renderBilling() {
  const panel = byId('mm-panel');
  if (!panel) return;
  panel.innerHTML = `
    <section class="card">${filterbarHtml({ node: false, model: false })}</section>
    <section class="card">
      <div class="mm-filters">
        <tf-segmented id="mm-billing-by" value="${escapeAttr(billingBy)}" size="sm">
          <option value="user">${escapeHtml(T('billing_by_user'))}</option>
          <option value="group">${escapeHtml(T('billing_by_group'))}</option>
        </tf-segmented>
        <span class="mm-spacer"></span>
        <tf-button variant="secondary" icon="download" id="mm-billing-export">${escapeHtml(T('export_csv'))}</tf-button>
      </div>
      <div id="mm-billing-overlap"></div>
      <tf-table id="mm-billing-table">
        <tf-column key="subject" label="${escapeAttr(T('col_subject'))}"></tf-column>
        <tf-column key="total" label="${escapeAttr(T('col_total_tok'))}" renderer="num" sortable></tf-column>
        <tf-column key="requests" label="${escapeAttr(T('col_requests'))}" renderer="num" sortable></tf-column>
        <tf-column key="cost" label="${escapeAttr(T('col_cost'))}" renderer="html" sortable></tf-column>
        <tf-column key="share" label="${escapeAttr(T('col_share'))}" renderer="html"></tf-column>
      </tf-table>
    </section>
    <section class="card">
      <div class="mm-tbl-title">${escapeHtml(T('pricing_title'))}</div>
      <div class="mm-note">${escapeHtml(T('pricing_hint'))}</div>
      <div id="mm-pricing"></div>
    </section>
  `;
  wireFilterbar(loadBillingData);
  byId('mm-billing-by')?.addEventListener('change', (e) => {
    billingBy = e.detail?.value || 'user';
    loadBillingData();
  });
  byId('mm-billing-export')?.addEventListener('click', exportBillingCsv);
  await Promise.all([loadBillingData(), loadPricing()]);
}

async function loadBillingData() {
  try {
    const data = await fetchSummary(billingBy, { model: false, node: false });
    const totalCost = billingBy === 'group' && data.grandTotal
      ? Number(data.grandTotal.cost || 0)
      : data.rows.reduce((s, r) => s + Number(r.cost || 0), 0);
    const denom = totalCost || 1;

    lastBillingRows = [...data.rows].sort((a, b) => Number(b.cost) - Number(a.cost));
    const rows = lastBillingRows.map((r) => {
      const share = (Number(r.cost || 0) / denom) * 100;
      return {
        subject: r.key,
        total: Number(r.totalTokens || 0),
        requests: Number(r.requestCount || 0),
        cost: fmtCostCell(r.cost, r.missingPricing),
        share: `<div class="mm-mono">${share.toFixed(1)}%</div><div class="mm-share-bar"><span style="width:${Math.min(100, share).toFixed(1)}%"></span></div>`,
      };
    });
    setTableRows('mm-billing-table', rows);

    const overlap = byId('mm-billing-overlap');
    if (overlap) {
      overlap.innerHTML = (billingBy === 'group' && data.grandTotal)
        ? `<div class="mm-note">${escapeHtml(T('group_overlap_note', {
          total: fmtTokensCompact(data.grandTotal.totalTokens),
          cost: fmtCostAggregate(data.grandTotal.cost, data.grandTotal.missingPricing),
        }))}</div>`
        : '';
    }
  } catch (err) {
    toast(err.message || T('load_failed'), 'error');
  }
}

async function loadPricing() {
  const host = byId('mm-pricing');
  if (!host) return;
  try {
    const resp = await ApiBinary.one('modelMetricsPricingGet');
    pricingRows = Array.isArray(resp?.rows) ? resp.rows : [];
  } catch (err) {
    pricingRows = [];
    toast(err.message || T('load_failed'), 'error');
  }
  renderPricingEditor(host);
}

function renderPricingEditor(host) {
  if (!pricingRows.length) {
    host.innerHTML = `<div class="mm-empty">${escapeHtml(T('no_pricing'))}</div>`;
    return;
  }
  host.innerHTML = `
    <tf-table id="mm-pricing-table">
      <tf-column key="model" label="${escapeAttr(T('col_model'))}"></tf-column>
      <tf-column key="prompt" label="${escapeAttr(T('col_price_prompt'))}" renderer="html"></tf-column>
      <tf-column key="completion" label="${escapeAttr(T('col_price_completion'))}" renderer="html"></tf-column>
      <tf-column key="audio" label="${escapeAttr(T('col_price_audio'))}" renderer="html"></tf-column>
      <tf-column key="image" label="${escapeAttr(T('col_price_image'))}" renderer="html"></tf-column>
    </tf-table>
  `;
  const table = byId('mm-pricing-table');
  if (table) {
    table.rows = pricingRows.map((r) => ({
      model: r.modelId,
      prompt: priceInput(r.modelId, 'prompt', r.promptPer1k),
      completion: priceInput(r.modelId, 'completion', r.completionPer1k),
      audio: priceInput(r.modelId, 'audio', r.audioPerMin),
      image: priceInput(r.modelId, 'image', r.imageEach),
    }));
    table.rowActions = (row) => pricingRowActions(row);
  }
}

function priceInput(modelId, field, value) {
  return `<tf-input class="mm-price-in mm-mono" type="number" step="0.0001" min="0"
    data-model="${escapeAttr(modelId)}" data-field="${escapeAttr(field)}"
    value="${escapeAttr(String(value ?? 0))}"></tf-input>`;
}

function pricingRowActions(row) {
  const wrap = document.createElement('div');
  const save = document.createElement('tf-button');
  save.setAttribute('variant', 'primary');
  save.setAttribute('size', 'sm');
  save.setAttribute('icon', 'save');
  save.title = T('save');
  save.addEventListener('click', () => savePricingRow(row.model));
  wrap.appendChild(save);
  return wrap;
}

async function savePricingRow(modelId) {
  const table = byId('mm-pricing-table');
  if (!table) return;
  const inputs = table.querySelectorAll(`tf-input[data-model="${cssAttr(modelId)}"]`);
  const vals = { prompt: 0, completion: 0, audio: 0, image: 0 };
  inputs.forEach((el) => {
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
    await Promise.all([loadPricing(), loadBillingData()]);
  } catch (err) {
    toast(err.message || T('save_failed'), 'error');
  }
}

// ---------------------------------------------------------------------------
// CSV eksport (klient-side).
// ---------------------------------------------------------------------------

function exportUsersCsv() {
  const header = [T('col_subject'), T('col_prompt_tok'), T('col_completion_tok'),
    T('col_total_tok'), T('col_requests'), T('col_audio_sec'), T('col_images'), T('col_cost')];
  const rows = lastUsersRows.map((r) => [
    r.key, r.promptTokens, r.completionTokens, r.totalTokens, r.requestCount,
    Math.round(Number(r.audioMs || 0) / 1000), r.images,
    r.missingPricing ? T('missing_pricing') : Number(r.cost || 0).toFixed(2),
  ]);
  downloadCsv(`model-metrics-${usersSubTab}-${effectivePeriodKey()}.csv`, header, rows);
}

function exportBillingCsv() {
  const header = [T('col_subject'), T('col_total_tok'), T('col_requests'), T('col_cost')];
  const rows = lastBillingRows.map((r) => [
    r.key, r.totalTokens, r.requestCount,
    r.missingPricing ? T('missing_pricing') : Number(r.cost || 0).toFixed(2),
  ]);
  downloadCsv(`model-metrics-billing-${billingBy}-${effectivePeriodKey()}.csv`, header, rows);
}

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

// ---------------------------------------------------------------------------
// Pomocnicze DOM.
// ---------------------------------------------------------------------------

function setTableRows(id, rows) {
  const table = byId(id);
  if (table) table.rows = rows;
}

function cssId(v) { return String(v).replace(/[^a-zA-Z0-9_-]/g, '_'); }

function cssAttr(v) { return String(v).replace(/["\\]/g, '\\$&'); }

export default ModelMetricsScreen;
