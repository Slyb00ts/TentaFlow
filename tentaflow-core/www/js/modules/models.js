// =============================================================================
// Plik: modules/models.js
// Opis: Lista modeli (R-LIST) + szczegoly (R-ONE) + delete (W-DELETE).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

let models = [];

const ModelsScreen = {
  title: 'Modele',

  render() {
    return `
      <div class="content-header">
        <h1>Modele</h1>
        <button class="btn" id="btn-refresh-models">Odśwież</button>
      </div>
      <div class="card" style="padding: 0;">
        <div id="models-table-host"></div>
      </div>
      <div id="model-detail-host"></div>
    `;
  },

  async mount() {
    byId('btn-refresh-models').addEventListener('click', load);
    await load();
  },

  unmount() {
    models = [];
  },
};

async function load() {
  try {
    models = await ApiBinary.list('modelListRequest');
    renderTable();
  } catch (err) {
    toast(`Błąd: ${err.message}`, 'error');
  }
}

function renderTable() {
  const host = byId('models-table-host');
  if (!host) return;
  if (models.length === 0) {
    host.innerHTML = `
      <div class="empty-state">
        <div class="empty-state-text">Brak modeli</div>
        <div class="empty-state-hint">Zainstaluj model przez Hub silników</div>
      </div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead>
        <tr>
          <th>ID</th>
          <th>Kategoria</th>
          <th>Silnik</th>
          <th>Status</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        ${models.map((m) => `
          <tr>
            <td><code>${escapeHtml(m.id)}</code></td>
            <td><span class="badge">${escapeHtml(m.category)}</span></td>
            <td>${escapeHtml(m.engineId)}</td>
            <td>${availabilityBadge(m.availability)}</td>
            <td>
              <button class="btn btn-sm" data-detail="${escapeHtml(m.id)}">Szczegóły</button>
              <button class="btn btn-sm btn-danger" data-delete="${escapeHtml(m.id)}">Usuń</button>
            </td>
          </tr>
        `).join('')}
      </tbody>
    </table>
  `;
  host.querySelectorAll('[data-detail]').forEach((b) => {
    b.addEventListener('click', () => showDetail(b.dataset.detail));
  });
  host.querySelectorAll('[data-delete]').forEach((b) => {
    b.addEventListener('click', () => deleteModel(b.dataset.delete));
  });
}

function availabilityBadge(s) {
  const cls = s === 'running' ? 'badge-success'
    : s === 'failed' || s === 'error' ? 'badge-error'
    : 'badge';
  return `<span class="badge ${cls}">${escapeHtml(s)}</span>`;
}

async function showDetail(modelId) {
  try {
    const d = await ApiBinary.one('modelDetailRequest', { modelId });
    const host = byId('model-detail-host');
    host.innerHTML = `
      <div class="card" style="margin-top: var(--space-4);">
        <div class="card-header">
          <h3 class="card-title">${escapeHtml(d.id)}</h3>
          <button class="btn btn-ghost btn-sm" id="close-detail">Zamknij</button>
        </div>
        <div class="form-row"><span class="label">Kategoria</span><div>${escapeHtml(d.category)}</div></div>
        <div class="form-row"><span class="label">Silnik</span><div>${escapeHtml(d.engineId)}</div></div>
        <div class="form-row"><span class="label">Status</span><div>${availabilityBadge(d.availability)}</div></div>
        <div class="form-row"><span class="label">Opis</span><div>${escapeHtml(d.description)}</div></div>
      </div>
    `;
    byId('close-detail').addEventListener('click', () => {
      host.innerHTML = '';
    });
  } catch (err) {
    toast(`Błąd: ${err.message}`, 'error');
  }
}

async function deleteModel(modelId) {
  if (!confirm(`Usunąć model "${modelId}"?`)) return;
  try {
    const r = await ApiBinary.action('modelDeleteRequest', { modelId });
    if (r.deleted) {
      toast('Model usunięty', 'success');
      await load();
    } else {
      toast('Model nie znaleziony', 'warning');
    }
  } catch (err) {
    toast(`Błąd: ${err.message}`, 'error');
  }
}

export default ModelsScreen;
