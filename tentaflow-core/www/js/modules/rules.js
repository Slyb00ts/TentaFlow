// =============================================================================
// Plik: modules/rules.js
// Opis: 3 zakladki (tf-tabs): TTS / PII / Fast-path.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

let activeTab = 'tts';

const RulesScreen = {
  get title() { return I18n.t('rules.title'); },
  render() {
    return `
      <div class="content-header"><h1>${escapeHtml(I18n.t('rules.title'))}</h1></div>
      <div style="margin-bottom: var(--space-4);">
        <tf-tabs variant="underline" value="${activeTab}" id="rules-tabs">
          <tf-tab id="tts">${escapeHtml(I18n.t('rules.tab_tts'))}</tf-tab>
          <tf-tab id="pii">${escapeHtml(I18n.t('rules.tab_pii'))}</tf-tab>
          <tf-tab id="fastpath">${escapeHtml(I18n.t('rules.tab_fastpath'))}</tf-tab>
        </tf-tabs>
      </div>
      <div class="card" style="padding: 0;"><div id="rules-host"></div></div>`;
  },
  async mount() {
    const tabs = byId('rules-tabs');
    tabs.addEventListener('change', (e) => {
      activeTab = e.detail.value;
      loadActive();
    });
    await loadActive();
  },
  unmount() {},
};

async function loadActive() {
  const host = byId('rules-host');
  host.innerHTML = `<div class="view-loader"><div class="view-loader-spinner"></div>${escapeHtml(I18n.t('rules.loading'))}</div>`;
  try {
    if (activeTab === 'tts') await loadTts(host);
    else if (activeTab === 'pii') await loadPii(host);
    else await loadFastPath(host);
  } catch (err) { toast(`${I18n.t('rules.error_prefix')}: ${err.message}`, 'error'); }
}

async function loadTts(host) {
  const rules = await ApiBinary.list('ttsRuleListRequest');
  if (rules.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('rules.empty_tts'))}</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr>
        <th>${escapeHtml(I18n.t('rules.col_pattern'))}</th>
        <th>${escapeHtml(I18n.t('rules.col_voice'))}</th>
        <th>${escapeHtml(I18n.t('rules.col_priority'))}</th>
        <th></th>
      </tr></thead>
      <tbody>${rules.map((r) => `<tr>
        <td><code>${escapeHtml(r.pattern)}</code></td>
        <td>${escapeHtml(r.voiceId)}</td>
        <td>${r.priority}</td>
        <td style="text-align:right;"><tf-button variant="danger" size="sm" icon="trash" data-rm="${escapeHtml(r.id)}" title="${escapeHtml(I18n.t('rules.delete_title'))}"></tf-button></td>
      </tr>`).join('')}</tbody>
    </table>`;
  host.querySelectorAll('[data-rm]').forEach((b) => {
    b.addEventListener('click', async () => {
      try {
        await ApiBinary.action('ttsRuleDeleteRequest', { ruleId: b.dataset.rm });
        toast(I18n.t('rules.deleted_ok'), 'success');
        await loadTts(host);
      } catch (err) { toast(`${I18n.t('rules.error_prefix')}: ${err.message}`, 'error'); }
    });
  });
}

async function loadPii(host) {
  const rules = await ApiBinary.list('piiRuleListRequest');
  host.innerHTML = rules.length === 0
    ? `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('rules.empty_pii'))}</div></div>`
    : `<table class="data-table">
        <thead><tr>
          <th>${escapeHtml(I18n.t('rules.col_category'))}</th>
          <th>${escapeHtml(I18n.t('rules.col_regex'))}</th>
          <th>${escapeHtml(I18n.t('rules.col_action'))}</th>
        </tr></thead>
        <tbody>${rules.map((r) => `<tr>
          <td><tf-chip status="accent">${escapeHtml(r.kind)}</tf-chip></td>
          <td><code>${escapeHtml(r.regex)}</code></td>
          <td>${escapeHtml(r.action)}</td>
        </tr>`).join('')}</tbody>
      </table>`;
}

async function loadFastPath(host) {
  const patterns = await ApiBinary.list('fastPathListRequest', { arrayKey: 'patterns' });
  host.innerHTML = patterns.length === 0
    ? `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('rules.empty_fastpath'))}</div></div>`
    : `<table class="data-table">
        <thead><tr>
          <th>${escapeHtml(I18n.t('rules.col_pattern'))}</th>
          <th>${escapeHtml(I18n.t('rules.col_response'))}</th>
          <th>${escapeHtml(I18n.t('rules.col_priority'))}</th>
        </tr></thead>
        <tbody>${patterns.map((p) => `<tr>
          <td><code>${escapeHtml(p.pattern)}</code></td>
          <td><pre style="margin: 0; max-width: 400px; overflow-x: auto;">${escapeHtml(p.response)}</pre></td>
          <td>${p.priority}</td>
        </tr>`).join('')}</tbody>
      </table>`;
}

export default RulesScreen;
