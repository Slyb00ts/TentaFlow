// =============================================================================
// Plik: modules/dashboard/DashboardBinary.js
// Opis: Dashboard ekran zmigrowany na binary protocol (Task #37 demo).
//       Polls DashboardMetricsRequest co 1s — w phase 2 zamiast pollingu uzyje
//       subscription stream gdy DashboardMetrics bedzie streaming variant.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const DashboardBinary = (() => {
  'use strict';

  let pollTimer = null;

  async function refreshMetrics() {
    try {
      const body = await ApiBinary.one('dashboardMetricsRequest');
      // body.variant === 'DashboardMetricsResponse' z polami z snapshot
      renderMetrics(body);
    } catch (err) {
      console.error('[dashboard-binary] refresh failed:', err);
    }
  }

  function renderMetrics(snap) {
    setText('metric-cpu', `${snap.cpuUsagePercent?.toFixed(1) ?? 0}%`);
    setText('metric-ram', `${snap.ramUsedMb} / ${snap.ramTotalMb} MB`);
    setText('metric-active', String(snap.activeRequests ?? 0));
    setText('metric-total', String(snap.totalRequests ?? 0));
    setText('metric-errors', String(snap.totalErrors ?? 0));
    setText('metric-tps', `${snap.tokensPerSecond ?? 0} t/s`);
    setText('metric-services', String(snap.activeServices ?? 0));
  }

  function setText(id, value) {
    const el = document.getElementById(id);
    if (el) el.textContent = value;
  }

  return {
    mount: () => {
      refreshMetrics();
      pollTimer = setInterval(refreshMetrics, 1000);
    },
    unmount: () => {
      if (pollTimer) {
        clearInterval(pollTimer);
        pollTimer = null;
      }
    },
  };
})();

export default DashboardBinary;
