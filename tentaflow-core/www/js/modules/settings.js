// =============================================================================
// Plik: modules/settings.js
// Opis: Lista ustawien + bulk update. Uzywa komponentow tf-button, tf-input, tf-chip.
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
        <tf-button variant="primary" id="btn-save-settings" disabled label="Zapisz zmiany"></tf-button>
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
  host.innerHTML = entries.map((e) => {
    const keyEsc = escapeHtml(e.key);
    const type = e.isSecret ? 'password' : 'text';
    const value = e.isSecret ? '' : escapeHtml(e.value);
    const placeholder = e.isSecret ? '<redacted> — wpisz, aby zaktualizować' : '';
    if (e.isSecret) {
      return `
        <div class="form-row">
          <tf-input
            id="set-${keyEsc}"
            type="${type}"
            value="${value}"
            placeholder="${placeholder}"
            data-key="${keyEsc}"
            data-secret="true"><span slot="label">${keyEsc} <tf-chip status="warn">secret</tf-chip></span></tf-input>
        </div>
      `;
    }
    return `
      <div class="form-row">
        <tf-input
          id="set-${keyEsc}"
          type="${type}"
          label="${keyEsc}"
          value="${value}"
          placeholder="${placeholder}"
          data-key="${keyEsc}"
          data-secret="false"></tf-input>
      </div>
    `;
  }).join('');
  host.querySelectorAll('tf-input[data-key]').forEach((input) => {
    input.addEventListener('input', (ev) => {
      const val = ev.detail?.value ?? input.value;
      dirty.set(input.dataset.key, {
        key: input.dataset.key,
        value: val,
        isSecret: input.dataset.secret === 'true',
      });
      const btn = byId('btn-save-settings');
      if (dirty.size === 0) btn.setAttribute('disabled', '');
      else btn.removeAttribute('disabled');
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
    byId('btn-save-settings').setAttribute('disabled', '');
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

export default SettingsScreen;
