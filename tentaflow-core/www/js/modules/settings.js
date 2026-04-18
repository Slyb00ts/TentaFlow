// =============================================================================
// Plik: modules/settings.js
// Opis: Lista ustawien + bulk update.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

let entries = [];
let dirty = new Map();

const SettingsScreen = {
  title: 'Ustawienia',
  render() {
    return `
      <div class="content-header">
        <h1>Ustawienia</h1>
        <button class="btn btn-primary" id="btn-save-settings" disabled>Zapisz zmiany</button>
      </div>
      <div class="card" id="settings-host"></div>`;
  },
  async mount() {
    byId('btn-save-settings').addEventListener('click', save);
    try {
      entries = await ApiBinary.list('settingsListRequest', { arrayKey: 'entries' });
      renderForm();
    } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
  },
  unmount() { entries = []; dirty.clear(); },
};

function renderForm() {
  const host = byId('settings-host');
  if (entries.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak ustawień</div></div>`;
    return;
  }
  host.innerHTML = entries.map((e) => `
    <div class="form-row">
      <label class="label" for="set-${escapeHtml(e.key)}">
        ${escapeHtml(e.key)}
        ${e.isSecret ? '<span class="badge badge-warning" style="margin-left: var(--space-2);">secret</span>' : ''}
      </label>
      <input class="input" id="set-${escapeHtml(e.key)}"
        type="${e.isSecret ? 'password' : 'text'}"
        value="${e.isSecret ? '' : escapeHtml(e.value)}"
        placeholder="${e.isSecret ? '<redacted> — wpisz, aby zaktualizować' : ''}"
        data-key="${escapeHtml(e.key)}"
        data-secret="${e.isSecret}">
    </div>
  `).join('');
  host.querySelectorAll('input[data-key]').forEach((input) => {
    input.addEventListener('input', () => {
      dirty.set(input.dataset.key, {
        key: input.dataset.key,
        value: input.value,
        isSecret: input.dataset.secret === 'true',
      });
      byId('btn-save-settings').disabled = dirty.size === 0;
    });
  });
}

async function save() {
  if (dirty.size === 0) return;
  const updates = Array.from(dirty.values()).filter((u) => !(u.isSecret && u.value === ''));
  if (updates.length === 0) {
    toast('Brak zmian do zapisania', 'warning');
    return;
  }
  try {
    const r = await ApiBinary.action('settingsUpdateRequest', { entries: updates });
    toast(`Zapisano ${r.applied} ustawień`, 'success');
    dirty.clear();
    byId('btn-save-settings').disabled = true;
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

export default SettingsScreen;
