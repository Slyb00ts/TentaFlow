// =============================================================================
// Plik: modules/apikeys.js
// Opis: Lista + create + revoke kluczy API.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate, formatRelative } from '/js/utils.js';

let keys = [];

const ApiKeysScreen = {
  title: 'Klucze API',
  render() {
    return `
      <div class="content-header">
        <h1>Klucze API</h1>
        <button class="btn btn-primary" id="btn-create-key">Utwórz klucz</button>
      </div>
      <div class="card" style="padding: 0;"><div id="keys-host"></div></div>`;
  },
  async mount() {
    byId('btn-create-key').addEventListener('click', openCreateModal);
    await load();
  },
  unmount() { keys = []; },
};

async function load() {
  try {
    keys = await ApiBinary.list('apiKeyListRequest');
    renderTable();
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function renderTable() {
  const host = byId('keys-host');
  if (keys.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak kluczy API</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr><th>ID</th><th>Nazwa</th><th>Utworzono</th><th>Ostatnio użyty</th><th></th></tr></thead>
      <tbody>${keys.map((k) => `<tr>
        <td><code>${escapeHtml(k.keyId)}</code></td>
        <td>${escapeHtml(k.name)}</td>
        <td>${formatDate(k.createdAtEpoch)}</td>
        <td>${k.lastUsedAtEpoch ? formatRelative(k.lastUsedAtEpoch) : '—'}</td>
        <td><button class="btn btn-sm btn-danger" data-revoke="${escapeHtml(k.keyId)}">Usuń</button></td>
      </tr>`).join('')}</tbody>
    </table>`;
  host.querySelectorAll('[data-revoke]').forEach((b) => {
    b.addEventListener('click', () => revoke(b.dataset.revoke));
  });
}

async function revoke(keyId) {
  if (!confirm(`Usunąć klucz ${keyId}?`)) return;
  try {
    const r = await ApiBinary.action('apiKeyRevokeRequest', { keyId });
    if (r.deleted) { toast('Usunięto', 'success'); await load(); }
    else { toast('Nie znaleziono', 'warning'); }
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function openCreateModal() {
  document.body.insertAdjacentHTML('beforeend', `
    <div class="modal-backdrop" id="key-modal">
      <div class="modal">
        <div class="modal-header"><h3 class="modal-title">Nowy klucz API</h3>
          <button class="btn btn-ghost btn-sm" id="k-x">×</button></div>
        <div class="modal-body">
          <div class="form-row">
            <label class="label" for="k-name">Nazwa</label>
            <input class="input" id="k-name" placeholder="np. CI Pipeline">
          </div>
          <div id="k-result" style="display: none; margin-top: var(--space-4);">
            <div class="label">Skopiuj klucz — będzie widoczny tylko teraz!</div>
            <pre id="k-result-token" style="background: var(--color-bg); padding: var(--space-3); border-radius: var(--radius-md); border: 1px solid var(--color-border); word-break: break-all; user-select: all;"></pre>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" id="k-close">Zamknij</button>
          <button class="btn btn-primary" id="k-create">Utwórz</button>
        </div>
      </div>
    </div>`);
  const close = () => byId('key-modal')?.remove();
  byId('k-x').addEventListener('click', close);
  byId('k-close').addEventListener('click', () => { close(); load(); });
  byId('k-create').addEventListener('click', async () => {
    const name = byId('k-name').value.trim();
    if (!name) { toast('Nazwa wymagana', 'warning'); return; }
    try {
      const r = await ApiBinary.action('apiKeyCreateRequest', { name, scopes: [] });
      byId('k-result').style.display = 'block';
      byId('k-result-token').textContent = r.token;
      byId('k-create').disabled = true;
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  });
}

export default ApiKeysScreen;
