// =============================================================================
// Plik: modules/rules/RulesBinary.js
// Opis: TTS + PII + Fast-path rules ekrany zmigrowane na binary protocol.
//       Trzy niezaleznie zaladowane listy w jednym module (jeden tab UI).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const RulesBinary = (() => {
  'use strict';
  let ttsRules = [];
  let piiRules = [];
  let fastPaths = [];

  async function loadAll() {
    try {
      [ttsRules, piiRules, fastPaths] = await Promise.all([
        ApiBinary.list('ttsRuleListRequest'),
        ApiBinary.list('piiRuleListRequest'),
        ApiBinary.list('fastPathListRequest', { arrayKey: 'patterns' }),
      ]);
      renderAll();
    } catch (err) {
      console.error('[rules-binary] load failed:', err);
    }
  }

  function renderAll() {
    renderList('tts-rules-tbody', ttsRules, r => `
      <tr><td>${Utils.escapeHtml(r.pattern)}</td><td>${Utils.escapeHtml(r.voiceId)}</td><td>${r.priority}</td></tr>
    `);
    renderList('pii-rules-tbody', piiRules, r => `
      <tr><td>${Utils.escapeHtml(r.kind)}</td><td><code>${Utils.escapeHtml(r.regex)}</code></td><td>${Utils.escapeHtml(r.action)}</td></tr>
    `);
    renderList('fastpath-tbody', fastPaths, p => `
      <tr><td><code>${Utils.escapeHtml(p.pattern)}</code></td><td>${Utils.escapeHtml(p.response)}</td><td>${p.priority}</td></tr>
    `);
  }

  function renderList(tbodyId, items, rowFn) {
    const tbody = document.getElementById(tbodyId);
    if (!tbody) return;
    tbody.innerHTML = items.length === 0
      ? `<tr><td colspan="3"><div class="empty-state"><div class="empty-state-text">${I18n.t('rules.empty')}</div></div></td></tr>`
      : items.map(rowFn).join('');
  }

  async function createTtsRule(rule) {
    try {
      await ApiBinary.action('ttsRuleCreateRequest', rule);
      await loadAll();
    } catch (err) {
      App.showToast(err.message, 'error');
    }
  }

  async function deleteTtsRule(ruleId) {
    if (!confirm(I18n.t('rules.delete_confirm'))) return;
    try {
      await ApiBinary.action('ttsRuleDeleteRequest', { ruleId });
      await loadAll();
    } catch (err) {
      App.showToast(err.message, 'error');
    }
  }

  return {
    mount: () => loadAll(),
    unmount: () => {
      ttsRules = [];
      piiRules = [];
      fastPaths = [];
    },
    createTtsRule,
    deleteTtsRule,
  };
})();

export default RulesBinary;
