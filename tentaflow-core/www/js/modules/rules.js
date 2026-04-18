// =============================================================================
// Plik: modules/rules.js
// Opis: 3 zakladki: TTS / PII / Fast-path.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

let activeTab = 'tts';

const RulesScreen = {
  title: 'Reguły',
  render() {
    return `
      <div class="content-header"><h1>Reguły</h1></div>
      <div style="margin-bottom: var(--space-4); display: flex; gap: var(--space-2);">
        <button class="btn" data-tab="tts">TTS</button>
        <button class="btn" data-tab="pii">PII</button>
        <button class="btn" data-tab="fastpath">Fast-path</button>
      </div>
      <div class="card" style="padding: 0;"><div id="rules-host"></div></div>`;
  },
  async mount() {
    document.querySelectorAll('[data-tab]').forEach((b) => {
      b.addEventListener('click', () => {
        activeTab = b.dataset.tab;
        loadActive();
      });
    });
    await loadActive();
  },
  unmount() {},
};

async function loadActive() {
  const host = byId('rules-host');
  document.querySelectorAll('[data-tab]').forEach((b) => {
    b.classList.toggle('btn-primary', b.dataset.tab === activeTab);
  });
  host.innerHTML = '<div class="view-loader"><div class="view-loader-spinner"></div>Ładowanie…</div>';
  try {
    if (activeTab === 'tts') await loadTts(host);
    else if (activeTab === 'pii') await loadPii(host);
    else await loadFastPath(host);
  } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
}

async function loadTts(host) {
  const rules = await ApiBinary.list('ttsRuleListRequest');
  if (rules.length === 0) {
    host.innerHTML = `<div class="empty-state"><div class="empty-state-text">Brak reguł TTS</div></div>`;
    return;
  }
  host.innerHTML = `
    <table class="data-table">
      <thead><tr><th>Pattern</th><th>Voice</th><th>Priorytet</th><th></th></tr></thead>
      <tbody>${rules.map((r) => `<tr>
        <td><code>${escapeHtml(r.pattern)}</code></td>
        <td>${escapeHtml(r.voiceId)}</td>
        <td>${r.priority}</td>
        <td><button class="btn btn-sm btn-danger" data-rm="${escapeHtml(r.id)}">Usuń</button></td>
      </tr>`).join('')}</tbody>
    </table>`;
  host.querySelectorAll('[data-rm]').forEach((b) => {
    b.addEventListener('click', async () => {
      try {
        await ApiBinary.action('ttsRuleDeleteRequest', { ruleId: b.dataset.rm });
        toast('Usunięto', 'success');
        await loadTts(host);
      } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
    });
  });
}

async function loadPii(host) {
  const rules = await ApiBinary.list('piiRuleListRequest');
  host.innerHTML = rules.length === 0
    ? `<div class="empty-state"><div class="empty-state-text">Brak reguł PII</div></div>`
    : `<table class="data-table">
        <thead><tr><th>Kategoria</th><th>Regex</th><th>Akcja</th></tr></thead>
        <tbody>${rules.map((r) => `<tr>
          <td><span class="badge">${escapeHtml(r.kind)}</span></td>
          <td><code>${escapeHtml(r.regex)}</code></td>
          <td>${escapeHtml(r.action)}</td>
        </tr>`).join('')}</tbody>
      </table>`;
}

async function loadFastPath(host) {
  const patterns = await ApiBinary.list('fastPathListRequest', { arrayKey: 'patterns' });
  host.innerHTML = patterns.length === 0
    ? `<div class="empty-state"><div class="empty-state-text">Brak fast-path patterns</div></div>`
    : `<table class="data-table">
        <thead><tr><th>Pattern</th><th>Response</th><th>Priorytet</th></tr></thead>
        <tbody>${patterns.map((p) => `<tr>
          <td><code>${escapeHtml(p.pattern)}</code></td>
          <td><pre style="margin: 0; max-width: 400px; overflow-x: auto;">${escapeHtml(p.response)}</pre></td>
          <td>${p.priority}</td>
        </tr>`).join('')}</tbody>
      </table>`;
}

export default RulesScreen;
