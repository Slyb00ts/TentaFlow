// =============================================================================
// File: modules/coming-soon.js — Honest placeholder for user-app routes whose
// backend binary handlers are not yet wired. Does NOT fake the feature: it
// explains to the user what is missing and returns them to the apps home.
//
// Usage:
//   Router.register('images', makeComingSoonScreen('images', 'image'));
// =============================================================================

import { Router } from '/js/router.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml } from '/js/utils.js';

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

export function makeComingSoonScreen(appId, iconId) {
  return {
    render() {
      const title = escapeHtml(I18n.t(`apps.${appId}.name`));
      const desc = escapeHtml(I18n.t(`apps.${appId}.desc`));
      const body = escapeHtml(I18n.t(`coming_soon.${appId}`));
      return `
        <div class="page-header">
          <div>
            <h1>${sprite(iconId)} ${title}</h1>
            <div class="sub">${desc}</div>
          </div>
        </div>
        <div class="card coming-soon-card">
          <div class="coming-soon-badge">${escapeHtml(I18n.t('apps.badge_soon'))}</div>
          <h3 class="coming-soon-title">${escapeHtml(I18n.t('coming_soon.heading'))}</h3>
          <p class="coming-soon-body">${body}</p>
          <tf-button variant="primary" id="coming-soon-back">${escapeHtml(I18n.t('coming_soon.back'))}</tf-button>
        </div>`;
    },
    mount() {
      byId('coming-soon-back')?.addEventListener('click', () => Router.navigate('apps-home'));
    },
    unmount() {},
  };
}
