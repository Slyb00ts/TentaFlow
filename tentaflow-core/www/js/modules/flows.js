// =============================================================================
// Plik: modules/flows.js
// Opis: Lista przeplywów + create (otwiera builder) + edit (builder) + delete +
//       historia wykonan. Status chip kolorowy, akcje icon-only.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import {
  byId, escapeHtml, escapeAttr, toast, formatDate,
} from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';
import { openFlowBuilder } from '/js/modules/flows-builder.js';

let flows = [];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function statusChip(status) {
  const s = (status || '').toLowerCase();
  const label = s === 'active' ? 'Aktywny' : (s === 'archived' ? 'Archiwum' : 'Draft');
  const cls = s === 'active' ? 'active' : (s === 'archived' ? 'archived' : 'draft');
  return `<span class="flows-status-chip ${cls}">${escapeHtml(label)}</span>`;
}

const FlowsScreen = {
  title: 'Przeplywy',
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('flow')} Przeplywy</h1>
          <div class="sub">Graf DAG wykonywany przez flow_engine — LLM, embeddings, TTS, RAG.</div>
        </div>
        <div class="actions">
          <tf-button variant="primary" icon="plus" id="btn-new-flow">Nowy flow</tf-button>
        </div>
      </div>
      <div id="flows-host"></div>
      <div id="execs-host"></div>`;
  },
  async mount() {
    byId('btn-new-flow').addEventListener('click', () => newFlow());
    await load();
  },
  unmount() { flows = []; },
};

async function load() {
  try {
    flows = await ApiBinary.list('flowListRequest');
    renderTable();
  } catch (err) {
    toast(`Blad: ${err.message}`, 'error');
  }
}

function renderTable() {
  const host = byId('flows-host');
  if (!host) return;
  if (flows.length === 0) {
    host.innerHTML = `
      <div class="empty-big">
        ${sprite('flow')}
        <h3>Brak przeplywow</h3>
        <p>Utworz pierwszy przeplyw, aby zdefiniowac graf przetwarzania.</p>
        <tf-button variant="primary" icon="plus" id="empty-new-flow">Nowy flow</tf-button>
      </div>`;
    const btn = byId('empty-new-flow');
    if (btn) btn.addEventListener('click', () => newFlow());
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead>
        <tr>
          <th>Nazwa</th>
          <th>Opis</th>
          <th>Status</th>
          <th>Aktualizacja</th>
          <th style="text-align:right;">Akcje</th>
        </tr>
      </thead>
      <tbody>
        ${flows.map(renderRow).join('')}
      </tbody>
    </table>`;
  bindRowActions();
}

function renderRow(f) {
  const status = f.status || (f.enabled ? 'active' : 'draft');
  const updated = f.updatedAtEpoch || f.updated_at_epoch || f.updated_at;
  return `
    <tr data-key="flow-${escapeAttr(f.id)}">
      <td data-label="Nazwa"><strong style="color: var(--accent-2);">${escapeHtml(f.name)}</strong></td>
      <td data-label="Opis">${f.description ? escapeHtml(f.description) : '<span style="color:var(--text-3);">—</span>'}</td>
      <td data-label="Status">${statusChip(status)}</td>
      <td data-label="Aktualizacja" style="font-size:12px;color:var(--text-3);">${formatDate(updated)}</td>
      <td data-label="Akcje" style="text-align:right;">
        <tf-button variant="ghost" size="sm" icon="settings" data-flow-edit="${escapeAttr(f.id)}" title="Edytuj"></tf-button>
        <tf-button variant="ghost" size="sm" icon="clock" data-flow-execs="${escapeAttr(f.id)}" title="Historia wykonan"></tf-button>
        <tf-button variant="danger" size="sm" icon="trash" data-flow-delete="${escapeAttr(f.id)}" data-flow-name="${escapeAttr(f.name)}" title="Usun"></tf-button>
      </td>
    </tr>`;
}

function bindRowActions() {
  document.querySelectorAll('[data-flow-edit]').forEach((b) => {
    b.onclick = () => openFlowBuilder(b.dataset.flowEdit);
  });
  document.querySelectorAll('[data-flow-execs]').forEach((b) => {
    b.onclick = () => showExecs(b.dataset.flowExecs);
  });
  document.querySelectorAll('[data-flow-delete]').forEach((b) => {
    b.onclick = () => deleteFlow(b.dataset.flowDelete, b.dataset.flowName);
  });
}

async function newFlow() {
  try {
    const resp = await ApiBinary.action('flowCreateRequest', {
      name: 'Nowy flow',
      description: null,
      graphJson: '{"nodes":[],"edges":[]}',
    });
    const id = resp?.flowId ?? resp?.flow_id;
    if (!id) throw new Error('Brak ID w odpowiedzi API');
    openFlowBuilder(id);
  } catch (err) {
    toast(`Nie udało się utworzyć flow: ${err.message}`, 'error');
  }
}

async function deleteFlow(flowId, flowName) {
  const ok = await TfWindow.confirm({
    title: 'Usun przeplyw?',
    message: `Czy na pewno usunac przeplyw "${flowName}"?`,
    description: 'Operacja nieodwracalna — powiazane wykonania zostana zachowane.',
    confirmLabel: 'Usun',
    cancelLabel: 'Anuluj',
    danger: true,
  });
  if (!ok) return;
  try {
    const r = await ApiBinary.action('flowDeleteRequest', { flowId });
    if (r.deleted) {
      toast('Usunieto przeplyw', 'success');
      await load();
    } else {
      toast('Przeplyw nie znaleziony', 'warning');
    }
  } catch (err) {
    toast(`Blad: ${err.message}`, 'error');
  }
}

async function showExecs(flowId) {
  try {
    const resp = await ApiBinary.one('flowExecutionsListRequest', { flowId });
    const execs = resp.executions ?? [];
    const host = byId('execs-host');
    if (!host) return;
    host.innerHTML = `
      <div class="card" style="margin-top: var(--space-4);">
        <div class="card-header">
          <h3 class="card-title">Historia wykonan — flow ${escapeHtml(flowId)}</h3>
          <tf-button variant="ghost" size="sm" icon="x" id="execs-close" title="Zamknij"></tf-button>
        </div>
        ${execs.length === 0 ? `
          <div class="empty-state"><div class="empty-state-text">Brak wykonan</div></div>
        ` : `
          <table class="data-table">
            <thead><tr><th>ID</th><th>Status</th><th>Start</th><th>Koniec</th></tr></thead>
            <tbody>
              ${execs.map((e) => `
                <tr>
                  <td><code style="font-size:11px;">${escapeHtml(e.id)}</code></td>
                  <td><tf-chip status="${execStatus(e.status)}">${escapeHtml(e.status)}</tf-chip></td>
                  <td style="font-size:12px;color:var(--text-3);">${formatDate(e.startedAtEpoch)}</td>
                  <td style="font-size:12px;color:var(--text-3);">${e.completedAtEpoch ? formatDate(e.completedAtEpoch) : '—'}</td>
                </tr>`).join('')}
            </tbody>
          </table>`}
      </div>`;
    byId('execs-close')?.addEventListener('click', () => { host.innerHTML = ''; });
  } catch (err) {
    toast(`Blad: ${err.message}`, 'error');
  }
}

function execStatus(s) {
  const v = (s || '').toLowerCase();
  if (v === 'completed' || v === 'success' || v === 'ok') return 'ok';
  if (v === 'running' || v === 'pending') return 'info';
  return 'warn';
}

export default FlowsScreen;
