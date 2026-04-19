// =============================================================================
// Plik: modules/apikeys.js
// Opis: Lista + create + revoke kluczy API. Uzywa tf-window, tf-button, tf-input.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate, formatRelative } from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';

let keys = [];

const ApiKeysScreen = {
  title: 'Klucze API',
  render() {
    return `
      <div class="content-header">
        <h1>Klucze API</h1>
        <tf-button variant="primary" id="btn-create-key" label="Utwórz klucz"></tf-button>
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
        <td><tf-button variant="danger" size="sm" data-revoke="${escapeHtml(k.keyId)}" label="Usuń"></tf-button></td>
      </tr>`).join('')}</tbody>
    </table>`;
  host.querySelectorAll('[data-revoke]').forEach((b) => {
    b.addEventListener('click', () => revoke(b.dataset.revoke));
  });
}

async function revoke(keyId) {
  const ok = await TfWindow.confirm({
    title: 'Usuń klucz API',
    message: `Usunąć klucz ${keyId}?`,
    confirmLabel: 'Usuń',
    cancelLabel: 'Anuluj',
    danger: true,
  });
  if (!ok) return;
  try {
    const r = await ApiBinary.action('apiKeyRevokeRequest', { keyId });
    if (r.deleted) { toast('Usunięto', 'success'); await load(); }
    else { toast('Nie znaleziono', 'warning'); }
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

function openCreateModal() {
  // Body okna — tf-input dla nazwy + kontener na wynik (token)
  const bodyEl = document.createElement('div');
  bodyEl.innerHTML = `
    <div class="form-row">
      <tf-input id="k-name" label="Nazwa" placeholder="np. CI Pipeline" autofocus></tf-input>
    </div>
    <div id="k-result" style="display: none; margin-top: var(--space-4);">
      <div class="tf-label">Skopiuj klucz — będzie widoczny tylko teraz!</div>
      <pre id="k-result-token" style="background: var(--color-bg); padding: var(--space-3); border-radius: var(--radius-md); border: 1px solid var(--color-border); word-break: break-all; user-select: all;"></pre>
    </div>
  `;

  const footerEl = document.createElement('div');
  footerEl.innerHTML = `
    <tf-button variant="ghost" data-action="close" label="Zamknij"></tf-button>
    <tf-button variant="primary" data-action="create" label="Utwórz" id="k-create-btn"></tf-button>
  `;

  // Recznie tworzymy okno (nie uzywamy TfWindow.open bo potrzebujemy nie zamykac
  // okna po akcji "create" — serwer zwraca token ktory musi zobaczyc uzytkownik).
  const win = document.createElement('tf-window');
  win.setAttribute('title', 'Nowy klucz API');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '420');
  win.setAttribute('min-height', '220');
  win.setAttribute('width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const bodyWrap = document.createElement('div');
  bodyWrap.slot = 'body';
  bodyWrap.appendChild(bodyEl);
  win.appendChild(bodyWrap);

  const footWrap = document.createElement('div');
  footWrap.slot = 'footer';
  footWrap.appendChild(footerEl);
  win.appendChild(footWrap);

  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.appendChild(backdrop);
  document.body.appendChild(win);

  const cleanup = () => {
    if (win.isConnected) win.remove();
    if (backdrop.isConnected) backdrop.remove();
    load();
  };

  win.addEventListener('action', async (e) => {
    const action = e.detail?.action;
    if (action === 'close') {
      cleanup();
      return;
    }
    if (action === 'create') {
      const nameInput = win.querySelector('#k-name');
      const name = (nameInput?.value || '').trim();
      if (!name) { toast('Nazwa wymagana', 'warning'); return; }
      try {
        const r = await ApiBinary.action('apiKeyCreateRequest', { name, scopes: [] });
        const resultBox = win.querySelector('#k-result');
        const resultToken = win.querySelector('#k-result-token');
        resultBox.style.display = 'block';
        resultToken.textContent = r.token;
        const createBtn = win.querySelector('#k-create-btn');
        if (createBtn) createBtn.setAttribute('disabled', '');
      } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
    }
  });
}

export default ApiKeysScreen;
