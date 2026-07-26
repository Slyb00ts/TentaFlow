// =============================================================================
// Plik: modules/rules.js
// Opis: 3 zakladki (tf-tabs): TTS / PII / Fast-path.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-input.js';

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

// Syntetyzuje `text` przez TTS (akcja binarna ttsPreviewRequest -> bajty audio)
// i odtwarza w przegladarce, zeby uslyszec jak zamiennik wyjdzie. Uzywa
// pierwszego wdrozonego modelu TTS; voice puste -> silnik bierze domyslny.
async function playTts(text, ttsModels) {
  const t = (text || '').trim();
  if (!t) { toast(I18n.t('rules.pattern_required'), 'error'); return; }
  if (!ttsModels.length) { toast(I18n.t('rules.no_tts_model'), 'error'); return; }
  try {
    const resp = await ApiBinary.action('ttsPreviewRequest', {
      text: t, model: ttsModels[0].model_name, voice: '',
    });
    if (!resp || !resp.bytes || !resp.bytes.length) {
      toast(I18n.t('rules.error_prefix'), 'error');
      return;
    }
    const blob = new Blob([resp.bytes], { type: `audio/${resp.format || 'wav'}` });
    const url = URL.createObjectURL(blob);
    const audio = new Audio(url);
    audio.addEventListener('ended', () => URL.revokeObjectURL(url));
    await audio.play();
  } catch (err) { toast(`${I18n.t('rules.error_prefix')}: ${err.message}`, 'error'); }
}

async function loadTts(host) {
  const rules = await ApiBinary.list('ttsRuleListRequest');
  // Modele TTS (do podgladu play). Filtr po kategorii — jak chat.js.
  const allModels = (await ApiBinary.list('modelListRequest', { arrayKey: 'models' })) || [];
  const ttsModels = allModels.filter(
    (m) => (m.category || m.service_type || '').toLowerCase() === 'tts',
  );
  // Formularz dodawania substytucji (pattern -> zamiennik). Reguly sa
  // stosowane przed TTS (clean_cache): kazde wystapienie `pattern` w tekscie
  // czytanym przez TTS zamieniane jest na `replacement`. Tekst odpowiedzi
  // (bąbel) NIE jest zmieniany — czyszczenie dotyczy wylacznie galezi TTS.
  const form = `
    <div class="tf-toolbar rules-add-row">
      <tf-input id="tts-pattern" label="${escapeHtml(I18n.t('rules.col_pattern'))}" placeholder="WWW"></tf-input>
      <tf-input id="tts-replacement" label="${escapeHtml(I18n.t('rules.col_replacement'))}" placeholder="w u w u"></tf-input>
      <tf-input id="tts-priority" type="number" label="${escapeHtml(I18n.t('rules.col_priority'))}" value="100" class="rules-priority"></tf-input>
      <tf-button id="tts-play" variant="secondary" icon="play" title="${escapeHtml(I18n.t('rules.play_title'))}"></tf-button>
      <tf-button id="tts-add" variant="primary" icon="plus">${escapeHtml(I18n.t('rules.add'))}</tf-button>
    </div>`;
  const table = rules.length === 0
    ? `<div class="empty-state"><div class="empty-state-text">${escapeHtml(I18n.t('rules.empty_tts'))}</div></div>`
    : `<table class="data-table">
        <thead><tr>
          <th>${escapeHtml(I18n.t('rules.col_pattern'))}</th>
          <th>${escapeHtml(I18n.t('rules.col_replacement'))}</th>
          <th>${escapeHtml(I18n.t('rules.col_priority'))}</th>
          <th></th>
        </tr></thead>
        <tbody>${rules.map((r) => `<tr>
          <td data-label="${escapeAttr(I18n.t('rules.col_pattern'))}"><code>${escapeHtml(r.pattern)}</code></td>
          <td data-label="${escapeAttr(I18n.t('rules.col_replacement'))}">${escapeHtml(r.voiceId)}</td>
          <td data-label="${escapeAttr(I18n.t('rules.col_priority'))}">${r.priority}</td>
          <td style="text-align:right; white-space:nowrap;">
            <tf-button variant="secondary" size="sm" icon="play" data-play="${escapeHtml(r.voiceId)}" title="${escapeHtml(I18n.t('rules.play_title'))}"></tf-button>
            <tf-button variant="danger" size="sm" icon="trash" data-rm="${escapeHtml(r.id)}" title="${escapeHtml(I18n.t('rules.delete_title'))}"></tf-button>
          </td>
        </tr>`).join('')}</tbody>
      </table>`;
  host.innerHTML = form + table;

  byId('tts-play').addEventListener('click', () => playTts(byId('tts-replacement').value, ttsModels));

  byId('tts-add').addEventListener('click', async () => {
    const pattern = (byId('tts-pattern').value || '').trim();
    const replacement = (byId('tts-replacement').value || '').trim();
    const priority = parseInt(byId('tts-priority').value, 10) || 100;
    if (!pattern) { toast(I18n.t('rules.pattern_required'), 'error'); return; }
    try {
      // Pole `voiceId` w protokole TtsRule niesie tekst zamiennika (historyczna
      // nazwa) — handler zapisuje regule typu `phonetic` (substytucja).
      await ApiBinary.action('ttsRuleCreateRequest', { pattern, voiceId: replacement, priority });
      toast(I18n.t('rules.added_ok'), 'success');
      await loadTts(host);
    } catch (err) { toast(`${I18n.t('rules.error_prefix')}: ${err.message}`, 'error'); }
  });

  host.querySelectorAll('[data-play]').forEach((b) => {
    b.addEventListener('click', () => playTts(b.dataset.play, ttsModels));
  });

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
          <td data-label="${escapeAttr(I18n.t('rules.col_category'))}"><tf-chip status="accent">${escapeHtml(r.kind)}</tf-chip></td>
          <td data-label="${escapeAttr(I18n.t('rules.col_regex'))}"><code>${escapeHtml(r.regex)}</code></td>
          <td data-label="${escapeAttr(I18n.t('rules.col_action'))}">${escapeHtml(r.action)}</td>
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
          <td data-label="${escapeAttr(I18n.t('rules.col_pattern'))}"><code>${escapeHtml(p.pattern)}</code></td>
          <td data-label="${escapeAttr(I18n.t('rules.col_response'))}"><pre style="margin: 0; max-width: 400px; overflow-x: auto;">${escapeHtml(p.response)}</pre></td>
          <td data-label="${escapeAttr(I18n.t('rules.col_priority'))}">${p.priority}</td>
        </tr>`).join('')}</tbody>
      </table>`;
}

export default RulesScreen;
