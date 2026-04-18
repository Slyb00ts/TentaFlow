// =============================================================================
// Plik: modules/mesh.js
// Opis: Lista mesh peers + pair init (admin).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, shortHex, formatRelative } from '/js/utils.js';

let peers = [];

const MeshScreen = {
  title: 'Peers mesh',
  render() {
    return `
      <div class="content-header">
        <h1>Peers mesh</h1>
        <button class="btn btn-primary" id="btn-pair">Połącz nowego peera</button>
      </div>
      <div class="card" style="padding: 0;">
        <div id="peers-host"></div>
      </div>`;
  },
  async mount() {
    byId('btn-pair').addEventListener('click', openPairModal);
    await load();
  },
  unmount() { peers = []; },
};

async function load() {
  try {
    peers = await ApiBinary.list('meshPeersListRequest');
    renderTable();
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function renderTable() {
  const host = byId('peers-host');
  if (!host) return;
  if (peers.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak peerów</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr><th>Hostname</th><th>Node ID</th><th>Status</th><th>Endpoint</th><th>Discovered</th></tr></thead>
      <tbody>
        ${peers.map((p) => `
          <tr>
            <td>${escapeHtml(p.displayName)}</td>
            <td><code>${shortHex(p.nodeId, 16)}…</code></td>
            <td><span class="badge badge-${p.trustState === 'connected' ? 'success' : 'warning'}">${escapeHtml(p.trustState)}</span></td>
            <td>${escapeHtml(p.endpoint ?? '—')}</td>
            <td>${p.lastSeenEpoch ? formatRelative(p.lastSeenEpoch) : '—'}</td>
          </tr>`).join('')}
      </tbody>
    </table>`;
}

function openPairModal() {
  document.body.insertAdjacentHTML('beforeend', `
    <div class="modal-backdrop" id="pair-modal">
      <div class="modal">
        <div class="modal-header"><h3 class="modal-title">Pair nowego peera</h3>
          <button class="btn btn-ghost btn-sm" id="pair-cancel">×</button></div>
        <div class="modal-body">
          <div class="form-row">
            <label class="label" for="pair-node-id">Node ID (hex, 64 znakow)</label>
            <input class="input" id="pair-node-id" placeholder="aabb...">
          </div>
          <div class="form-row">
            <label class="label" for="pair-pin">PIN (6 cyfr)</label>
            <input class="input" id="pair-pin" maxlength="6">
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" id="pair-cancel-btn">Anuluj</button>
          <button class="btn btn-primary" id="pair-submit">Połącz</button>
        </div>
      </div>
    </div>`);
  const close = () => byId('pair-modal')?.remove();
  byId('pair-cancel').addEventListener('click', close);
  byId('pair-cancel-btn').addEventListener('click', close);
  byId('pair-submit').addEventListener('click', async () => {
    const idHex = byId('pair-node-id').value.trim();
    const pin = byId('pair-pin').value.trim();
    if (idHex.length !== 64 || !pin.match(/^\d{6}$/)) {
      toast('Niepoprawne dane', 'error');
      return;
    }
    const nodeId = new Uint8Array(32);
    for (let i = 0; i < 32; i++) nodeId[i] = parseInt(idHex.substr(i * 2, 2), 16);
    try {
      const r = await ApiBinary.action('meshPairInitRequest', { nodeId, pin });
      toast(`Pair init: ${r.pairId}`, 'success');
      close();
      await load();
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  });
}

export default MeshScreen;
