// =============================================================================
// Plik: modules/apikeys.js
// Opis: Lista + create + revoke kluczy API. Uzywa tf-window, tf-button, tf-input.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast, formatDate, formatRelative } from '/js/utils.js';
import { TfWindow } from '/js/components/tf-window.js';
import { I18n } from '/js/i18n.js';

let keys = [];

const ApiKeysScreen = {
  get title() { return I18n.t('apikeys.title'); },
  render() {
    return `
      <div class="content-header">
        <h1>${escapeHtml(I18n.t('apikeys.title'))}</h1>
        <tf-button variant="primary" id="btn-create-key" label="${escapeHtml(I18n.t('apikeys.create_key'))}"></tf-button>
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
  } catch (err) { toast(`${I18n.t('apikeys.error_prefix')}: ${err.message}`, 'error'); }
}

function renderTable() {
  const host = byId('keys-host');
  if (keys.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('apikeys.empty'))}</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr>
        <th>${escapeHtml(I18n.t('apikeys.col_id'))}</th>
        <th>${escapeHtml(I18n.t('apikeys.col_name'))}</th>
        <th>${escapeHtml(I18n.t('apikeys.col_created'))}</th>
        <th>${escapeHtml(I18n.t('apikeys.col_last_used'))}</th>
        <th></th>
      </tr></thead>
      <tbody>${keys.map((k) => `<tr>
        <td><code>${escapeHtml(k.keyId)}</code></td>
        <td>${escapeHtml(k.name)}</td>
        <td>${formatDate(k.createdAtEpoch)}</td>
        <td>${k.lastUsedAtEpoch ? formatRelative(k.lastUsedAtEpoch) : '—'}</td>
        <td><tf-button variant="danger" size="sm" icon="trash" data-revoke="${escapeHtml(k.keyId)}" title="${escapeHtml(I18n.t('apikeys.delete_title'))}"></tf-button></td>
      </tr>`).join('')}</tbody>
    </table>`;
  host.querySelectorAll('[data-revoke]').forEach((b) => {
    b.addEventListener('click', () => revoke(b.dataset.revoke));
  });
}

async function revoke(keyId) {
  const ok = await TfWindow.confirm({
    title: I18n.t('apikeys.delete_confirm_title'),
    message: I18n.t('apikeys.delete_confirm_msg', { keyId }),
    confirmLabel: I18n.t('apikeys.delete_title'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    const r = await ApiBinary.action('apiKeyRevokeRequest', { keyId });
    if (r.deleted) { toast(I18n.t('apikeys.deleted_ok'), 'success'); await load(); }
    else { toast(I18n.t('apikeys.not_found'), 'warning'); }
  } catch (err) { toast(`${I18n.t('apikeys.error_prefix')}: ${err.message}`, 'error'); }
}

function openCreateModal() {
  // Body okna — tf-input dla nazwy + kontener na wynik (token)
  const bodyEl = document.createElement('div');
  bodyEl.innerHTML = `
    <div class="form-row">
      <tf-input id="k-name" label="${escapeHtml(I18n.t('apikeys.name_label'))}" placeholder="${escapeHtml(I18n.t('apikeys.name_placeholder_ci'))}" autofocus></tf-input>
    </div>
    <div id="k-result" style="display: none; margin-top: var(--space-4);">
      <div class="tf-label">${escapeHtml(I18n.t('apikeys.copy_hint'))}</div>
      <pre id="k-result-token" style="background: var(--color-bg); padding: var(--space-3); border-radius: var(--radius-md); border: 1px solid var(--color-border); word-break: break-all; user-select: all;"></pre>
    </div>
  `;

  const footerEl = document.createElement('div');
  footerEl.innerHTML = `
    <tf-button variant="ghost" data-action="close" label="${escapeHtml(I18n.t('apikeys.close'))}"></tf-button>
    <tf-button variant="primary" data-action="create" label="${escapeHtml(I18n.t('apikeys.create_btn'))}" id="k-create-btn"></tf-button>
  `;

  // Recznie tworzymy okno (nie uzywamy TfWindow.open bo potrzebujemy nie zamykac
  // okna po akcji "create" — serwer zwraca token ktory musi zobaczyc uzytkownik).
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('apikeys.new_key'));
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
      if (!name) { toast(I18n.t('apikeys.name_required_short'), 'warning'); return; }
      try {
        const r = await ApiBinary.action('apiKeyCreateRequest', { name, scopes: [] });
        const resultBox = win.querySelector('#k-result');
        const resultToken = win.querySelector('#k-result-token');
        resultBox.style.display = 'block';
        resultToken.textContent = r.token;
        const createBtn = win.querySelector('#k-create-btn');
        if (createBtn) createBtn.setAttribute('disabled', '');
      } catch (err) { toast(`${I18n.t('apikeys.error_prefix')}: ${err.message}`, 'error'); }
    }
  });
}

export default ApiKeysScreen;
