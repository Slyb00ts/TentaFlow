// =============================================================================
// Plik: modules/flows.js
// Opis: Lista flows + create/delete + executions list.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate } from '/js/utils.js';

let flows = [];

const FlowsScreen = {
  title: 'Flows',
  render() {
    return `
      <div class="content-header">
        <h1>Flows</h1>
        <button class="btn btn-primary" id="btn-new-flow">Nowy flow</button>
      </div>
      <div class="card" style="padding: 0;">
        <div id="flows-host"></div>
      </div>`;
  },
  async mount() {
    byId('btn-new-flow').addEventListener('click', openCreateModal);
    await load();
  },
  unmount() { flows = []; },
};

async function load() {
  try {
    flows = await ApiBinary.list('flowListRequest');
    renderTable();
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function renderTable() {
  const host = byId('flows-host');
  if (!host) return;
  if (flows.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak flows</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr><th>Nazwa</th><th>Opis</th><th>Status</th><th>Aktualizacja</th><th></th></tr></thead>
      <tbody>
        ${flows.map((f) => `
          <tr>
            <td>${escapeHtml(f.name)}</td>
            <td>${escapeHtml(f.description ?? '')}</td>
            <td><span class="badge badge-${f.enabled ? 'success' : 'warning'}">${f.enabled ? 'aktywny' : 'wyłączony'}</span></td>
            <td>${formatDate(f.updatedAtEpoch)}</td>
            <td>
              <button class="btn btn-sm" data-execs="${escapeHtml(f.id)}">Wykonania</button>
              <button class="btn btn-sm btn-danger" data-delete="${escapeHtml(f.id)}">Usuń</button>
            </td>
          </tr>`).join('')}
      </tbody>
    </table>
    <div id="execs-host"></div>`;
  host.querySelectorAll('[data-delete]').forEach((b) => {
    b.addEventListener('click', () => deleteFlow(b.dataset.delete));
  });
  host.querySelectorAll('[data-execs]').forEach((b) => {
    b.addEventListener('click', () => showExecs(b.dataset.execs));
  });
}

async function deleteFlow(flowId) {
  if (!confirm('Usunąć flow?')) return;
  try {
    const r = await ApiBinary.action('flowDeleteRequest', { flowId });
    if (r.deleted) { toast('Usunięto', 'success'); await load(); }
    else { toast('Flow nie znaleziony', 'warning'); }
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

async function showExecs(flowId) {
  try {
    const execs = await ApiBinary.list('flowExecutionsListRequest', { arrayKey: 'executions' });
    const host = byId('execs-host');
    host.innerHTML = `
      <div class="card" style="margin-top: var(--space-4);">
        <h3 class="card-title">Wykonania flow ${escapeHtml(flowId)}</h3>
        ${execs.length === 0 ? '<p>Brak wykonań</p>' : `
          <table class="data-table">
            <thead><tr><th>ID</th><th>Status</th><th>Start</th><th>Koniec</th></tr></thead>
            <tbody>
              ${execs.map((e) => `<tr>
                <td>${escapeHtml(e.id)}</td>
                <td>${escapeHtml(e.status)}</td>
                <td>${formatDate(e.startedAtEpoch)}</td>
                <td>${e.completedAtEpoch ? formatDate(e.completedAtEpoch) : '—'}</td>
              </tr>`).join('')}
            </tbody>
          </table>`}
      </div>`;
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function openCreateModal() {
  document.body.insertAdjacentHTML('beforeend', `
    <div class="modal-backdrop" id="flow-modal">
      <div class="modal modal-lg">
        <div class="modal-header"><h3 class="modal-title">Nowy flow</h3>
          <button class="btn btn-ghost btn-sm" id="fl-x">×</button></div>
        <div class="modal-body">
          <div class="form-row"><label class="label" for="fl-name">Nazwa</label>
            <input class="input" id="fl-name"></div>
          <div class="form-row"><label class="label" for="fl-desc">Opis</label>
            <input class="input" id="fl-desc"></div>
          <div class="form-row"><label class="label" for="fl-graph">Graph JSON</label>
            <textarea class="textarea" id="fl-graph" rows="10">{
  "nodes": [],
  "edges": []
}</textarea></div>
        </div>
        <div class="modal-footer">
          <button class="btn" id="fl-cancel">Anuluj</button>
          <button class="btn btn-primary" id="fl-create">Utwórz</button>
        </div>
      </div>
    </div>`);
  const close = () => byId('flow-modal')?.remove();
  byId('fl-x').addEventListener('click', close);
  byId('fl-cancel').addEventListener('click', close);
  byId('fl-create').addEventListener('click', async () => {
    const name = byId('fl-name').value.trim();
    const description = byId('fl-desc').value.trim() || null;
    const graphJson = byId('fl-graph').value.trim();
    if (!name) { toast('Nazwa wymagana', 'warning'); return; }
    try {
      JSON.parse(graphJson);
    } catch { toast('graph musi byc poprawnym JSON', 'error'); return; }
    try {
      const r = await ApiBinary.action('flowCreateRequest', { name, description, graphJson });
      toast(`Utworzono: ${r.flowId}`, 'success');
      close();
      await load();
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  });
}

export default FlowsScreen;
