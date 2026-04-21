// =============================================================================
// File: modules/translate.js — Translate user app. 2-column source/target
// layout, debounced auto-translate, language swap, copy to clipboard. Uses
// TranslateRequest binary handler (single-shot, not a stream).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { byId, escapeHtml, toast } from '/js/utils.js';

// Must stay in sync with backend SUPPORTED_LANGS allowlist in
// api/dashboard/handlers_translate.rs.
const SUPPORTED_LANGS = ['en', 'pl', 'de', 'es', 'fr', 'it', 'nl', 'pt', 'uk', 'ru', 'cs', 'ja', 'zh', 'ko'];
const TONE_VALUES = ['neutral', 'formal', 'casual'];
const DEBOUNCE_MS = 400;
const MAX_SOURCE_CHARS = 10_000;

let state = {
  sourceLang: 'auto',
  targetLang: 'en',
  tone: 'neutral',
  sourceText: '',
  result: null,
  error: null,
  pending: false,
  debounceTimer: null,
  requestSeq: 0,
};

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

function langOptions(includeAuto) {
  const opts = includeAuto
    ? [`<option value="auto">${escapeHtml(I18n.t('translate.auto_detect'))}</option>`]
    : [];
  for (const code of SUPPORTED_LANGS) {
    opts.push(`<option value="${code}">${escapeHtml(I18n.t(`translate.langs.${code}`))}</option>`);
  }
  return opts.join('');
}

function toneOptions() {
  return TONE_VALUES
    .map((v) => `<option value="${v}">${escapeHtml(I18n.t(`translate.tone.${v}`))}</option>`)
    .join('');
}

const TranslateScreen = {
  render() {
    // Seed target language from the UI language when supported; otherwise 'en'.
    const uiLang = (I18n.getLanguage() || 'en').toLowerCase();
    state.targetLang = SUPPORTED_LANGS.includes(uiLang) ? uiLang : 'en';
    state.sourceLang = 'auto';
    state.tone = 'neutral';
    state.sourceText = '';
    state.result = null;
    state.error = null;
    state.pending = false;

    return `
      <div class="translate-page">
        <div class="page-header">
          <div>
            <h1>${sprite('globe')} ${escapeHtml(I18n.t('translate.page_title'))}</h1>
            <div class="sub">${escapeHtml(I18n.t('translate.subtitle'))}</div>
          </div>
        </div>

        <div class="card" style="padding: 0;">
          <div class="translate-toolbar">
            <tf-select id="translate-source-lang" label="${escapeHtml(I18n.t('translate.source_label'))}" value="${state.sourceLang}">
              ${langOptions(true)}
            </tf-select>
            <tf-button variant="ghost" size="sm" id="translate-swap" class="translate-swap-btn" title="${escapeHtml(I18n.t('translate.swap'))}" aria-label="${escapeHtml(I18n.t('translate.swap'))}">↔</tf-button>
            <tf-select id="translate-target-lang" label="${escapeHtml(I18n.t('translate.target_label'))}" value="${state.targetLang}">
              ${langOptions(false)}
            </tf-select>
            <tf-select id="translate-tone" label="${escapeHtml(I18n.t('translate.tone_label'))}" value="${state.tone}">
              ${toneOptions()}
            </tf-select>
          </div>

          <div class="translate-grid">
            <div class="translate-panel">
              <tf-textarea
                id="translate-source"
                rows="10"
                autogrow
                maxlength="${MAX_SOURCE_CHARS}"
                placeholder="${escapeHtml(I18n.t('translate.source_placeholder'))}"
              ></tf-textarea>
              <div class="translate-footer">
                <span id="translate-counter" class="translate-counter"></span>
              </div>
            </div>

            <div class="translate-panel">
              <div id="translate-result" class="translate-result">
                <span class="translate-placeholder">${escapeHtml(I18n.t('translate.result_placeholder'))}</span>
              </div>
              <div class="translate-footer">
                <span id="translate-status"></span>
                <tf-button variant="ghost" size="sm" id="translate-copy" disabled title="${escapeHtml(I18n.t('translate.copy'))}" aria-label="${escapeHtml(I18n.t('translate.copy'))}">${sprite('copy')}</tf-button>
              </div>
            </div>
          </div>
        </div>
      </div>`;
  },

  mount() {
    const srcLangEl = byId('translate-source-lang');
    const tgtLangEl = byId('translate-target-lang');
    const toneEl = byId('translate-tone');
    const srcEl = byId('translate-source');
    const swapBtn = byId('translate-swap');
    const copyBtn = byId('translate-copy');

    srcLangEl?.addEventListener('change', (e) => {
      state.sourceLang = e.target.value;
      scheduleTranslate();
    });
    tgtLangEl?.addEventListener('change', (e) => {
      state.targetLang = e.target.value;
      scheduleTranslate();
    });
    toneEl?.addEventListener('change', (e) => {
      state.tone = e.target.value;
      scheduleTranslate();
    });

    srcEl?.addEventListener('input', (e) => {
      state.sourceText = e.detail?.value ?? '';
      updateCounter();
      scheduleTranslate();
    });

    swapBtn?.addEventListener('click', () => swapLanguages());
    copyBtn?.addEventListener('click', () => copyResult());

    updateCounter();
    renderResult();
  },

  unmount() {
    if (state.debounceTimer) {
      clearTimeout(state.debounceTimer);
      state.debounceTimer = null;
    }
    state.result = null;
    state.error = null;
    state.pending = false;
    state.sourceText = '';
  },
};

