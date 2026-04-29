// =============================================================================
// Plik: modules/dashboard.js
// Opis: Ekran Admin Home / Dashboard. Renderuje hero z maskotka i orbami,
//       trzy karty kontekstowe (Modele / Mesh / Active Flow), siatke metryk
//       czasu rzeczywistego i panele ostatnich zdarzen oraz aktywnych prze-
//       plywow. Subskrybuje broadcast `AuditEvent` z serwera dla live feedu
//       i odswieza metryki przez `dashboardMetricsRequest` co 5 sekund.
//       Kontrolki akcji w headerze uzywaja komponentow <tf-button>.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, formatDate } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { createRefresher } from '/js/lib/refresh.js';
import { isOnline as isOnlineHelper } from '/js/modules/mesh-helpers.js';

let metricsRefresher = null;
let auditUnsubscribe = null;
const recentEvents = [];
const MAX_RECENT_EVENTS = 8;

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}
function spriteCorner(id) {
  return `<svg class="icon icon-corner"><use href="#i-${id}"/></svg>`;
}

const DashboardScreen = {
  get title() { return I18n.t('nav.dashboard'); },
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${escapeHtml(I18n.t('home.title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('home.subtitle'))}</div>
        </div>
        <div class="actions">
          <tf-button variant="secondary" id="dash-refresh">${sprite('refresh')}${escapeHtml(I18n.t('home.refresh'))}</tf-button>
          <tf-button variant="primary" id="dash-add-node">${sprite('plus')}${escapeHtml(I18n.t('home.add_node'))}</tf-button>
        </div>
      </div>

      <div class="hero">
        <!-- Animowana siatka particles + linie miedzy bliskimi sasiadami.
             Renderowana w canvasie przez lib/hero-network.js (mountowane w mount()). -->
        <canvas class="hero-network" id="hero-network" aria-hidden="true"></canvas>

        <div class="orbs">
          <div class="orb-wrap" style="--angle: -70deg;   --float-delay: 0s;"><div class="orb red">${sprite('brain')}</div></div>
          <div class="orb-wrap" style="--angle: -35deg;   --float-delay: 0.6s;"><div class="orb yellow">${sprite('share')}</div></div>
          <div class="orb-wrap" style="--angle: 0deg;     --float-delay: 1.2s;"><div class="orb blue">${sprite('network-svg')}</div></div>
          <div class="orb-wrap" style="--angle: 35deg;    --float-delay: 1.8s;"><div class="orb green">${sprite('database')}</div></div>
          <div class="orb-wrap" style="--angle: 70deg;    --float-delay: 2.4s;"><div class="orb purple">${sprite('cloud')}</div></div>
        </div>
        <div class="hero-content">
          <img class="hero-mascot" src="/tentaflow.png" alt="">
          <div class="hero-title">TENTAFLOW</div>
          <div class="hero-tagline">${escapeHtml(I18n.t('home.tagline'))}</div>
        </div>
      </div>

      <div class="stat-grid">
        <div class="stat-card" id="stat-models">
          <div class="indicator"></div>
          <div class="label">${escapeHtml(I18n.t('home.stat_models'))}</div>
          <div class="value" id="stat-models-value">—</div>
          <div class="sub" id="stat-models-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="stat-card" id="stat-mesh">
          ${spriteCorner('network')}
          <div class="label">${escapeHtml(I18n.t('home.stat_mesh'))}</div>
          <div class="value" id="stat-mesh-value">—</div>
          <div class="sub" id="stat-mesh-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="stat-card" id="stat-flow">
          ${spriteCorner('chevron-right')}
          <div class="label">${escapeHtml(I18n.t('home.stat_flow'))}</div>
          <div class="value" id="stat-flow-value">—</div>
          <div class="sub" id="stat-flow-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
      </div>

      <div class="mesh-section-title">${sprite('dashboard')} ${escapeHtml(I18n.t('home.section_metrics'))}
        <span class="section-count" id="metrics-rtt">— ms RTT</span>
      </div>
      <div class="stat-grid" style="grid-template-columns: repeat(4, 1fr);">
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_tps'))}</div>
          <div class="value" id="m-tps">0</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_tps_sub'))}</div>
        </div>
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_active'))}</div>
          <div class="value" id="m-active">0</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_active_sub'))}</div>
        </div>
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_errors'))}</div>
          <div class="value" id="m-errors" style="color: var(--danger);">0</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_errors_sub'))}</div>
        </div>
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_services'))}</div>
          <div class="value" id="m-services" style="color: var(--success);">0</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_services_sub'))}</div>
        </div>
      </div>

      <div class="stat-grid" style="grid-template-columns: repeat(3, 1fr); margin-top: 12px;">
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_cpu'))}</div>
          <div class="value" id="m-cpu">0%</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_cpu_sub'))}</div>
        </div>
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_ram'))}</div>
          <div class="value" id="m-ram">0 / 0 GB</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_ram_sub'))}</div>
        </div>
        <div class="stat-card">
          <div class="label">${escapeHtml(I18n.t('home.metric_total_requests'))}</div>
          <div class="value" id="m-total">0</div>
          <div class="sub">${escapeHtml(I18n.t('home.metric_total_requests_sub'))}</div>
        </div>
      </div>

      <div style="display: grid; grid-template-columns: 1.6fr 1fr; gap: 16px; margin-top: 24px;">
        <div>
          <div class="mesh-section-title">${sprite('audit')} ${escapeHtml(I18n.t('home.section_recent_events'))} <span class="section-count" id="events-count">0</span></div>
          <div class="card" style="padding: 0;">
            <div id="events-host">
              <div style="padding: 28px; text-align: center; color: var(--text-3); font-size: 13px;">
                ${escapeHtml(I18n.t('home.events_empty'))}
              </div>
            </div>
          </div>
        </div>
        <div>
          <div class="mesh-section-title">${sprite('flow')} ${escapeHtml(I18n.t('home.section_active_flows'))} <span class="section-count" id="active-flows-count">0</span></div>
          <div class="card" style="padding: 0;">
            <div id="active-flows-host">
              <div style="padding: 28px; text-align: center; color: var(--text-3); font-size: 13px;">
                ${escapeHtml(I18n.t('home.active_flows_empty'))}
              </div>
            </div>
          </div>
        </div>
      </div>
    `;
  },
  async mount() {
    byId('dash-refresh')?.addEventListener('click', () => refresh());

    // Animowana siatka network w hero — lazy import zeby nie opozniac
    // pierwszego paint'u ekranow ktore go nie maja.
    try {
      const heroCanvas = byId('hero-network');
      if (heroCanvas) {
        const { mount: mountHeroNetwork } = await import('/js/lib/hero-network.js');
        mountHeroNetwork(heroCanvas);
      }
    } catch (e) {
      console.warn('[dashboard] hero network mount failed', e);
    }

    await refresh();
    metricsRefresher = createRefresher({
      run: refresh,
      intervalMs: 5000,
      hiddenIntervalMs: 20000,
    });
    metricsRefresher.start();

    try {
      const client = await ApiBinary.client();
      auditUnsubscribe = client.addUnsolicitedListener(({ body }) => {
        if (body?.variant === 'AuditEvent') {
          recentEvents.unshift(body);
          if (recentEvents.length > MAX_RECENT_EVENTS) recentEvents.length = MAX_RECENT_EVENTS;
          renderEvents();
        }
      });
    } catch (e) {
      console.warn('[dashboard] audit subscribe failed', e);
    }
  },
  async unmount() {
    if (metricsRefresher) metricsRefresher.dispose();
    metricsRefresher = null;
    if (auditUnsubscribe) auditUnsubscribe();
    auditUnsubscribe = null;
    recentEvents.length = 0;
    try {
      const { unmount: unmountHeroNetwork } = await import('/js/lib/hero-network.js');
      unmountHeroNetwork();
    } catch { /* ignore */ }
  },
};

async function refresh() {
  const start = performance.now();
  try {
    const [metrics, nodes, services, flows, models] = await Promise.allSettled([
      ApiBinary.one('dashboardMetricsRequest'),
      ApiBinary.list('nodeListRequest'),
      ApiBinary.list('serviceListRequest'),
      ApiBinary.list('flowListRequest'),
      ApiBinary.list('modelListRequest'),
    ]);
    const rtt = Math.round(performance.now() - start);
    const rttEl = byId('metrics-rtt');
    if (rttEl) rttEl.textContent = I18n.t('home.section_metrics_rtt', { rtt });

    if (metrics.status === 'fulfilled') updateMetrics(metrics.value);
    if (nodes.status === 'fulfilled') updateMeshStat(nodes.value);
    if (flows.status === 'fulfilled') updateActiveFlow(flows.value);
    if (models.status === 'fulfilled') updateModelStat(models.value);
    if (services.status === 'fulfilled') {
      const m = byId('m-services');
      const fallback = services.value.filter((s) => s.status === 'running').length;
      const val = metrics.status === 'fulfilled' ? (metrics.value.activeServices ?? fallback) : fallback;
      if (m) m.textContent = String(val);
    }
  } catch (e) {
    console.error('[dashboard] refresh failed', e);
  }
}

function updateMetrics(m) {
  setText('m-tps', m.tokensPerSecond ?? 0);
  setText('m-active', m.activeRequests ?? 0);
  setText('m-errors', m.totalErrors ?? 0);
  setText('m-services', m.activeServices ?? 0);
  setText('m-total', m.totalRequests ?? 0);
  const cpu = (m.cpuUsagePercent ?? 0).toFixed(1);
  setText('m-cpu', `${cpu}%`);
  const used = ((Number(m.ramUsedMb ?? 0)) / 1024).toFixed(1);
  const total = ((Number(m.ramTotalMb ?? 0)) / 1024).toFixed(1);
  setText('m-ram', `${used} / ${total} GB`);
}

function updateMeshStat(nodes) {
  const total = nodes.length;
  const online = nodes.filter((n) => isOnlineHelper(n) || n.isSelf).length;
  setText('stat-mesh-value', I18n.t('home.stat_mesh_value', { online }));
  const offline = total - online;
  setText('stat-mesh-sub', offline > 0
    ? I18n.t('home.stat_mesh_sub', { total, offline })
    : I18n.t('home.stat_mesh_sub_all', { total }));
}

function updateModelStat(models) {
  if (!models || models.length === 0) {
    setText('stat-models-value', I18n.t('home.stat_models_empty'));
    setText('stat-models-sub', I18n.t('home.stat_models_empty_hint'));
    return;
  }
  const first = models[0];
  setText('stat-models-value', first.id ?? '—');
  setText('stat-models-sub', I18n.t('home.stat_models_sub', { count: models.length, engine: first.engineId ?? '' }));
}

function updateActiveFlow(flows) {
  if (!flows || flows.length === 0) {
    setText('stat-flow-value', I18n.t('home.stat_flow_empty'));
    setText('stat-flow-sub', I18n.t('home.stat_flow_empty_hint'));
    setText('active-flows-count', '0');
    return;
  }
  const enabled = flows.filter((f) => f.enabled);
  setText('active-flows-count', String(enabled.length));
  if (enabled.length === 0) {
    setText('stat-flow-value', I18n.t('home.stat_flow_inactive'));
    setText('stat-flow-sub', I18n.t('home.stat_flow_inactive_sub', { total: flows.length }));
    return;
  }
  const f = enabled[0];
  setText('stat-flow-value', f.name ?? f.id);
  setText('stat-flow-sub', I18n.t('home.stat_flow_active_sub', { date: formatDate(f.updatedAtEpoch) }));
  const host = byId('active-flows-host');
  if (!host) return;
  host.innerHTML = enabled.slice(0, 6).map((it) => `
    <div style="padding: 12px 16px; border-bottom: 1px solid var(--border); display: flex; align-items: center; gap: 10px;">
      <span style="width: 6px; height: 6px; border-radius: 50%; background: var(--success); box-shadow: 0 0 8px currentColor;"></span>
      <div style="flex: 1; min-width: 0;">
        <div style="font-size: 13px; font-weight: 600;">${escapeHtml(it.name ?? it.id)}</div>
        <div style="font-size: 11px; color: var(--text-3);">updated ${formatDate(it.updatedAtEpoch)}</div>
      </div>
    </div>
  `).join('');
}

function renderEvents() {
  const host = byId('events-host');
  const cnt = byId('events-count');
  if (!host) return;
  if (cnt) cnt.textContent = String(recentEvents.length);
  if (recentEvents.length === 0) {
    host.innerHTML = `<div style="padding: 28px; text-align: center; color: var(--text-3); font-size: 13px;">${escapeHtml(I18n.t('home.events_empty'))}</div>`;
    return;
  }
  host.innerHTML = recentEvents.map((e) => {
    const sev = inferSeverity(e.eventKind);
    return `
      <div style="padding: 10px 16px; border-bottom: 1px solid var(--border); display: grid; grid-template-columns: 110px 70px 1fr; gap: 10px; align-items: center;">
        <span style="color: var(--text-3); font-family: 'SF Mono', monospace; font-size: 11px;">${escapeHtml(formatDate(e.tsEpoch))}</span>
        <span class="audit-sev ${sev}">${escapeHtml(e.eventKind)}</span>
        <span style="font-size: 12px; color: var(--text);">${escapeHtml(e.message)}</span>
      </div>
    `;
  }).join('');
}

function inferSeverity(kind) {
  if (!kind) return 'info';
  const k = kind.toLowerCase();
  if (k.includes('error') || k.includes('fail') || k.includes('revoke')) return 'err';
  if (k.includes('warn') || k.includes('expire')) return 'warn';
  if (k.includes('login') || k.includes('create') || k.includes('deploy')) return 'ok';
  return 'info';
}

function setText(id, text) {
  const el = byId(id);
  if (el) el.textContent = text;
}

export default DashboardScreen;
