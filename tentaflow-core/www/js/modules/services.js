// =============================================================================
// Plik: modules/services.js
// Opis: Lista serwisow + stop (admin).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate } from '/js/utils.js';

let services = [];

const ServicesScreen = {
  title: 'Serwisy',
  render() {
    return `
      <div class="content-header">
        <h1>Serwisy</h1>
        <button class="btn" id="btn-refresh">Odśwież</button>
      </div>
      <div class="card" style="padding: 0;">
        <div id="services-host"></div>
      </div>`;
  },
  async mount() {
    byId('btn-refresh').addEventListener('click', load);
    await load();
  },
  unmount() { services = []; },
};

async function load() {
  try {
    services = await ApiBinary.list('serviceListRequest');
    renderTable();
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function renderTable() {
  const host = byId('services-host');
  if (!host) return;
  if (services.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak serwisów</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr>
        <th>Silnik</th><th>Model</th><th>Status</th><th>Strategia</th><th>Uruchomiono</th><th></th>
      </tr></thead>
      <tbody>
        ${services.map((s) => `
          <tr>
            <td>${escapeHtml(s.engineId)}</td>
            <td><code>${escapeHtml(s.modelId)}</code></td>
            <td><span class="badge badge-${s.status === 'running' ? 'success' : 'warning'}">${escapeHtml(s.status)}</span></td>
            <td>${escapeHtml(s.deployMethod)}</td>
            <td>${s.startedAtEpoch ? formatDate(s.startedAtEpoch) : '—'}</td>
            <td><button class="btn btn-sm btn-danger" data-stop="${escapeHtml(s.id)}">Stop</button></td>
          </tr>`).join('')}
      </tbody>
    </table>`;
  host.querySelectorAll('[data-stop]').forEach((b) => {
    b.addEventListener('click', () => stop(b.dataset.stop));
  });
}

async function stop(serviceId) {
  if (!confirm('Zatrzymać serwis?')) return;
  try {
    const r = await ApiBinary.action('serviceStopRequest', { serviceId });
    if (r.stopped) { toast('Zatrzymano', 'success'); await load(); }
    else { toast('Serwis nie znaleziony', 'warning'); }
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

export default ServicesScreen;