function updateCounter() {
  const el = byId('translate-counter');
  if (!el) return;
  const n = state.sourceText.length;
  el.textContent = I18n.t('translate.character_count').replace('{n}', `${n}/${MAX_SOURCE_CHARS}`);
}

function scheduleTranslate() {
  if (state.debounceTimer) clearTimeout(state.debounceTimer);
  const text = state.sourceText.trim();
  if (!text) {
    state.result = null;
    state.error = null;
    state.pending = false;
    renderResult();
    return;
  }
  state.pending = true;
  renderResult();
  state.debounceTimer = setTimeout(doTranslate, DEBOUNCE_MS);
}

async function doTranslate() {
  const text = state.sourceText.trim();
  if (!text) return;
  const mySeq = ++state.requestSeq;
  try {
    const body = await ApiBinary.one('translateRequest', {
      sourceText: text,
      sourceLang: state.sourceLang,
      targetLang: state.targetLang,
      tone: state.tone === 'neutral' ? null : state.tone,
    });
    // Skip stale responses when the user typed more in the meantime.
    if (mySeq !== state.requestSeq) return;
    state.pending = false;
    state.error = null;
    state.result = body;
    renderResult();
  } catch (err) {
    if (mySeq !== state.requestSeq) return;
    state.pending = false;
    state.error = err.message || 'unknown error';
    state.result = null;
    renderResult();
  }
}

function renderResult() {
  const host = byId('translate-result');
  const status = byId('translate-status');
  const copyBtn = byId('translate-copy');
  if (!host) return;

  if (state.error) {
    const msg = I18n.t('translate.error').replace('{reason}', escapeHtml(state.error));
    host.innerHTML = `<span class="translate-error-text">${msg}</span>`;
    if (status) status.textContent = '';
    if (copyBtn) copyBtn.setAttribute('disabled', '');
    return;
  }
  if (state.pending) {
    host.innerHTML = `<span class="translate-placeholder">${escapeHtml(I18n.t('translate.translating'))}</span>`;
    if (status) status.textContent = '';
    if (copyBtn) copyBtn.setAttribute('disabled', '');
    return;
  }
  if (state.result) {
    host.textContent = state.result.translatedText || '';
    if (status) {
      const info = I18n.t('translate.model_info')
        .replace('{model}', state.result.modelUsed || '—')
        .replace('{tokens}', String(state.result.tokensUsed ?? 0));
      status.textContent = info;
    }
    if (copyBtn) copyBtn.removeAttribute('disabled');
    return;
  }
  host.innerHTML = `<span class="translate-placeholder">${escapeHtml(I18n.t('translate.result_placeholder'))}</span>`;
  if (status) status.textContent = '';
  if (copyBtn) copyBtn.setAttribute('disabled', '');
}

function swapLanguages() {
  // If source is "auto", swap uses detected lang if known, else falls back
  // to the current target as new source.
  const srcLangEl = byId('translate-source-lang');
  const tgtLangEl = byId('translate-target-lang');
  const srcTextEl = byId('translate-source');
  if (!srcLangEl || !tgtLangEl) return;

  const effectiveSource = state.sourceLang === 'auto'
    ? (state.result?.detectedSourceLang || state.targetLang)
    : state.sourceLang;
  const newSource = state.targetLang;
  const newTarget = effectiveSource;
  state.sourceLang = newSource;
  state.targetLang = newTarget;
  srcLangEl.setAttribute('value', newSource);
  tgtLangEl.setAttribute('value', newTarget);

  const resultText = state.result?.translatedText || '';
  if (resultText) {
    state.sourceText = resultText;
    if (srcTextEl) srcTextEl.value = resultText;
    state.result = null;
    updateCounter();
    scheduleTranslate();
  } else {
    scheduleTranslate();
  }
}

async function copyResult() {
  if (!state.result?.translatedText) return;
  try {
    await navigator.clipboard.writeText(state.result.translatedText);
    toast(I18n.t('translate.copied'), 'success');
  } catch (err) {
    toast(`${I18n.t('translate.error').replace('{reason}', err.message)}`, 'error');
  }
}

export default TranslateScreen;
