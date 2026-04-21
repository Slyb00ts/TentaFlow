// =============================================================================
// File: modules/prompts-user.js — Read-only prompt library for role=user.
// Reuses PromptListRequest / PromptDetailRequest (same binary handlers as the
// admin prompts screen); editing is omitted.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, toast, formatDate } from '/js/utils.js';

let prompts = [];

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const PromptsUserScreen = {
  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('star')} ${escapeHtml(I18n.t('prompts_user.title'))}</h1>
          <div class="sub">${escapeHtml(I18n.t('prompts_user.subtitle'))}</div>
        </div>
      </div>
      <div class="card" style="padding: 0;"><div id="prompts-user-host"></div></div>
      <div id="prompts-user-detail"></div>`;
  },
  async mount() {
    try {
      prompts = await ApiBinary.list('promptListRequest');
      renderTable();
    } catch (err) {
      toast(`${I18n.t('prompts_user.load_error')}: ${err.message}`, 'error');
    }
  },
  unmount() { prompts = []; },
};

function renderTable() {
  const host = byId('prompts-user-host');
  if (!host) return;
  if (prompts.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('prompts_user.empty'))}</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr>
        <th>${escapeHtml(I18n.t('prompts_user.col_name'))}</th>
        <th>${escapeHtml(I18n.t('prompts_user.col_category'))}</th>
        <th>${escapeHtml(I18n.t('prompts_user.col_updated'))}</th>
        <th></th>
      </tr></thead>
      <tbody>
        ${prompts.map((p) => `
          <tr>
            <td>${escapeHtml(p.name ?? '—')}</td>
            <td><tf-chip status="accent">${escapeHtml(p.category ?? '—')}</tf-chip></td>
            <td>${p.updatedAtEpoch ? formatDate(p.updatedAtEpoch) : '—'}</td>
            <td><tf-button variant="secondary" size="sm" data-detail="${escapeHtml(p.id)}">${escapeHtml(I18n.t('prompts_user.view'))}</tf-button></td>
          </tr>`).join('')}
      </tbody>
    </table>`;
  host.querySelectorAll('[data-detail]').forEach((b) => {
    b.addEventListener('click', () => showDetail(b.dataset.detail));
  });
}

async function showDetail(promptId) {
  try {
    const d = await ApiBinary.one('promptDetailRequest', { promptId });
    const host = byId('prompts-user-detail');
    host.innerHTML = `
      <div class="card" style="margin-top: var(--space-4);">
        <div class="card-header">
          <h3 class="card-title">${escapeHtml(d.name)}</h3>
          <tf-button variant="ghost" size="sm" id="prompts-user-close">×</tf-button>
        </div>
        <div class="form-row"><span class="label">${escapeHtml(I18n.t('prompts_user.col_category'))}</span><div>${escapeHtml(d.category ?? '—')}</div></div>
        <div class="form-row"><span class="label">${escapeHtml(I18n.t('prompts_user.variables'))}</span>
          <div>${(d.variables || []).map((v) => `<tf-chip status="accent">${escapeHtml(v)}</tf-chip>`).join(' ') || '—'}</div>
        </div>
        <div class="form-row"><span class="label">${escapeHtml(I18n.t('prompts_user.template'))}</span>
          <pre style="background: var(--bg); padding: 12px; border-radius: var(--radius); border: 1px solid var(--border); white-space: pre-wrap;">${escapeHtml(d.template ?? '')}</pre>
        </div>
      </div>`;
    byId('prompts-user-close').addEventListener('click', () => { host.innerHTML = ''; });
  } catch (err) {
    toast(`${I18n.t('prompts_user.load_error')}: ${err.message}`, 'error');
  }
}

export default PromptsUserScreen;
