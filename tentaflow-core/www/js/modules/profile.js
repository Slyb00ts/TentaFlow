// =============================================================================
// File: modules/profile.js — Read-only user profile page. Data source:
// AuthMeRequest (real backend handler, returns {username, role, ...}).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const ProfileScreen = {
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('user')} ${escapeHtml(I18n.t('profile.title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('profile.subtitle'))}</div>
        </div>
      </div>
      <div class="card" id="profile-card"><div class="empty-state-small">${escapeHtml(I18n.t('profile.loading'))}</div></div>`;
  },
  async mount() {
    try {
      const me = await ApiBinary.one('authMeRequest');
      const card = byId('profile-card');
      const initials = (me?.username ?? '?').slice(0, 2).toUpperCase();
      const roleKey = (me?.role ?? 'user').toLowerCase() === 'admin' ? 'role.administrator' : 'role.user';
      card.innerHTML = `
        <div class="profile-header">
          <div class="profile-avatar">${escapeHtml(initials)}</div>
          <div class="profile-identity">
            <div class="profile-name">${escapeHtml(me?.username ?? '—')}</div>
            <div class="profile-role">${escapeHtml(I18n.t(roleKey))}</div>
          </div>
        </div>
        <div class="form-row"><span class="label">${escapeHtml(I18n.t('profile.username'))}</span><div>${escapeHtml(me?.username ?? '—')}</div></div>
        <div class="form-row"><span class="label">${escapeHtml(I18n.t('profile.role'))}</span><div>${escapeHtml(I18n.t(roleKey))}</div></div>
        ${me?.userId ? `<div class="form-row"><span class="label">${escapeHtml(I18n.t('profile.user_id'))}</span><div><code>${escapeHtml(String(me.userId))}</code></div></div>` : ''}
      `;
    } catch (err) {
      toast(`${I18n.t('profile.load_error')}: ${err.message}`, 'error');
    }
  },
  unmount() {},
};

export default ProfileScreen;
