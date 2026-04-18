// =============================================================================
// Plik: modules/dashboard/HeartbeatBinary.js
// Opis: Connection heartbeat indicator widget — pokazuje status WS connection
//       i RTT do serwera. Uzywa MetaHeartbeat variantu.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const HeartbeatBinary = (() => {
  'use strict';
  let pingTimer = null;

  async function ping() {
    const now = Math.floor(Date.now() / 1000);
    try {
      const start = performance.now();
      const body = await ApiBinary.one('metaHeartbeat', now);
      const rtt = Math.round(performance.now() - start);
      renderStatus('connected', rtt);
    } catch (err) {
      renderStatus('disconnected', null);
    }
  }

  function renderStatus(status, rttMs) {
    const dot = document.getElementById('connection-dot');
    const text = document.getElementById('connection-text');
    if (dot) dot.className = `connection-dot ${status}`;
    if (text) text.textContent = rttMs !== null ? `${rttMs}ms` : 'offline';
  }

  return {
    mount: () => {
      ping();
      pingTimer = setInterval(ping, 5000);
    },
    unmount: () => {
      if (pingTimer) clearInterval(pingTimer);
      pingTimer = null;
    },
  };
})();

export default HeartbeatBinary;
