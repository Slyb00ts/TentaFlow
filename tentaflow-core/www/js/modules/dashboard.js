// =============================================================================
// Plik: modules/dashboard.js
// Opis: Dashboard z metrykami (DashboardMetricsRequest co 2s).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml } from '/js/utils.js';

let pollTimer = null;

const DashboardScreen = {
  title: 'Dashboard',

  render() {
    return `
      <div class="content-header">
        <h1>Dashboard</h1>
        <span class="badge">Live</span>
      </div>
      <div class="metric-grid">
        <div class="metric-card">
          <div class="metric-label">CPU</div>
          <div class="metric-value accent" id="m-cpu">—</div>
          <div class="metric-sub">Local node</div>
        </div>
        <div class="metric-card">
          <div class="metric-label">RAM</div>
          <div class="metric-value" id="m-ram">—</div>
          <div class="metric-sub" id="m-ram-sub">— / —</div>
        </div>
        <div class="metric-card">
          <div class="metric-label">Tokens / s</div>
          <div class="metric-value accent" id="m-tps">0</div>
          <div class="metric-sub">Output</div>
        </div>
        <div class="metric-card">
          <div class="metric-label">Aktywne requesty</div>
          <div class="metric-value" id="m-active">0</div>
          <div class="metric-sub">In-flight</div>
        </div>
        <div class="metric-card">
          <div class="metric-label">Wszystkie requesty</div>
          <div class="metric-value" id="m-total">0</div>
          <div class="metric-sub">Lifetime</div>
        </div>
        <div class="metric-card">
          <div class="metric-label">Błędy</div>
          <div class="metric-value" id="m-errors" style="color: var(--color-error);">0</div>
          <div class="metric-sub">Lifetime</div>
        </div>
        <div class="metric-card">
          <div class="metric-label">Aktywne serwisy</div>
          <div class="metric-value" id="m-services">0</div>
          <div class="metric-sub">Running</div>
        </div>
      </div>
    `;
  },

  async mount() {
    await refresh();
    pollTimer = setInterval(refresh, 2000);
  },

  unmount() {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
  },
};

async function refresh() {
  try {
    const m = await ApiBinary.one('dashboardMetricsRequest');
    setText('m-cpu', `${(m.cpuUsagePercent ?? 0).toFixed(1)}%`);
    const ramUsed = Number(m.ramUsedMb ?? 0n);
    const ramTotal = Number(m.ramTotalMb ?? 0n);
    setText('m-ram', `${(ramUsed / 1024).toFixed(1)} GB`);
    setText('m-ram-sub', `${ramUsed} MB / ${ramTotal} MB`);
    setText('m-tps', String(m.tokensPerSecond ?? 0));
    setText('m-active', String(m.activeRequests ?? 0));
    setText('m-total', String(m.totalRequests ?? 0));
    setText('m-errors', String(m.totalErrors ?? 0));
    setText('m-services', String(m.activeServices ?? 0));
  } catch (err) {
    console.error('[dashboard] refresh failed', err);
  }
}

function setText(id, txt) {
  const el = byId(id);
  if (el) el.textContent = txt;
}

export default DashboardScreen;
