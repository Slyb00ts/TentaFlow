// =============================================================================
// Plik: modules/benchmark-studio.js
// Opis: Benchmark Studio — benchmarki wydajnosci LLM (jak llama-bench) dla
//       serwisow mesh i zewnetrznych API. Zywe dane przez binarny protokol
//       (BenchmarkBody). Szesc ekranow: lista, kreator (targety + testy),
//       run live (streaming), wyniki, porownanie. Wykresy inline SVG (zero-lib).
// Przyklad: Router.register('benchmark-studio', BenchmarkStudioScreen)
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-table.js';

const T = (key, params) => I18n.t(`benchmark.${key}`, params);

// Paleta serii — deterministycznie przypisywana per etykieta targetu.
const SERIE_COLORS = [
  'var(--serie-0)', 'var(--serie-1)', 'var(--serie-2)',
  'var(--serie-3)', 'var(--serie-4)', 'var(--serie-5)',
];

// Kolejnosc i etykiety scenariuszy (klucz Core → i18n).
const SCENARIO_ORDER = ['latency', 'throughput', 'context', 'sustained'];

// Stan modulu: aktywny widok + robocze dane kreatora / runu.
const state = {
  me: null,
  view: 'list',
  // Kreator.
  draft: null,
  wizardStep: 1,
  meshServices: [],
  // Run live.
  runId: null,
  runBenchmarkId: null,
  runBenchmarkName: '',
  runUnsub: null,
  runTimer: null,
  runPoll: null,
  runStartedMs: 0,
  runLog: [],
  runTargets: [],
  runScenarios: [],
  runStatus: 'running',
  resultsTimeout: null,
  // Wyniki / porownanie.
  resultsRunId: null,
  resultsBenchmarkId: null,
  resultsBenchmarkName: '',
  compareBenchmarkId: null,
};

// ---------------------------------------------------------------------------
// Formatowanie.
// ---------------------------------------------------------------------------

function lang() { return I18n.getLanguage(); }
function fmtInt(n) { return Number(n || 0).toLocaleString(lang()); }

