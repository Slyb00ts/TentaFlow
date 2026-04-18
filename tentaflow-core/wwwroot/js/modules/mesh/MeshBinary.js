// =============================================================================
// Plik: modules/mesh/MeshBinary.js
// Opis: Mesh peers ekran zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const MeshBinary = (() => {
  'use strict';
  let peersList = [];

  async function loadPeers() {
    try {
      peersList = await ApiBinary.list('meshPeersListRequest');
      renderTable();
    } catch (err) {
      console.error('[mesh-binary] load failed:', err);
      peersList = [];
      renderTable();
    }
  }

  function renderTable() {
    const tbody = document.getElementById('mesh-peers-tbody');
    if (!tbody) return;
    tbody.innerHTML = peersList.length === 0
      ? `<tr><td colspan="4"><div class="empty-state"><div class="empty-state-text">${I18n.t('mesh.empty')}</div></div></td></tr>`
      : peersList.map(p => `
          <tr>
            <td>${Utils.escapeHtml(p.displayName)}</td>
            <td><code>${bytesToHex(p.nodeId).slice(0, 16)}...</code></td>
            <td><span class="badge">${p.trustState}</span></td>
            <td>${p.endpoint ?? '-'}</td>
          </tr>
        `).join('');
  }

  function bytesToHex(bytes) {
    return Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('');
  }

  return {
    mount: () => loadPeers(),
    unmount: () => { peersList = []; },
  };
})();

export default MeshBinary;
