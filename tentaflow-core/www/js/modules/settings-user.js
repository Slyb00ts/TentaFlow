// =============================================================================
// File: modules/settings-user.js — User-level preferences (language). Backed by
// I18n (localStorage-persisted) — no backend handler required.
// =============================================================================

import { I18n, SUPPORTED_LANGS } from '/js/i18n.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const SettingsUserScreen = {
  render() {
    const current = I18n.getLanguage();
    const options = SUPPORTED_LANGS.map((l) =>
      `<option value="${l.code}"${l.code === current ? ' selected' : ''}>${l.flag} ${escapeHtml(l.label)}</option>`
    ).join('');
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('settings')} ${escapeHtml(I18n.t('settings_user.title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('settings_user.subtitle'))}</div>
        </div>
      </div>
      <div class="card">
        <div class="card-header"><h3 class="card-title">${escapeHtml(I18n.t('settings_user.language_title'))}</h3></div>
        <div class="form-row">
          <span class="label">${escapeHtml(I18n.t('lang.label'))}</span>
          <div><tf-select id="settings-user-lang">${options}</tf-select></div>
        </div>
        <div class="form-hint">${escapeHtml(I18n.t('settings_user.language_hint'))}</div>
      </div>`;
  },
  mount() {
    const sel = byId('settings-user-lang');
    sel?.addEventListener('change', async (e) => {
      try {
        await I18n.setLanguage(e.target.value);
      } catch (err) {
        toast(`${I18n.t('settings_user.save_error')}: ${err.message}`, 'error');
      }
    });
  },
  unmount() {},
};

export default SettingsUserScreen;