function fmtNum(v, digits = 1) {
  if (v == null || Number.isNaN(v)) return '—';
  return Number(v).toLocaleString(lang(), { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

// mean ± σ (jak llama-bench). Zwraca HTML z wyszarzona sigma.
function fmtStat(mean, sigma, digits = 1) {
  if (mean == null) return '—';
  const m = fmtNum(mean, digits);
  if (sigma == null) return escapeHtml(m);
  return `${escapeHtml(m)} <span class="sigma">±${escapeHtml(fmtNum(sigma, digits))}</span>`;
}

function fmtMsInt(v) {
  if (v == null) return '—';
  return fmtInt(Math.round(v));
}

// Znaczniki z backendu (started_at/finished_at) to SQLite `datetime('now')` —
// naive UTC "YYYY-MM-DD HH:MM:SS" BEZ strefy. Date.parse traktuje taki format
// jak LOCAL, co w strefie != UTC dawalo elapsed przesuniety o offset (np. +2h w
// CEST -> licznik startowal od ~120:00). Normalizujemy: brak jawnej strefy =>
// interpretuj jako UTC (spacja->T, doklej Z).
function parseServerTs(s) {
  if (!s) return NaN;
  const str = String(s).trim();
  if (/[zZ]$|[+-]\d{2}:?\d{2}$/.test(str)) return Date.parse(str);
  return Date.parse(str.replace(' ', 'T') + 'Z');
}

// Skrocony czas trwania z dwoch znacznikow.
function fmtDuration(startIso, endIso) {
  if (!startIso) return '—';
  const start = parseServerTs(startIso);
  const end = endIso ? parseServerTs(endIso) : Date.now();
  if (Number.isNaN(start) || Number.isNaN(end)) return '—';
  const secs = Math.max(0, Math.round((end - start) / 1000));
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return m > 0 ? `${m} min ${s} s` : `${s} s`;
}

function fmtClock(ms) {
  const secs = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

// Skrocona data ISO → YYYY-MM-DD HH:MM (lokalnie).
function fmtDate(iso) {
  if (!iso) return '—';
  const t = parseServerTs(iso);
  if (Number.isNaN(t)) return escapeHtml(iso);
  const d = new Date(t);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function statusPillClass(status) {
  switch ((status || '').toLowerCase()) {
    case 'success': return 'success';
    case 'running': return 'running';
    case 'failed': return 'failed';
    case 'cancelled': return 'cancelled';
    default: return 'idle';
  }
}

function scenarioLabel(scenario) { return T(`scenario_${scenario}`); }

// Przypisanie koloru serii per etykieta (stabilne w ramach zestawu wynikow).
function seriesColorMap(labels) {
  const uniq = [...new Set(labels)];
  const map = new Map();
  uniq.forEach((l, i) => map.set(l, SERIE_COLORS[i % SERIE_COLORS.length]));
  return map;
}

function parseJson(str, fallback) {
  try { return JSON.parse(str); } catch { return fallback; }
}

// ---------------------------------------------------------------------------
// Szkielet ekranu / routing wewnetrzny.
// ---------------------------------------------------------------------------

const BenchmarkStudioScreen = {
  get title() { return T('title'); },

  render() { return `<div id="bench-root" class="bench-root"></div>`; },

  async mount() {
    try { state.me = await ApiBinary.one('authMeRequest'); } catch { state.me = null; }
    const root = byId('bench-root');
    if (!root) return;
    if (!state.me || (state.me.role !== 'admin' && !state.me.isAdmin)) {
      root.innerHTML = `<div class="section-card"><p>${escapeHtml(T('admin_only'))}</p></div>`;
      return;
    }
    await goList();
  },

  unmount() { teardownRun(); Object.assign(state, { view: 'list', draft: null }); },
};

function root() { return byId('bench-root'); }

// Breadcrumb — segmenty {label, onClick?}.
function crumbsHtml(segments) {
  return `<nav class="bench-crumbs">${segments.map((s, i) => {
    const last = i === segments.length - 1;
    const inner = last
      ? `<span class="crumb current">${escapeHtml(s.label)}</span>`
      : `<tf-button variant="ghost" size="sm" class="crumb" data-crumb="${i}">${escapeHtml(s.label)}</tf-button>`;
    const sep = last ? '' : '<span class="sep">›</span>';
    return inner + sep;
  }).join('')}</nav>`;
}

function wireCrumbs(segments) {
  root().querySelectorAll('[data-crumb]').forEach((el) => {
    const seg = segments[Number(el.getAttribute('data-crumb'))];
    if (seg?.onClick) el.addEventListener('click', seg.onClick);
  });
}

// ---------------------------------------------------------------------------
// M1 — Lista benchmarkow.
// ---------------------------------------------------------------------------

async function goList() {
  teardownRun();
  state.view = 'list';
  const el = root();
  el.innerHTML = `
    ${crumbsHtml([{ label: T('crumb_analytics') }, { label: T('title') }])}
    <div class="bench-head-row">
      <div>
        <div class="bench-title">${escapeHtml(T('benchmarks'))}</div>
        <div class="bench-subtitle" id="bench-list-sub"></div>
      </div>
      <tf-button variant="primary" icon="plus" id="bench-new">${escapeHtml(T('new_benchmark'))}</tf-button>
    </div>
    <div class="bench-grid" id="bench-grid"></div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${escapeHtml(T('recent_runs'))}</div>
        <div class="hint">${escapeHtml(T('recent_runs_hint'))}</div>
      </div>
      <tf-table id="bench-recent">
        <tf-column key="run" label="${escapeAttr(T('col_run'))}"></tf-column>
        <tf-column key="benchmark" label="${escapeAttr(T('col_benchmark'))}"></tf-column>
        <tf-column key="started" label="${escapeAttr(T('col_started'))}"></tf-column>
        <tf-column key="duration" label="${escapeAttr(T('col_duration'))}"></tf-column>
        <tf-column key="status" label="${escapeAttr(T('col_status'))}" renderer="html"></tf-column>
      </tf-table>
    </div>
  `;
  byId('bench-new')?.addEventListener('click', () => goWizard(null));
  await Promise.all([loadBenchmarkCards(), loadRecentRuns()]);
}

async function loadBenchmarkCards() {
  const grid = byId('bench-grid');
  if (!grid) return;
  let benchmarks = [];
  try {
    const resp = await ApiBinary.one('benchmarkListRequest');
    benchmarks = Array.isArray(resp?.benchmarks) ? resp.benchmarks : [];
  } catch (err) {
    grid.innerHTML = `<div class="bench-empty">${escapeHtml(err.message || T('load_failed'))}</div>`;
    return;
  }
  const running = benchmarks.filter((b) => b.lastRun?.status === 'running').length;
  const sub = byId('bench-list-sub');
  if (sub) sub.textContent = T('list_summary', { count: benchmarks.length, running });

  if (!benchmarks.length) {
    grid.innerHTML = `<div class="bench-empty">${escapeHtml(T('no_benchmarks'))}</div>`;
    return;
  }

  // Sparkline trendu decode t/s pobieramy z historii runow (leniwie, per karta).
  grid.innerHTML = benchmarks.map((b) => benchmarkCardHtml(b)).join('');
  benchmarks.forEach((b) => {
    const card = grid.querySelector(`[data-bench-id="${cssAttr(b.id)}"]`);
    if (!card) return;
    card.addEventListener('click', () => {
      const run = b.lastRun;
      if (run && run.status === 'running') openRun(run.id, b.id, b.name);
      else if (run) openResults(run.id, b.id, b.name);
      else goWizard(b.id);
    });
    loadSparkline(b.id, card.querySelector('[data-spark]'));
  });
}

function benchmarkCardHtml(b) {
  const run = b.lastRun;
  const pill = run
    ? `<span class="chip ${statusPillClass(run.status)}"><span class="dot"></span>${escapeHtml(run.status)}</span>`
    : `<span class="chip">${escapeHtml(T('never_run'))}</span>`;
  const lastLine = run
    ? T('last_run_line', { date: fmtDate(run.startedAt), dur: fmtDuration(run.startedAt, run.finishedAt) })
    : T('no_runs_yet');
  return `
    <div class="bench-card" data-bench-id="${escapeAttr(b.id)}">
      <div class="b-head">
        <div style="flex:1;">
          <div class="b-name">${escapeHtml(b.name)}</div>
        </div>
        ${pill}
      </div>
      <div class="b-meta">
        <span class="chip">${escapeHtml(T('targets_n', { n: b.targetCount }))}</span>
        <span class="chip">${escapeHtml(T('tests_n', { n: b.testCount }))}</span>
        ${Array.isArray(b.models) && b.models.length ? `<span class="chip info">${escapeHtml(b.models.join(' · '))}</span>` : ''}
      </div>
      <div class="b-last">
        ${escapeHtml(lastLine)}
        <span class="ml-auto trend-label">${escapeHtml(T('decode_trend'))}</span>
        <svg class="sparkline" data-spark viewBox="0 0 90 24" preserveAspectRatio="none"></svg>
      </div>
    </div>
  `;
}

// Sparkline: srednie decode t/s per run (najnowsze runy), chronologicznie.
async function loadSparkline(benchmarkId, svg) {
  if (!svg) return;
  let runs = [];
  try {
    const resp = await ApiBinary.one('benchmarkListRunsRequest', { benchmarkId });
    runs = Array.isArray(resp?.runs) ? resp.runs : [];
  } catch { return; }
  const done = runs.filter((r) => r.status === 'success').slice(0, 6).reverse();
  if (done.length < 2) return;
  const values = [];
  for (const r of done) {
    try {
      const res = await ApiBinary.one('benchmarkRunResultsRequest', { runId: r.id });
      const rows = Array.isArray(res?.results) ? res.results : [];
      const decodes = rows.map((x) => x.decodeTpsMean).filter((v) => v != null);
      values.push(decodes.length ? decodes.reduce((a, b) => a + b, 0) / decodes.length : null);
    } catch { values.push(null); }
  }
  const clean = values.filter((v) => v != null);
  if (clean.length < 2) return;
  const min = Math.min(...clean);
  const max = Math.max(...clean);
  const span = max - min || 1;
  const stepX = 86 / (values.length - 1);
  const pts = values.map((v, i) => {
    const y = v == null ? 22 : 22 - ((v - min) / span) * 20;
    return `${(2 + i * stepX).toFixed(1)},${y.toFixed(1)}`;
  }).join(' ');
  const trend = clean[clean.length - 1] >= clean[0] ? 'var(--success)' : 'var(--danger)';
  svg.innerHTML = `<polyline points="${pts}" stroke="${trend}"/>`;
}

async function loadRecentRuns() {
  const table = byId('bench-recent');
  if (!table) return;
  let runs = [];
  try {
    const resp = await ApiBinary.one('benchmarkRecentRunsRequest');
    runs = Array.isArray(resp?.runs) ? resp.runs : [];
  } catch (err) {
    toast(err.message || T('load_failed'), 'error');
    return;
  }
  table.rows = runs.map((r) => ({
    _run: r,
    run: r.id.slice(0, 8),
    benchmark: r.benchmarkName || '—',
    started: fmtDate(r.startedAt),
    duration: r.status === 'running' ? T('in_progress') : fmtDuration(r.startedAt, r.finishedAt),
    status: `<span class="status-pill ${statusPillClass(r.status)}">${escapeHtml(r.status)}</span>`,
  }));
  table.addEventListener('row-click', (e) => {
    const r = e.detail?.row?._run;
    if (!r) return;
    if (r.status === 'running') openRun(r.id, r.benchmarkId, r.benchmarkName || '');
    else openResults(r.id, r.benchmarkId, r.benchmarkName || '');
  });
}

// ---------------------------------------------------------------------------
// M2/M3 — Kreator (targety + testy).
// ---------------------------------------------------------------------------

function defaultConfig() {
  return {
    prompt_tokens: 512,
    gen_tokens: 128,
    request_timeout_secs: 120,
    latency: { repeats: 5 },
    throughput: { levels: [1, 4, 16, 64], requests_per_worker: 4 },
    context: { prompt_lengths: [128, 2048, 8192, 32768], repeats: 3 },
    sustained: { minutes: 10, concurrency: 8 },
  };
}

async function goWizard(benchmarkId) {
  teardownRun();
  state.view = 'wizard';
  state.wizardStep = 1;

  // Wczytaj serwisy mesh (kategoria LLM) do kolumny targetow. Pomijamy serwisy
  // BEZ endpointu (nie da sie ich zbenchmarkowac) — m.in. headless worker klastra
  // TP, ktory ma te sama nazwe co head, ale nie serwuje inferencji. Bez tego user
  // widzi dwa identyczne wiersze DeepSeek, z ktorych jeden (worker) jest martwy.
  try {
    const services = await ApiBinary.list('serviceListRequest', { arrayKey: 'services' });
    state.meshServices = (Array.isArray(services) ? services : [])
      .filter((s) => (s.category || '').toLowerCase() === 'llm')
      .filter((s) => (s.endpointUrl || s.endpoint_url || '').length > 0);
  } catch { state.meshServices = []; }

  if (benchmarkId) {
    try {
      const resp = await ApiBinary.one('benchmarkGetRequest', { id: benchmarkId });
      const b = resp?.benchmark;
      state.draft = {
        id: b.id,
        name: b.name,
        config: { ...defaultConfig(), ...parseJson(b.configJson, {}) },
        targets: (b.targets || []).map(targetFromWire),
      };
    } catch (err) {
      toast(err.message || T('load_failed'), 'error');
      state.draft = { id: null, name: '', config: defaultConfig(), targets: [] };
    }
  } else {
    state.draft = { id: null, name: '', config: defaultConfig(), targets: [] };
  }
  renderWizard();
}

function targetFromWire(t) {
  return {
    id: t.id,
    kind: t.kind,
    serviceRef: t.serviceRef || null,
    apiType: t.apiType || 'openai',
    host: t.host || '',
    port: Number(t.port || 0),
    model: t.model || '',
    label: t.label || '',
    hasKey: !!t.hasKey,
    apiKey: undefined, // undefined = zachowaj zapisany klucz
  };
}

function renderWizard() {
  if (state.wizardStep === 1) renderWizardTargets();
  else renderWizardTests();
}

function stepperHtml(step) {
  return `
    <div class="stepper">
      <span class="step ${step === 1 ? 'active' : 'done'}"><span class="n">${step === 1 ? '1' : '✓'}</span>${escapeHtml(T('step_targets'))}</span>
      <span class="line"></span>
      <span class="step ${step === 2 ? 'active' : ''}"><span class="n">2</span>${escapeHtml(T('step_tests'))}</span>
    </div>`;
}

// ----- M2: targety -----
function renderWizardTargets() {
  const d = state.draft;
  const crumbs = [
    { label: T('crumb_analytics') },
    { label: T('title'), onClick: goList },
    { label: d.id ? d.name : T('new_benchmark') },
  ];
  const meshRows = state.meshServices.map((s) => meshTargetRowHtml(s)).join('')
    || `<div class="bench-subtitle">${escapeHtml(T('no_mesh_services'))}</div>`;
  const extCards = d.targets.filter((t) => t.kind === 'external').map((t) => extCardHtml(t)).join('');

  root().innerHTML = `
    ${crumbsHtml(crumbs)}
    ${stepperHtml(1)}
    <div class="section-card">
      <h3>${escapeHtml(T('benchmark'))}</h3>
      <label class="bench-subtitle" for="">${escapeHtml(T('name_label'))}</label>
      <div class="section-sub">${escapeHtml(T('name_hint'))}</div>
      <tf-input id="bench-name" value="${escapeAttr(d.name)}" placeholder="${escapeAttr(T('name_ph'))}" style="max-width:440px;"></tf-input>
    </div>
    <div class="grid-2">
      <div class="section-card">
        <h3>${escapeHtml(T('from_services'))}</h3>
        <div class="section-sub">${escapeHtml(T('from_services_hint'))}</div>
        ${meshRows}
      </div>
      <div class="section-card">
        <h3>${escapeHtml(T('external_apis'))}</h3>
        <div class="section-sub">${escapeHtml(T('external_apis_hint'))}</div>
        <div id="bench-ext-list">${extCards}</div>
        <div class="add-ext">
          <div class="section-card-head"><div class="title">${escapeHtml(T('add_external'))}</div></div>
          <label class="bench-subtitle">${escapeHtml(T('api_type'))}</label>
          <tf-select id="ext-api-type" value="openai" style="max-width:280px;">
            <option value="openai">${escapeHtml(T('api_openai'))}</option>
            <option value="anthropic">${escapeHtml(T('api_anthropic'))}</option>
          </tf-select>
          <div class="field-row" style="margin-top:10px; align-items:flex-end;">
            <div style="flex:1;">
              <label class="bench-subtitle">${escapeHtml(T('host_label'))}</label>
              <tf-input id="ext-host" placeholder="${escapeAttr(T('host_ph'))}"></tf-input>
            </div>
            <div style="max-width:110px;">
              <label class="bench-subtitle">${escapeHtml(T('port_label'))}</label>
              <tf-input id="ext-port" placeholder="443"></tf-input>
            </div>
          </div>
          <div style="margin-top:10px;">
            <label class="bench-subtitle">${escapeHtml(T('api_key_optional'))}</label>
            <tf-input id="ext-key" type="password" placeholder="sk-..."></tf-input>
          </div>
          <div style="margin-top:10px;">
            <label class="bench-subtitle">${escapeHtml(T('model_name'))}</label>
            <tf-input id="ext-model" placeholder="${escapeAttr(T('model_ph'))}"></tf-input>
          </div>
          <tf-button variant="secondary" icon="plus" id="ext-add" style="margin-top:12px;">${escapeHtml(T('add_target'))}</tf-button>
        </div>
      </div>
    </div>
    <div class="wizard-foot">
      <tf-button variant="ghost" id="wiz-cancel">${escapeHtml(T('cancel'))}</tf-button>
      <div class="foot-right">
        <span class="text-2 text-sm" id="wiz-count"></span>
        <tf-button variant="primary" id="wiz-next">${escapeHtml(T('next_tests'))}</tf-button>
      </div>
    </div>
  `;
  wireCrumbs(crumbs);

  byId('bench-name')?.addEventListener('input', (e) => { d.name = e.target.value; });
  byId('wiz-cancel')?.addEventListener('click', goList);
  byId('ext-add')?.addEventListener('click', addExternalTarget);
  byId('wiz-next')?.addEventListener('click', () => {
    if (!d.name.trim()) { toast(T('name_required'), 'error'); return; }
    if (!d.targets.length) { toast(T('targets_required'), 'error'); return; }
    state.wizardStep = 2;
    renderWizardTests();
  });

  // Wire checkboxy + selektory modelu serwisow mesh.
  state.meshServices.forEach((s) => {
    const serviceRef = String(s.id);
    const row = root().querySelector(`[data-mesh-id="${cssAttr(serviceRef)}"]`);
    const cb = row?.querySelector('tf-checkbox');
    const sel = row?.querySelector('[data-mesh-model]');
    if (sel) {
      sel.addEventListener('change', (e) => {
        const model = e.detail?.value || sel.value;
        const t = state.draft.targets.find((tt) => tt.kind === 'service' && tt.serviceRef === serviceRef);
        if (t) { t.model = model; t.label = model || s.displayName; }
      });
    }
    if (!cb) return;
    cb.addEventListener('change', (e) => {
      toggleMeshTarget(s, e.detail?.checked);
      row.classList.toggle('checked', !!e.detail?.checked);
      updateWizCount();
    });
  });
  wireExtRemovers();
  updateWizCount();
}

// Served model names z serwisu (ServiceModelEntry.model_name → modelName po
// decode). To jest OpenAI `model` dla `/v1/chat/completions` — dla vLLM MUSI
// byc alias serwowany przez serwis, NIE engineId (typu `vllm`).
function serviceModelNames(s) {
  const models = Array.isArray(s.models) ? s.models : [];
  return models.map((m) => m.modelName).filter(Boolean);
}

// Domyslny serwowany model: oznaczony `isDefault`, inaczej pierwszy z listy.
function defaultServiceModel(s) {
  const models = Array.isArray(s.models) ? s.models : [];
  const def = models.find((m) => m.isDefault && m.modelName);
  return (def || models.find((m) => m.modelName))?.modelName || '';
}

// Aktualnie wybrany model serwisu z selektora (gdy serwis serwuje >1 model).
function selectedMeshModel(s) {
  const sel = root().querySelector(`[data-mesh-model="${cssAttr(String(s.id))}"]`);
  return sel?.value || '';
}

function meshTargetRowHtml(s) {
  const serviceRef = String(s.id);
  const existing = state.draft.targets.find((t) => t.kind === 'service' && t.serviceRef === serviceRef);
  const checked = !!existing;
  const online = (s.status || '').toLowerCase() === 'running' || (s.status || '').toLowerCase() === 'online';
  const modelNames = serviceModelNames(s);
  const endpoint = s.endpointUrl || '';
  const disabled = !endpoint;
  const name = s.displayName || s.engineId || serviceRef;
  const sub = [s.nodeId, s.deployMethod].filter(Boolean).join(' · ');
  const selectedModel = existing?.model || defaultServiceModel(s) || modelNames[0] || '';
  const modelBlock = modelNames.length > 1
    ? `<div class="t-model-row"><label class="bench-subtitle">${escapeHtml(T('served_model'))}</label>
         <tf-select data-mesh-model="${escapeAttr(serviceRef)}" value="${escapeAttr(selectedModel)}" style="max-width:260px;">
           ${modelNames.map((m) => `<option value="${escapeAttr(m)}" ${m === selectedModel ? 'selected' : ''}>${escapeHtml(m)}</option>`).join('')}
         </tf-select></div>`
    : (selectedModel ? `<div class="t-model">${escapeHtml(selectedModel)}</div>` : '');
  const warn = disabled ? `<div class="t-warn">${escapeHtml(T('no_endpoint'))}</div>` : '';
  return `
    <div class="target-option ${checked ? 'checked' : ''} ${disabled ? 'disabled' : ''}" data-mesh-id="${escapeAttr(serviceRef)}">
      <tf-checkbox ${checked ? 'checked' : ''} ${disabled ? 'disabled' : ''}></tf-checkbox>
      <div style="flex:1;">
        <div class="t-name">${escapeHtml(name)}</div>
        <div class="t-sub">${escapeHtml(sub)}</div>
        ${modelBlock}
        ${warn}
      </div>
      <span class="status-pill ${online ? 'online' : 'offline'}">${escapeHtml(online ? T('online') : T('offline'))}</span>
    </div>`;
}

function toggleMeshTarget(s, checked) {
  const d = state.draft;
  const serviceRef = String(s.id);
  if (checked) {
    if (d.targets.some((t) => t.kind === 'service' && t.serviceRef === serviceRef)) return;
    const endpoint = s.endpointUrl || '';
    if (!endpoint) { toast(T('no_endpoint'), 'error'); return; }
    const model = selectedMeshModel(s) || defaultServiceModel(s) || serviceModelNames(s)[0] || s.displayName || serviceRef;
    d.targets.push({
      id: crypto.randomUUID(),
      kind: 'service',
      serviceRef,
      apiType: 'openai',
      // Backend base_url bierze pelny endpoint verbatim (host = endpointUrl).
      host: endpoint,
      port: 0,
      model,
      label: model || s.displayName,
      hasKey: false,
      apiKey: undefined,
    });
  } else {
    d.targets = d.targets.filter((t) => !(t.kind === 'service' && t.serviceRef === serviceRef));
  }
}

function extCardHtml(t) {
  const keyNote = t.hasKey ? '•••••••' : T('no_key');
  return `
    <div class="ext-card" data-ext-id="${escapeAttr(t.id)}">
      <div style="flex:1;">
        <div class="t-name">${escapeHtml(t.model || t.label)}</div>
        <div class="t-sub">${escapeHtml(t.apiType)} · ${escapeHtml(t.host)}${(t.port && !t.host.includes('://')) ? ':' + t.port : ''} · ${escapeHtml(T('key'))}: ${escapeHtml(keyNote)}</div>
      </div>
      <span class="chip info">${escapeHtml(T('external'))}</span>
      <tf-button variant="ghost" size="sm" icon="trash" data-ext-remove="${escapeAttr(t.id)}"></tf-button>
    </div>`;
}

function addExternalTarget() {
  const apiType = byId('ext-api-type')?.value || 'openai';
  const host = (byId('ext-host')?.value || '').trim();
  const portRaw = (byId('ext-port')?.value || '').trim();
  const key = (byId('ext-key')?.value || '').trim();
  const model = (byId('ext-model')?.value || '').trim();
  if (!host) { toast(T('host_required'), 'error'); return; }
  if (!model) { toast(T('model_required'), 'error'); return; }
  // Host z pelnym schematem (http://…:port) niesie port w URL; backend base_url()
  // bierze go verbatim, wiec osobne pole portu jest nadmiarowe → wymus 0.
  const port = host.includes('://') ? 0 : (Number(portRaw) || 443);
  state.draft.targets.push({
    id: crypto.randomUUID(),
    kind: 'external',
    serviceRef: null,
    apiType,
    host,
    port,
    model,
    label: model,
    hasKey: !!key,
    apiKey: key || '',
  });
  // Odswiez liste zewnetrznych + pola.
  const list = byId('bench-ext-list');
  if (list) list.innerHTML = state.draft.targets.filter((t) => t.kind === 'external').map((t) => extCardHtml(t)).join('');
  ['ext-host', 'ext-port', 'ext-key', 'ext-model'].forEach((id) => { const el = byId(id); if (el) el.value = ''; });
  wireExtRemovers();
  updateWizCount();
}

function wireExtRemovers() {
  root().querySelectorAll('[data-ext-remove]').forEach((el) => {
    el.addEventListener('click', () => {
      const id = el.getAttribute('data-ext-remove');
      state.draft.targets = state.draft.targets.filter((t) => t.id !== id);
      const list = byId('bench-ext-list');
      if (list) list.innerHTML = state.draft.targets.filter((t) => t.kind === 'external').map((t) => extCardHtml(t)).join('');
      wireExtRemovers();
      updateWizCount();
    });
  });
}

function updateWizCount() {
  const d = state.draft;
  const mesh = d.targets.filter((t) => t.kind === 'service').length;
  const ext = d.targets.filter((t) => t.kind === 'external').length;
  const el = byId('wiz-count');
  if (el) el.textContent = T('selected_targets', { total: d.targets.length, mesh, ext });
}

// ----- M3: testy -----
function renderWizardTests() {
  const d = state.draft;
  const cfg = d.config;
  const crumbs = [
    { label: T('crumb_analytics') },
    { label: T('title'), onClick: goList },
    { label: d.name || T('new_benchmark') },
  ];
  root().innerHTML = `
    ${crumbsHtml(crumbs)}
    ${stepperHtml(2)}
    ${testCardHtml('latency', 'bolt', !!cfg.latency, [
      ['prompt_tokens', T('prompt_tokens'), cfg.prompt_tokens],
      ['gen_tokens', T('gen_tokens'), cfg.gen_tokens],
      ['latency_repeats', T('repeats'), cfg.latency?.repeats ?? 5],
    ], T('per_target_est', { n: 2 }))}
    ${testCardHtml('throughput', 'bar-chart', !!cfg.throughput, [
      ['throughput_levels', T('concurrency_levels'), (cfg.throughput?.levels || [1, 4, 16, 64]).join(', '), 'wide'],
      ['throughput_rpw', T('requests_per_worker'), cfg.throughput?.requests_per_worker ?? 4],
    ], T('per_target_est', { n: 6 }))}
    ${testCardHtml('context', 'zap', !!cfg.context, [
      ['context_lengths', T('context_lengths'), (cfg.context?.prompt_lengths || [128, 2048, 8192, 32768]).join(', '), 'wide'],
      ['context_repeats', T('repeats'), cfg.context?.repeats ?? 3],
    ], T('per_target_est', { n: 4 }))}
    ${testCardHtml('sustained', 'clock', !!cfg.sustained, [
      ['sustained_minutes', T('duration_min'), cfg.sustained?.minutes ?? 10],
      ['sustained_concurrency', T('concurrency'), cfg.sustained?.concurrency ?? 8],
    ], T('minutes_target', { n: cfg.sustained?.minutes ?? 10 }))}
    <div class="summary-bar">
      <svg class="icon summary-icon"><use href="#i-clock-glance"/></svg>
      <span id="bench-summary"></span>
      <span class="summary-note">${escapeHtml(T('estimate_note'))}</span>
    </div>
    <div class="wizard-foot">
      <tf-button variant="ghost" id="wiz-back">${escapeHtml(T('back_targets'))}</tf-button>
      <div class="foot-right">
        <tf-button variant="secondary" id="wiz-save">${escapeHtml(T('save_no_run'))}</tf-button>
        <tf-button variant="primary" icon="play" id="wiz-run">${escapeHtml(T('run_benchmark'))}</tf-button>
      </div>
    </div>
  `;
  wireCrumbs(crumbs);

  byId('wiz-back')?.addEventListener('click', () => { state.wizardStep = 1; renderWizardTargets(); });
  byId('wiz-save')?.addEventListener('click', () => saveBenchmark(false));
  byId('wiz-run')?.addEventListener('click', () => saveBenchmark(true));

  SCENARIO_ORDER.forEach((scn) => {
    const card = root().querySelector(`[data-test="${scn}"]`);
    const toggle = card?.querySelector('tf-toggle');
    toggle?.addEventListener('change', (e) => {
      applyScenarioToggle(scn, !!e.detail?.checked);
      card.classList.toggle('on', !!e.detail?.checked);
      card.classList.toggle('off', !e.detail?.checked);
      const params = card.querySelector('.tc-params');
      if (params) params.style.display = e.detail?.checked ? '' : 'none';
      updateSummary();
    });
    card?.querySelectorAll('tf-input[data-param]').forEach((el) => {
      el.addEventListener('input', () => { readParams(); updateSummary(); });
    });
  });
  updateSummary();
}

function testCardHtml(scenario, icon, on, params, estimate) {
  const paramsHtml = params.map(([key, label, value, cls]) => `
    <div class="tc-param">
      <label>${escapeHtml(label)}</label>
      <tf-input class="${cls === 'wide' ? 'wide' : ''}" data-param="${escapeAttr(key)}" value="${escapeAttr(String(value))}"></tf-input>
    </div>`).join('');
  return `
    <div class="test-card ${on ? 'on' : 'off'}" data-test="${escapeAttr(scenario)}">
      <div class="tc-head">
        <tf-toggle ${on ? 'checked' : ''}></tf-toggle>
        <div style="flex:1;">
          <div class="tc-name"><svg class="icon tc-icon"><use href="#i-${escapeAttr(icon)}"/></svg>${escapeHtml(T(`scenario_${scenario}_name`))}</div>
          <div class="tc-hint">${escapeHtml(T(`scenario_${scenario}_hint`))}</div>
        </div>
        <span class="chip">${escapeHtml(estimate)}</span>
      </div>
      <div class="tc-params" style="${on ? '' : 'display:none;'}">${paramsHtml}</div>
    </div>`;
}

// Wlacz/wylacz scenariusz — ustawia/kasuje blok w configu (domyslne wartosci).
function applyScenarioToggle(scenario, on) {
  const cfg = state.draft.config;
  if (!on) { cfg[scenario] = null; return; }
  if (scenario === 'latency') cfg.latency = { repeats: 5 };
  if (scenario === 'throughput') cfg.throughput = { levels: [1, 4, 16, 64], requests_per_worker: 4 };
  if (scenario === 'context') cfg.context = { prompt_lengths: [128, 2048, 8192, 32768], repeats: 3 };
  if (scenario === 'sustained') cfg.sustained = { minutes: 10, concurrency: 8 };
  readParams();
}

// Zbierz wartosci z pol do configu.
function readParams() {
  const cfg = state.draft.config;
  const get = (key) => root().querySelector(`tf-input[data-param="${key}"]`)?.value ?? '';
  const intOf = (key, dflt) => { const n = parseInt(get(key), 10); return Number.isFinite(n) && n > 0 ? n : dflt; };
  const listOf = (key, dflt) => {
    const arr = String(get(key)).split(',').map((x) => parseInt(x.trim(), 10)).filter((n) => Number.isFinite(n) && n > 0);
    return arr.length ? arr : dflt;
  };
  cfg.prompt_tokens = intOf('prompt_tokens', cfg.prompt_tokens || 512);
  cfg.gen_tokens = intOf('gen_tokens', cfg.gen_tokens || 128);
  if (cfg.latency) cfg.latency.repeats = intOf('latency_repeats', 5);
  if (cfg.throughput) {
    cfg.throughput.levels = listOf('throughput_levels', [1, 4, 16, 64]);
    cfg.throughput.requests_per_worker = intOf('throughput_rpw', 4);
  }
  if (cfg.context) {
    cfg.context.prompt_lengths = listOf('context_lengths', [128, 2048, 8192, 32768]);
    cfg.context.repeats = intOf('context_repeats', 3);
  }
  if (cfg.sustained) {
    cfg.sustained.minutes = intOf('sustained_minutes', 10);
    cfg.sustained.concurrency = intOf('sustained_concurrency', 8);
  }
}

function estimateMinutes() {
  const cfg = state.draft.config;
  let m = 0;
  if (cfg.latency) m += 2;
  if (cfg.throughput) m += 6;
  if (cfg.context) m += 4;
  if (cfg.sustained) m += (cfg.sustained.minutes || 10);
  return m;
}

function updateSummary() {
  const d = state.draft;
  const tests = SCENARIO_ORDER.filter((s) => d.config[s]).length;
  const el = byId('bench-summary');
  if (el) el.innerHTML = `<span class="big">${escapeHtml(T('summary_line', { targets: d.targets.length, tests }))}</span> · ${escapeHtml(T('estimate', { min: estimateMinutes() }))}`;
}

// Zapis (upsert) + opcjonalny start runu.
async function saveBenchmark(thenRun) {
  const d = state.draft;
  readParams();
  if (!d.name.trim()) { toast(T('name_required'), 'error'); return; }
  if (!d.targets.length) { toast(T('targets_required'), 'error'); return; }
  const tests = SCENARIO_ORDER.filter((s) => d.config[s]).length;
  if (!tests) { toast(T('tests_required'), 'error'); return; }

  // Config: pomijamy wylaczone (null) scenariusze, zeby Core nie liczyl ich jako aktywne.
  const config = {
    prompt_tokens: d.config.prompt_tokens,
    gen_tokens: d.config.gen_tokens,
    request_timeout_secs: d.config.request_timeout_secs || 120,
  };
  SCENARIO_ORDER.forEach((s) => { if (d.config[s]) config[s] = d.config[s]; });

  const targets = d.targets.map((t) => ({
    id: t.id,
    kind: t.kind,
    service_ref: t.serviceRef || null,
    api_type: t.apiType,
    host: t.host,
    port: t.port,
    // undefined → zachowaj zapisany klucz; '' → wyczysc; wartosc → zapisz.
    api_key: t.apiKey,
    model: t.model,
    label: t.label,
  }));

  let benchmarkId = d.id;
  try {
    const resp = await ApiBinary.one('benchmarkSaveRequest', {
      id: d.id || undefined,
      name: d.name.trim(),
      configJson: JSON.stringify(config),
      targets,
    });
    benchmarkId = resp?.id || d.id;
    d.id = benchmarkId;
    toast(T('saved'), 'success');
  } catch (err) {
    toast(err.message || T('save_failed'), 'error');
    return;
  }

  if (!thenRun) { await goList(); return; }
  try {
    const resp = await ApiBinary.one('benchmarkStartRunRequest', { benchmarkId });
    const runId = resp?.runId;
    if (!runId) { toast(T('run_start_failed'), 'error'); return; }
    openRun(runId, benchmarkId, d.name.trim());
  } catch (err) {
    toast(err.message || T('run_start_failed'), 'error');
  }
}

// ---------------------------------------------------------------------------
// M4 — Run live (streaming).
// ---------------------------------------------------------------------------

function teardownRun() {
  if (state.runUnsub) { try { state.runUnsub(); } catch (_) {} state.runUnsub = null; }
  if (state.runTimer) { clearInterval(state.runTimer); state.runTimer = null; }
  if (state.runPoll) { clearInterval(state.runPoll); state.runPoll = null; }
  if (state.resultsTimeout) { clearTimeout(state.resultsTimeout); state.resultsTimeout = null; }
}

async function openRun(runId, benchmarkId, benchmarkName) {
  teardownRun();
  state.view = 'run';
  state.runId = runId;
  state.runBenchmarkId = benchmarkId;
  state.runBenchmarkName = benchmarkName || '';
  state.runLog = [];
  state.runStatus = 'running';

  // Poznaj definicje (targety + aktywne scenariusze) do macierzy.
  try {
    const resp = await ApiBinary.one('benchmarkGetRequest', { id: benchmarkId });
    const b = resp?.benchmark;
    state.runTargets = (b?.targets || []).map((t) => ({ id: t.id, label: t.model || t.label, model: t.model }));
    const cfg = parseJson(b?.configJson, {});
    state.runScenarios = SCENARIO_ORDER.filter((s) => cfg[s]);
  } catch {
    state.runTargets = [];
    state.runScenarios = SCENARIO_ORDER;
  }

  renderRun();

  // Status → started_at + status.
  try {
    const st = await ApiBinary.one('benchmarkRunStatusRequest', { runId });
    state.runStartedMs = parseServerTs(st?.startedAt || '') || Date.now();
    state.runStatus = st?.status || 'running';
  } catch { state.runStartedMs = Date.now(); }

  startElapsedTimer();
  await refreshRunResults();

  if (state.runStatus === 'running') {
    // Poll czesciowych wynikow co 4 s (macierz + tabela rosna).
    state.runPoll = setInterval(refreshRunResults, 4000);
    // Subskrypcja live-logu + progresu.
    try {
      state.runUnsub = await ApiBinary.subscribe('benchmarkRunStreamRequest', { runId }, {
        onChunk: onRunChunk,
        onEnd: onRunEnd,
        onError: (body) => appendRunLog(`[stream error] ${body?.message || ''}`),
      });
    } catch (err) {
      appendRunLog(`[stream error] ${err?.message || ''}`);
    }
  } else {
    // Run juz zakonczony — pokaz wynik od razu.
    finalizeRun(state.runStatus);
  }
}

function renderRun() {
  const crumbs = [
    { label: T('crumb_analytics') },
    { label: T('title'), onClick: goList },
    { label: state.runBenchmarkName || T('run') },
    { label: T('run_short', { id: state.runId.slice(0, 8) }) },
  ];
  root().innerHTML = `
    ${crumbsHtml(crumbs)}
    <div class="run-head">
      <div style="flex:1;">
        <div class="r-name">${escapeHtml(state.runBenchmarkName)} <span class="chip running" id="run-status-chip"><span class="dot"></span>${escapeHtml(T('running'))}</span></div>
        <div class="r-sub">${escapeHtml(T('run_started', { date: fmtDate(new Date(state.runStartedMs || Date.now()).toISOString()) }))}</div>
      </div>
      <div><div class="elapsed" id="run-elapsed">00:00</div><div class="text-3 text-xs center">elapsed</div></div>
      <tf-button variant="danger" icon="stop" id="run-stop">${escapeHtml(T('stop'))}</tf-button>
    </div>
    <div class="prog-row">
      <span class="prog-label">${escapeHtml(T('overall_progress'))}</span>
      <div class="progress"><span id="run-progress" style="width:0%"></span></div>
      <span class="pct" id="run-pct">0%</span>
    </div>
    <div class="grid-2">
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${escapeHtml(T('matrix_title'))}</div>
          <div class="hint">${escapeHtml(T('matrix_hint'))}</div>
        </div>
        <div id="run-matrix"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${escapeHtml(T('live_log'))}</div>
          <div class="hint">${escapeHtml(T('live_log_hint'))}</div>
        </div>
        <div class="live-log" id="run-log"></div>
      </div>
    </div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${escapeHtml(T('partial_results'))}</div>
        <div class="hint">${escapeHtml(T('partial_results_hint'))}</div>
      </div>
      <div id="run-results"></div>
    </div>
  `;
  wireCrumbs(crumbs);
  byId('run-stop')?.addEventListener('click', cancelRun);
  renderMatrix([]);
}

function startElapsedTimer() {
  const tick = () => {
    const el = byId('run-elapsed');
    if (el) el.textContent = fmtClock(Date.now() - (state.runStartedMs || Date.now()));
  };
  tick();
  state.runTimer = setInterval(tick, 1000);
}

function appendRunLog(line) {
  if (!line) return;
  state.runLog.push(line);
  if (state.runLog.length > 300) state.runLog = state.runLog.slice(-300);
  const box = byId('run-log');
  if (box) { box.textContent = state.runLog.join('\n'); box.scrollTop = box.scrollHeight; }
}

function onRunChunk(body) {
  if (!body || body.variant !== 'BenchmarkRunStreamChunk') return;
  if (body.runId && body.runId !== state.runId) return;
  const ts = body.tsMs ? new Date(body.tsMs).toLocaleTimeString(lang()) : '';
  if (body.kind === 'progress') {
    applyProgress(body.progressPct);
  } else if (body.kind === 'result') {
    refreshRunResults();
  } else {
    // log | phase.
    appendRunLog(`${ts ? ts + '  ' : ''}${body.phase ? '[' + body.phase + '] ' : ''}${body.line || ''}`);
    if (Number.isFinite(body.progressPct) && body.progressPct > 0) applyProgress(body.progressPct);
  }
}

function applyProgress(pct) {
  const p = Math.max(0, Math.min(100, Math.round(pct || 0)));
  const bar = byId('run-progress');
  const label = byId('run-pct');
  if (bar) bar.style.width = `${p}%`;
  if (label) label.textContent = `${p}%`;
}

function onRunEnd(body) {
  const status = body?.status || 'success';
  if (body?.error) appendRunLog(`[${status}] ${body.error}`);
  finalizeRun(status);
}

function finalizeRun(status) {
  teardownRun();
  state.runStatus = status;
  applyProgress(100);
  const chip = byId('run-status-chip');
  if (chip) { chip.className = `chip ${statusPillClass(status)}`; chip.innerHTML = `<span class="dot"></span>${escapeHtml(status)}`; }
  const stop = byId('run-stop');
  if (stop) stop.remove();
  refreshRunResults();
  if (status === 'success') {
    toast(T('run_finished'), 'success');
    state.resultsTimeout = setTimeout(() => {
      state.resultsTimeout = null;
      openResults(state.runId, state.runBenchmarkId, state.runBenchmarkName);
    }, 800);
  } else {
    toast(T('run_ended', { status }), status === 'cancelled' ? 'info' : 'error');
  }
}

async function cancelRun() {
  try {
    await ApiBinary.one('benchmarkCancelRunRequest', { runId: state.runId });
    appendRunLog(`[cancel] ${T('cancel_requested')}`);
  } catch (err) {
    toast(err.message || T('cancel_failed'), 'error');
  }
}

// Odswiez czesciowe/koncowe wyniki + macierz statusow.
// Wiersze wynikow niosa `targetLabel` = displayName SERWISU, ktory jest nazwa
// SILNIKA (np. "vLLM", "MLX"), a nie modelu. Podmieniamy etykiete na rzeczywisty
// model targetu (to jego benchmarkujemy; silnik to tylko backend). Mapowanie po
// `targetId` z targetow benchmarku/runu. Naprawia tez stare runy zapisane z
// etykieta-silnikiem, bo model bierzemy ze zrodla targetu przy renderze.
function withTargetModel(rows, targetsOrBench) {
  const targets = Array.isArray(targetsOrBench) ? targetsOrBench : (targetsOrBench?.targets || []);
  const modelById = new Map();
  for (const t of targets) {
    const m = t && t.model ? String(t.model).trim() : '';
    if (m && t.id != null) modelById.set(t.id, m);
  }
  if (!modelById.size) return rows;
  return rows.map((r) => (modelById.has(r.targetId) ? { ...r, targetLabel: modelById.get(r.targetId) } : r));
}

async function refreshRunResults() {
  if (state.view !== 'run') return;
  let rows = [];
  try {
    const resp = await ApiBinary.one('benchmarkRunResultsRequest', { runId: state.runId });
    rows = withTargetModel(Array.isArray(resp?.results) ? resp.results : [], state.runTargets);
  } catch { return; }
  renderMatrix(rows);
  renderResultsTable(byId('run-results'), rows, { partial: true });
}

// Macierz target×scenariusz: done gdy sa wyniki, running = pierwszy niezakonczony.
function renderMatrix(rows) {
  const host = byId('run-matrix');
  if (!host) return;
  const targets = state.runTargets.length
    ? state.runTargets
    : [...new Set(rows.map((r) => r.targetLabel))].map((label) => ({ id: label, label }));
  const scenarios = state.runScenarios.length ? state.runScenarios : SCENARIO_ORDER;
  const colorMap = seriesColorMap(targets.map((t) => t.label));
  const doneSet = new Set(rows.map((r) => `${r.targetLabel}|${r.scenario}`));

  const head = `<tr><th></th>${scenarios.map((s) => `<th>${escapeHtml(scenarioLabel(s))}</th>`).join('')}</tr>`;
  const body = targets.map((t) => {
    let markedRunning = false;
    const cells = scenarios.map((s) => {
      const done = doneSet.has(`${t.label}|${s}`);
      let cls = 'pending';
      let txt = `⏳ ${T('pending')}`;
      if (done) { cls = 'done'; txt = `✓ ${T('done')}`; }
      else if (state.runStatus === 'running' && !markedRunning) { cls = 'running'; txt = `▶ ${scenarioLabel(s)}`; markedRunning = true; }
      return `<td class="cell ${cls}">${escapeHtml(txt)}</td>`;
    }).join('');
    return `<tr><td class="rowhead"><span class="serie-dot" style="background:${colorMap.get(t.label)}"></span>${escapeHtml(t.label)}<span class="sub">${escapeHtml(t.model || '')}</span></td>${cells}</tr>`;
  }).join('');

  host.innerHTML = `<table class="run-matrix"><thead>${head}</thead><tbody>${body}</tbody></table>`;
}

// ---------------------------------------------------------------------------
// M5 — Wyniki runu.
// ---------------------------------------------------------------------------

async function openResults(runId, benchmarkId, benchmarkName) {
  teardownRun();
  state.view = 'results';
  state.resultsRunId = runId;
  state.resultsBenchmarkId = benchmarkId;
  state.resultsBenchmarkName = benchmarkName || '';

  root().innerHTML = `<div class="bench-empty">${escapeHtml(T('loading'))}</div>`;
  let rows = [];
  let status = null;
  try {
    const [res, st, benchResp] = await Promise.all([
      ApiBinary.one('benchmarkRunResultsRequest', { runId }),
      ApiBinary.one('benchmarkRunStatusRequest', { runId }).catch(() => null),
      state.resultsBenchmarkId
        ? ApiBinary.one('benchmarkGetRequest', { id: state.resultsBenchmarkId }).catch(() => null)
        : Promise.resolve(null),
    ]);
    rows = withTargetModel(Array.isArray(res?.results) ? res.results : [], benchResp?.benchmark);
    status = st;
  } catch (err) {
    root().innerHTML = `<div class="bench-empty">${escapeHtml(err.message || T('load_failed'))}</div>`;
    return;
  }
  renderResults(rows, status);
}

function renderResults(rows, status) {
  const crumbs = [
    { label: T('crumb_analytics') },
    { label: T('title'), onClick: goList },
    { label: state.resultsBenchmarkName || T('run'), onClick: () => goWizard(state.resultsBenchmarkId) },
    { label: T('run_results_short', { id: state.resultsRunId.slice(0, 8) }) },
  ];
  const st = (status?.status) || 'success';
  const labels = [...new Set(rows.map((r) => r.targetLabel))];
  const colorMap = seriesColorMap(labels);
  const legend = labels.map((l) => `<span class="lg"><span class="sw" style="background:${colorMap.get(l)}"></span>${escapeHtml(l)}</span>`).join('');

  root().innerHTML = `
    ${crumbsHtml(crumbs)}
    <div class="run-head">
      <div style="flex:1;">
        <div class="r-name">${escapeHtml(state.resultsBenchmarkName)} · ${escapeHtml(T('run_results_short', { id: state.resultsRunId.slice(0, 8) }))}
          <span class="chip ${statusPillClass(st)}"><span class="dot"></span>${escapeHtml(st)}</span></div>
        <div class="r-sub">${escapeHtml(resultsSubtitle(status, rows))}</div>
      </div>
      <div class="bench-actions">
        <tf-button variant="secondary" icon="download" id="res-csv">${escapeHtml(T('export_csv'))}</tf-button>
        <tf-button variant="secondary" icon="file" id="res-pdf">${escapeHtml(T('export_pdf'))}</tf-button>
        <tf-button variant="primary" icon="refresh" id="res-compare">${escapeHtml(T('compare'))}</tf-button>
      </div>
    </div>
    <div class="chart-legend" style="margin-bottom:14px;">${legend}</div>
    <div class="section-card">
      <div class="section-card-head">
        <div class="title">${escapeHtml(T('results_table'))}</div>
        <div class="hint">${escapeHtml(T('results_table_hint'))}</div>
      </div>
      <div id="res-table"></div>
    </div>
    <div class="grid-2">
      <div class="section-card">
        <div class="section-card-head"><div class="title">${escapeHtml(T('chart_decode_conc'))}</div><div class="hint">${escapeHtml(T('chart_decode_conc_hint'))}</div></div>
        <div id="chart-conc"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${escapeHtml(T('chart_ctx'))}</div><div class="hint">${escapeHtml(T('chart_ctx_hint'))}</div></div>
        <div id="chart-ctx"></div>
      </div>
    </div>
    <div class="grid-2">
      <div class="section-card">
        <div class="section-card-head"><div class="title">${escapeHtml(T('chart_latency'))}</div><div class="hint">${escapeHtml(T('chart_latency_hint'))}</div></div>
        <div id="chart-lat"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${escapeHtml(T('chart_stability'))}</div><div class="hint">${escapeHtml(T('chart_stability_hint'))}</div></div>
        <div id="chart-stab"></div>
      </div>
    </div>
  `;
  wireCrumbs(crumbs);
  byId('res-csv')?.addEventListener('click', () => exportResultsCsv(rows));
  byId('res-pdf')?.addEventListener('click', () => exportResultsPdf(rows, status));
  byId('res-compare')?.addEventListener('click', () => goCompare(state.resultsBenchmarkId, state.resultsBenchmarkName, state.resultsRunId));

  renderResultsTable(byId('res-table'), rows, { partial: false });
  renderConcurrencyChart(byId('chart-conc'), rows, colorMap);
  renderContextChart(byId('chart-ctx'), rows, colorMap);
  renderLatencyChart(byId('chart-lat'), rows, colorMap);
  renderStabilityChart(byId('chart-stab'), rows, colorMap);
}

function resultsSubtitle(status, rows) {
  const total = rows.reduce((s, r) => s + Number(r.requests || 0), 0);
  const dur = status ? fmtDuration(status.startedAt, status.finishedAt) : '—';
  const targets = new Set(rows.map((r) => r.targetLabel)).size;
  const scenarios = new Set(rows.map((r) => r.scenario)).size;
  return T('results_subtitle', { date: fmtDate(status?.startedAt), dur, targets, scenarios, requests: total });
}

// Tabela wynikow a la llama-bench: grupy per scenariusz, ★ najlepszy w kolumnie.
function renderResultsTable(host, rows, { partial }) {
  if (!host) return;
  if (!rows.length) {
    host.innerHTML = `<div class="bench-empty">${escapeHtml(partial ? T('no_partial_yet') : T('no_results'))}</div>`;
    return;
  }
  const colorMap = seriesColorMap(rows.map((r) => r.targetLabel));
  const scenarios = SCENARIO_ORDER.filter((s) => rows.some((r) => r.scenario === s));
  const html = scenarios.map((scn) => {
    const group = rows.filter((r) => r.scenario === scn).sort(variantSort);
    // Najlepszy w kolumnie (w ramach grupy): TTFT/latencja min, decode/prefill max.
    const best = {
      ttft: minIdx(group.map((r) => r.ttftMsMean)),
      prefill: maxIdx(group.map((r) => r.prefillTpsMean)),
      decode: maxIdx(group.map((r) => r.decodeTpsMean)),
      p50: minIdx(group.map((r) => r.p50Ms)),
    };
    const body = group.map((r, i) => {
      const errPct = r.requests ? (r.errors / r.requests) * 100 : 0;
      const p = [r.p50Ms, r.p90Ms, r.p99Ms].map((v) => v == null ? '—' : fmtMsInt(v)).join(' / ');
      return `<tr>
        <td><span class="serie-dot" style="background:${colorMap.get(r.targetLabel)}"></span><strong>${escapeHtml(r.targetLabel)}</strong></td>
        <td>${escapeHtml(scenarioLabel(scn))}</td>
        <td class="mono">${escapeHtml(variantText(r))}</td>
        <td class="num-r mono ${best.ttft === i ? 'best' : ''}">${fmtStat(r.ttftMsMean, r.ttftMsSigma, 0)}</td>
        <td class="num-r mono ${best.prefill === i ? 'best' : ''}">${r.prefillTpsMean == null ? '—' : escapeHtml(fmtNum(r.prefillTpsMean, 0))}</td>
        <td class="num-r mono ${best.decode === i ? 'best' : ''}">${fmtStat(r.decodeTpsMean, r.decodeTpsSigma, 1)}</td>
        <td class="num-r mono ${best.p50 === i ? 'best' : ''}">${escapeHtml(p)}</td>
        <td class="num-r mono">${escapeHtml(fmtNum(errPct, 1))}</td>
      </tr>`;
    }).join('');
    return `
      <div class="results-group-title">${escapeHtml(T(`scenario_${scn}_name`))}</div>
      <table class="tf-table results-table">
        <thead><tr>
          <th>${escapeHtml(T('col_model'))}</th><th>${escapeHtml(T('col_test'))}</th><th>${escapeHtml(T('col_params'))}</th>
          <th class="num-r">${escapeHtml(T('col_ttft'))}</th><th class="num-r">${escapeHtml(T('col_prefill'))}</th>
          <th class="num-r">${escapeHtml(T('col_decode'))}</th><th class="num-r">${escapeHtml(T('col_percentiles'))}</th>
          <th class="num-r">${escapeHtml(T('col_err'))}</th>
        </tr></thead>
        <tbody>${body}</tbody>
      </table>`;
  }).join('');
  host.innerHTML = html;
}

// Sortowanie wariantow w grupie (concurrency / ctx / minute rosnaco).
function variantSort(a, b) {
  const va = parseJson(a.variantJson, {});
  const vb = parseJson(b.variantJson, {});
  const key = (v) => v.concurrency ?? v.minute ?? v.prompt_tokens ?? 0;
  return key(va) - key(vb) || String(a.targetLabel).localeCompare(String(b.targetLabel));
}

function variantText(r) {
  const v = parseJson(r.variantJson, {});
  if (r.scenario === 'throughput') return `c=${v.concurrency ?? '?'}`;
  if (r.scenario === 'context') return `ctx=${v.prompt_tokens ?? '?'}`;
  if (r.scenario === 'sustained') return `min ${v.minute ?? 0} · c=${v.concurrency ?? '?'}`;
  return `p${v.prompt_tokens ?? '?'} g${v.gen_tokens ?? '?'}`;
}

function minIdx(arr) {
  let bi = -1; let bv = Infinity;
  arr.forEach((v, i) => { if (v != null && v < bv) { bv = v; bi = i; } });
  return bi;
}
function maxIdx(arr) {
  let bi = -1; let bv = -Infinity;
  arr.forEach((v, i) => { if (v != null && v > bv) { bv = v; bi = i; } });
  return bi;
}

// ---------------------------------------------------------------------------
// Wykresy inline SVG.
// ---------------------------------------------------------------------------

function chartEmpty(host, msg) {
  if (host) host.innerHTML = `<div class="chart-empty">${escapeHtml(msg)}</div>`;
}

function gridLinesY(top, bottom, left, right, maxVal, steps = 4) {
  const out = [];
  for (let i = 0; i <= steps; i += 1) {
    const y = top + ((bottom - top) * i) / steps;
    const val = maxVal * (1 - i / steps);
    out.push(`<line class="gridline" x1="${left}" y1="${y.toFixed(1)}" x2="${right}" y2="${y.toFixed(1)}"/>`);
    out.push(`<text x="${left - 4}" y="${(y + 3).toFixed(1)}" text-anchor="end">${escapeHtml(fmtNum(val, 0))}</text>`);
  }
  return out.join('');
}

// Wykres 1: decode t/s per-request vs poziom rownoleglosci (slupki grupowane).
function renderConcurrencyChart(host, rows, colorMap) {
  const tp = rows.filter((r) => r.scenario === 'throughput');
  if (!tp.length) { chartEmpty(host, T('no_data')); return; }
  const labels = [...new Set(tp.map((r) => r.targetLabel))];
  const levels = [...new Set(tp.map((r) => parseJson(r.variantJson, {}).concurrency))].filter((v) => v != null).sort((a, b) => a - b);
  const maxV = Math.max(1, ...tp.map((r) => r.decodeTpsMean || 0)) * 1.1;
  const W = 960; const H = 220; const top = 10; const bottom = 190; const left = 40; const right = 950;
  const groupW = (right - left) / levels.length;
  const barW = Math.min(40, (groupW * 0.7) / labels.length);
  let bars = '';
  levels.forEach((lvl, gi) => {
    const gx = left + groupW * gi + (groupW - barW * labels.length) / 2;
    labels.forEach((lbl, li) => {
      const row = tp.find((r) => r.targetLabel === lbl && parseJson(r.variantJson, {}).concurrency === lvl);
      const v = row?.decodeTpsMean || 0;
      const h = (v / maxV) * (bottom - top);
      const x = gx + li * barW;
      bars += `<rect x="${x.toFixed(1)}" y="${(bottom - h).toFixed(1)}" width="${(barW - 2).toFixed(1)}" height="${h.toFixed(1)}" fill="${colorMap.get(lbl)}"><title>${escapeHtml(lbl)} c=${lvl}: ${fmtNum(v, 1)} t/s</title></rect>`;
    });
    bars += `<text x="${(gx + barW * labels.length / 2).toFixed(1)}" y="204" text-anchor="middle">c = ${lvl}</text>`;
  });
  host.innerHTML = `<svg class="chart-svg chart-h" viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">${gridLinesY(top, bottom, left, right, maxV)}${bars}</svg>`;
}

// Wykres 2: degradacja decode t/s vs dlugosc kontekstu (linie).
function renderContextChart(host, rows, colorMap) {
  const cx = rows.filter((r) => r.scenario === 'context');
  if (!cx.length) { chartEmpty(host, T('no_data')); return; }
  const labels = [...new Set(cx.map((r) => r.targetLabel))];
  const lengths = [...new Set(cx.map((r) => parseJson(r.variantJson, {}).prompt_tokens))].filter((v) => v != null).sort((a, b) => a - b);
  const maxV = Math.max(1, ...cx.map((r) => r.decodeTpsMean || 0)) * 1.1;
  const top = 10; const bottom = 190; const left = 40; const right = 950;
  const stepX = lengths.length > 1 ? (right - left - 60) / (lengths.length - 1) : 0;
  const x0 = left + 30;
  let series = '';
  labels.forEach((lbl) => {
    const pts = lengths.map((len, i) => {
      const row = cx.find((r) => r.targetLabel === lbl && parseJson(r.variantJson, {}).prompt_tokens === len);
      const v = row?.decodeTpsMean || 0;
      const y = bottom - (v / maxV) * (bottom - top);
      return `${(x0 + stepX * i).toFixed(1)},${y.toFixed(1)}`;
    });
    series += `<polyline points="${pts.join(' ')}" fill="none" stroke="${colorMap.get(lbl)}" stroke-width="2.5"/>`;
    series += pts.map((p) => { const [px, py] = p.split(','); return `<circle cx="${px}" cy="${py}" r="3.5" fill="${colorMap.get(lbl)}"/>`; }).join('');
  });
  const xlabels = lengths.map((len, i) => `<text x="${(x0 + stepX * i).toFixed(1)}" y="207" text-anchor="middle">${escapeHtml(fmtInt(len))}</text>`).join('');
  host.innerHTML = `<svg class="chart-svg chart-h" viewBox="0 0 960 220" preserveAspectRatio="none">${gridLinesY(top, bottom, left, right, maxV)}${series}${xlabels}</svg>`;
}

// Wykres 3: latencje p50/p90/p99 (poziome slupki, test latencji).
function renderLatencyChart(host, rows, colorMap) {
  const lat = rows.filter((r) => r.scenario === 'latency' && r.p50Ms != null);
  if (!lat.length) { chartEmpty(host, T('no_data')); return; }
  const maxV = Math.max(1, ...lat.map((r) => r.p99Ms || r.p90Ms || r.p50Ms || 0)) * 1.05;
  const left = 150; const right = 950; const top = 8;
  const rowH = 18; const gap = 10;
  let y = top;
  let content = '';
  const axis = [0, 0.25, 0.5, 0.75, 1].map((f) => {
    const x = left + (right - left) * f;
    return `<line class="gridline" x1="${x}" y1="6" x2="${x}" y2="168"/><text x="${x}" y="182" text-anchor="middle">${escapeHtml(fmtInt(Math.round(maxV * f)))}</text>`;
  }).join('');
  lat.forEach((r) => {
    const c = colorMap.get(r.targetLabel);
    [['p50', r.p50Ms, 1], ['p90', r.p90Ms, 0.65], ['p99', r.p99Ms, 0.4]].forEach(([name, v, op], idx) => {
      if (v == null) return;
      const w = ((v / maxV) * (right - left));
      content += `<text x="${left - 6}" y="${y + 9}" text-anchor="end">${escapeHtml(idx === 0 ? r.targetLabel + ' ' + name : name)}</text>`;
      content += `<rect x="${left}" y="${y}" width="${w.toFixed(1)}" height="11" fill="${c}" opacity="${op}"><title>${escapeHtml(r.targetLabel)} ${name}: ${fmtMsInt(v)} ms</title></rect>`;
      y += rowH;
    });
    y += gap;
  });
  host.innerHTML = `<svg class="chart-svg chart-h-sm" viewBox="0 0 960 190" preserveAspectRatio="none">${axis}${content}</svg>`;
}

// Wykres 4: stabilnosc — decode t/s w czasie (per minuta, scenariusz sustained).
function renderStabilityChart(host, rows, colorMap) {
  const su = rows.filter((r) => r.scenario === 'sustained');
  if (!su.length) { chartEmpty(host, T('no_data')); return; }
  const labels = [...new Set(su.map((r) => r.targetLabel))];
  const minutes = [...new Set(su.map((r) => parseJson(r.variantJson, {}).minute ?? 0))].sort((a, b) => a - b);
  const maxV = Math.max(1, ...su.map((r) => r.decodeTpsMean || 0)) * 1.1;
  const top = 8; const bottom = 164; const left = 40; const right = 950;
  const stepX = minutes.length > 1 ? (right - left) / (minutes.length - 1) : 0;
  let series = '';
  labels.forEach((lbl) => {
    const pts = minutes.map((mn, i) => {
      const row = su.find((r) => r.targetLabel === lbl && (parseJson(r.variantJson, {}).minute ?? 0) === mn);
      const v = row?.decodeTpsMean || 0;
      const y = bottom - (v / maxV) * (bottom - top);
      return `${(left + stepX * i).toFixed(1)},${y.toFixed(1)}`;
    });
    series += `<polyline points="${pts.join(' ')}" fill="none" stroke="${colorMap.get(lbl)}" stroke-width="2"/>`;
  });
  const lastMin = minutes[minutes.length - 1] ?? 0;
  const xlabels = `<text x="${left}" y="180">0 min</text><text x="${right}" y="180" text-anchor="end">${escapeHtml(String(lastMin))} min</text>`;
  host.innerHTML = `<svg class="chart-svg chart-h-sm" viewBox="0 0 960 190" preserveAspectRatio="none">${gridLinesY(top, bottom, left, right, maxV)}${series}${xlabels}</svg>`;
}

// ---------------------------------------------------------------------------
// M6 — Porownanie runow.
// ---------------------------------------------------------------------------

async function goCompare(benchmarkId, benchmarkName, preselectRunId) {
  teardownRun();
  state.view = 'compare';
  state.compareBenchmarkId = benchmarkId;
  root().innerHTML = `<div class="bench-empty">${escapeHtml(T('loading'))}</div>`;
  let runs = [];
  try {
    const resp = await ApiBinary.one('benchmarkListRunsRequest', { benchmarkId });
    runs = (Array.isArray(resp?.runs) ? resp.runs : []).filter((r) => r.status === 'success');
  } catch (err) {
    root().innerHTML = `<div class="bench-empty">${escapeHtml(err.message || T('load_failed'))}</div>`;
    return;
  }
  if (runs.length < 2) {
    root().innerHTML = `${crumbsHtml([{ label: T('title'), onClick: goList }, { label: T('compare') }])}<div class="bench-empty">${escapeHtml(T('need_two_runs'))}</div>`;
    wireCrumbs([{ label: T('title'), onClick: goList }]);
    return;
  }
  const runB = preselectRunId && runs.some((r) => r.id === preselectRunId) ? preselectRunId : runs[0].id;
  const runA = runs.find((r) => r.id !== runB)?.id || runs[1].id;
  renderCompareShell(benchmarkName, runs, runA, runB);
  await loadCompare(runA, runB, runs);
}

function renderCompareShell(benchmarkName, runs, runA, runB) {
  const crumbs = [
    { label: T('crumb_analytics') },
    { label: T('title'), onClick: goList },
    { label: benchmarkName || T('benchmark') },
    { label: T('compare') },
  ];
  const opt = (r, sel) => `<option value="${escapeAttr(r.id)}" ${r.id === sel ? 'selected' : ''}>#${escapeHtml(r.id.slice(0, 8))} · ${escapeHtml(fmtDate(r.startedAt))}</option>`;
  root().innerHTML = `
    ${crumbsHtml(crumbs)}
    <div class="run-select">
      <div class="run-box a">
        <div class="rb-label">${escapeHtml(T('run_a'))}</div>
        <tf-select id="cmp-a" value="${escapeAttr(runA)}">${runs.map((r) => opt(r, runA)).join('')}</tf-select>
        <div class="rb-meta" id="cmp-a-meta"></div>
      </div>
      <div class="vs">VS</div>
      <div class="run-box b">
        <div class="rb-label">${escapeHtml(T('run_b'))}</div>
        <tf-select id="cmp-b" value="${escapeAttr(runB)}">${runs.map((r) => opt(r, runB)).join('')}</tf-select>
        <div class="rb-meta" id="cmp-b-meta"></div>
      </div>
    </div>
    <div class="section-card">
      <div class="section-card-head"><div class="title">${escapeHtml(T('regressions'))}</div><div class="hint">${escapeHtml(T('regressions_hint'))}</div></div>
      <div class="regres-chips" id="cmp-regres"></div>
    </div>
    <div class="grid-2">
      <div class="section-card">
        <div class="section-card-head"><div class="title">${escapeHtml(T('delta_table'))}</div><div class="hint">${escapeHtml(T('delta_table_hint'))}</div></div>
        <div id="cmp-table"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head"><div class="title">${escapeHtml(T('chart_ab'))}</div><div class="hint">${escapeHtml(T('chart_ab_hint'))}</div></div>
        <div class="chart-legend" style="margin-bottom:10px;">
          <span class="lg"><span class="sw" style="background:var(--accent-1)"></span>${escapeHtml(T('run_a'))}</span>
          <span class="lg"><span class="sw" style="background:var(--accent-2)"></span>${escapeHtml(T('run_b'))}</span>
          <span class="lg"><span class="sw" style="background:var(--danger)"></span>${escapeHtml(T('regression'))}</span>
        </div>
        <div id="cmp-chart"></div>
      </div>
    </div>
  `;
  wireCrumbs(crumbs);
  byId('cmp-a')?.addEventListener('change', (e) => loadCompare(e.detail?.value || runA, byId('cmp-b')?.value, runs));
  byId('cmp-b')?.addEventListener('change', (e) => loadCompare(byId('cmp-a')?.value, e.detail?.value || runB, runs));
}

// Metryki porownania: [klucz, etykieta, funkcja(row→value), kierunek(1=wyzej lepiej,-1=nizej lepiej)].
function compareMetrics() {
  const decodeC1 = (rows) => firstVal(rows, (r) => r.scenario === 'latency', 'decodeTpsMean');
  return [
    { key: 'ttft', label: T('m_ttft'), unit: 'ms', dir: -1, val: (rows) => firstVal(rows, (r) => r.scenario === 'latency', 'ttftMsMean') },
    { key: 'prefill', label: T('m_prefill'), unit: 't/s', dir: 1, val: (rows) => firstVal(rows, (r) => r.scenario === 'latency', 'prefillTpsMean') },
    { key: 'decode1', label: T('m_decode_c1'), unit: 't/s', dir: 1, val: decodeC1 },
    { key: 'p50', label: T('m_p50'), unit: 'ms', dir: -1, val: (rows) => firstVal(rows, (r) => r.scenario === 'latency', 'p50Ms') },
    { key: 'p99', label: T('m_p99'), unit: 'ms', dir: -1, val: (rows) => firstVal(rows, (r) => r.scenario === 'latency', 'p99Ms') },
    { key: 'ctx32', label: T('m_decode_ctx_max'), unit: 't/s', dir: 1, val: (rows) => ctxDecode(rows, 'max') },
    { key: 'ttft32', label: T('m_ttft_ctx_max'), unit: 'ms', dir: -1, val: (rows) => ctxTtft(rows, 'max') },
  ];
}

function firstVal(rows, pred, field) {
  const r = rows.find(pred);
  return r ? r[field] : null;
}
function ctxDecode(rows, mode) {
  const cx = rows.filter((r) => r.scenario === 'context');
  if (!cx.length) return null;
  const sorted = cx.slice().sort((a, b) => (parseJson(a.variantJson, {}).prompt_tokens || 0) - (parseJson(b.variantJson, {}).prompt_tokens || 0));
  const r = mode === 'max' ? sorted[sorted.length - 1] : sorted[0];
  return r?.decodeTpsMean ?? null;
}
function ctxTtft(rows, mode) {
  const cx = rows.filter((r) => r.scenario === 'context');
  if (!cx.length) return null;
  const sorted = cx.slice().sort((a, b) => (parseJson(a.variantJson, {}).prompt_tokens || 0) - (parseJson(b.variantJson, {}).prompt_tokens || 0));
  const r = mode === 'max' ? sorted[sorted.length - 1] : sorted[0];
  return r?.ttftMsMean ?? null;
}

async function loadCompare(runA, runB, runs) {
  const [rowsA, rowsB] = await Promise.all([
    ApiBinary.one('benchmarkRunResultsRequest', { runId: runA }).then((r) => r?.results || []).catch(() => []),
    ApiBinary.one('benchmarkRunResultsRequest', { runId: runB }).then((r) => r?.results || []).catch(() => []),
  ]);
  const metaA = runs.find((r) => r.id === runA);
  const metaB = runs.find((r) => r.id === runB);
  const am = byId('cmp-a-meta'); if (am) am.textContent = fmtDuration(metaA?.startedAt, metaA?.finishedAt);
  const bm = byId('cmp-b-meta'); if (bm) bm.textContent = fmtDuration(metaB?.startedAt, metaB?.finishedAt);

  const metrics = compareMetrics();
  const deltas = metrics.map((m) => {
    const a = m.val(rowsA);
    const b = m.val(rowsB);
    let pct = null;
    if (a != null && b != null && a !== 0) pct = ((b - a) / Math.abs(a)) * 100;
    // Kierunek: better gdy zmiana zgodna z m.dir; regresja gdy > 5% w zla strone.
    let cls = 'neutral';
    if (pct != null) {
      const improved = (m.dir === 1 && b > a) || (m.dir === -1 && b < a);
      const worsened = (m.dir === 1 && b < a) || (m.dir === -1 && b > a);
      if (improved && Math.abs(pct) >= 0.05) cls = 'better';
      else if (worsened && Math.abs(pct) >= 0.05) cls = 'worse';
    }
    const regression = cls === 'worse' && pct != null && Math.abs(pct) > 5;
    return { m, a, b, pct, cls, regression };
  });

  renderCompareTable(byId('cmp-table'), deltas);
  renderRegressions(byId('cmp-regres'), deltas);
  renderCompareChart(byId('cmp-chart'), rowsA, rowsB);
}

function renderCompareTable(host, deltas) {
  if (!host) return;
  const body = deltas.map((d) => {
    const pctTxt = d.pct == null ? '—' : `${d.pct > 0 ? '+' : ''}${fmtNum(d.pct, 1)}%`;
    return `<tr>
      <td>${escapeHtml(d.m.label)} <span class="text-3">(${escapeHtml(d.m.unit)})</span></td>
      <td class="num-r mono">${d.a == null ? '—' : escapeHtml(fmtNum(d.a, d.m.unit === 'ms' ? 0 : 1))}</td>
      <td class="num-r mono">${d.b == null ? '—' : escapeHtml(fmtNum(d.b, d.m.unit === 'ms' ? 0 : 1))}</td>
      <td class="num-r"><span class="delta-val ${d.cls}">${escapeHtml(pctTxt)}</span></td>
    </tr>`;
  }).join('');
  host.innerHTML = `
    <table class="tf-table results-table">
      <thead><tr><th>${escapeHtml(T('col_metric'))}</th><th class="num-r">${escapeHtml(T('run_a'))}</th><th class="num-r">${escapeHtml(T('run_b'))}</th><th class="num-r">Δ%</th></tr></thead>
      <tbody>${body}</tbody>
    </table>`;
}

function renderRegressions(host, deltas) {
  if (!host) return;
  const regs = deltas.filter((d) => d.regression);
  const okCount = deltas.length - regs.length;
  const chips = regs.map((d) => `<span class="chip warning">${escapeHtml(d.m.label)}: ${d.pct > 0 ? '+' : ''}${escapeHtml(fmtNum(d.pct, 0))}% ⚠</span>`).join('');
  const okChip = `<span class="chip success">${escapeHtml(T('metrics_ok', { n: okCount }))}</span>`;
  host.innerHTML = (chips || `<span class="chip success">${escapeHtml(T('no_regressions'))}</span>`) + (regs.length ? okChip : '');
}

// Slupki A vs B: decode per wariant kontekstu (regresja = czerwony slupek B).
function renderCompareChart(host, rowsA, rowsB) {
  const cxA = rowsA.filter((r) => r.scenario === 'context');
  const cxB = rowsB.filter((r) => r.scenario === 'context');
  if (!cxA.length && !cxB.length) { chartEmpty(host, T('no_data')); return; }
  const lengths = [...new Set([...cxA, ...cxB].map((r) => parseJson(r.variantJson, {}).prompt_tokens))].filter((v) => v != null).sort((a, b) => a - b);
  const decodeFor = (rows, len) => rows.find((r) => parseJson(r.variantJson, {}).prompt_tokens === len)?.decodeTpsMean || 0;
  const maxV = Math.max(1, ...lengths.map((len) => Math.max(decodeFor(cxA, len), decodeFor(cxB, len)))) * 1.1;
  const top = 10; const bottom = 270; const left = 40; const right = 950;
  const groupW = (right - left) / Math.max(1, lengths.length);
  const barW = Math.min(52, groupW * 0.32);
  let bars = '';
  lengths.forEach((len, gi) => {
    const gx = left + groupW * gi + (groupW - barW * 2) / 2;
    const a = decodeFor(cxA, len);
    const b = decodeFor(cxB, len);
    const ha = (a / maxV) * (bottom - top);
    const hb = (b / maxV) * (bottom - top);
    const regression = a > 0 && b < a * 0.95;
    bars += `<rect x="${gx.toFixed(1)}" y="${(bottom - ha).toFixed(1)}" width="${barW.toFixed(1)}" height="${ha.toFixed(1)}" fill="var(--accent-1)"><title>A ctx ${len}: ${fmtNum(a, 1)}</title></rect>`;
    bars += `<rect x="${(gx + barW).toFixed(1)}" y="${(bottom - hb).toFixed(1)}" width="${barW.toFixed(1)}" height="${hb.toFixed(1)}" fill="${regression ? 'var(--danger)' : 'var(--accent-2)'}"><title>B ctx ${len}: ${fmtNum(b, 1)}</title></rect>`;
    bars += `<text x="${(gx + barW).toFixed(1)}" y="286" text-anchor="middle">ctx ${escapeHtml(fmtInt(len))}</text>`;
  });
  host.innerHTML = `<svg class="chart-svg" style="height:300px;" viewBox="0 0 960 300" preserveAspectRatio="none">${gridLinesY(top, bottom, left, right, maxV)}${bars}</svg>`;
}

// ---------------------------------------------------------------------------
// CSV eksport (klient-side).
// ---------------------------------------------------------------------------

function exportResultsCsv(rows) {
  const header = [T('col_model'), T('col_test'), T('col_params'), 'ttft_ms_mean', 'ttft_ms_sigma',
    'prefill_tps', 'decode_tps_mean', 'decode_tps_sigma', 'p50_ms', 'p90_ms', 'p99_ms', 'requests', 'errors'];
  const data = rows.map((r) => [
    r.targetLabel, scenarioLabel(r.scenario), variantText(r),
    r.ttftMsMean ?? '', r.ttftMsSigma ?? '', r.prefillTpsMean ?? '',
    r.decodeTpsMean ?? '', r.decodeTpsSigma ?? '', r.p50Ms ?? '', r.p90Ms ?? '', r.p99Ms ?? '',
    r.requests ?? 0, r.errors ?? 0,
  ]);
  downloadCsv(`benchmark-${state.resultsRunId.slice(0, 8)}.csv`, header, data);
}

// PDF eksport: otwiera okno print z DOKLADNIE tymi samymi tabelami wynikow co na
// stronie (renderResultsTable) + linkowany CSS appki (ten sam origin, wiec CSP
// przepuszcza) + `print-color-adjust: exact`, zeby ciemny motyw i kolory
// (best/★/errory) przetrwaly. User zapisuje jako PDF w dialogu drukowania.
function exportResultsPdf(rows, status) {
  if (!rows || !rows.length) { toast(T('no_results'), 'error'); return; }
  const tmp = document.createElement('div');
  renderResultsTable(tmp, rows, { partial: false });
  const tablesHtml = tmp.innerHTML;
  const title = `${state.resultsBenchmarkName || T('run')} · ${T('run_results_short', { id: state.resultsRunId.slice(0, 8) })}`;
  const sub = resultsSubtitle(status, rows);
  const win = window.open('', '_blank');
  if (!win) { toast(T('pdf_popup_blocked'), 'error'); return; }
  win.document.write(`<!doctype html><html lang="${escapeAttr(I18n.getLanguage())}"><head><meta charset="utf-8">
<title>${escapeHtml(title)}</title>
<link href="https://fonts.googleapis.com/css2?family=Manrope:wght@400;500;600;700;800&display=swap" rel="stylesheet">
<link rel="stylesheet" href="/css/style.css">
<link rel="stylesheet" href="/css/controls.css">
<link rel="stylesheet" href="/css/benchmark-studio.css">
<style>
  @page { margin: 12mm; size: A4 landscape; }
  html, body { background: var(--bg, #0b0e1a); color: var(--text, #e6e9f5);
    -webkit-print-color-adjust: exact; print-color-adjust: exact;
    margin: 0; padding: 28px 32px; font-family: 'Manrope', system-ui, -apple-system, sans-serif; }
  .pdf-h { font-size: 20px; font-weight: 800; letter-spacing: -0.01em; }
  .pdf-sub { color: var(--text-3, #8a90a8); font-size: 12px; margin: 6px 0 22px; }
  .results-table { width: 100%; }
  .results-group-title { margin: 20px 0 8px; }
  tr, .results-table { break-inside: avoid; }
</style></head><body>
<div class="pdf-h">${escapeHtml(title)}</div>
<div class="pdf-sub">${escapeHtml(sub)}</div>
${tablesHtml}
<script>window.addEventListener('load',function(){setTimeout(function(){window.focus();window.print();},400);});<\/script>
</body></html>`);
  win.document.close();
}

function downloadCsv(filename, header, rows) {
  const esc = (v) => {
    let s = String(v ?? '');
    // Formula-injection guard: a text cell starting with = + - @ (or tab/CR)
    // could execute as a formula in Excel/LibreOffice. Prefix with an apostrophe
    // so it stays text. Numbers (typeof number) never trigger this.
    if (typeof v === 'string' && /^[=+\-@\t\r]/.test(s)) s = `'${s}`;
    return /[",\n;]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  const lines = [header.map(esc).join(';'), ...rows.map((r) => r.map(esc).join(';'))];
  const blob = new Blob([`﻿${lines.join('\n')}`], { type: 'text/csv;charset=utf-8;' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = filename;
  document.body.appendChild(a); a.click(); a.remove();
  URL.revokeObjectURL(url);
}

function cssAttr(v) { return String(v).replace(/["\\]/g, '\\$&'); }

export default BenchmarkStudioScreen;
