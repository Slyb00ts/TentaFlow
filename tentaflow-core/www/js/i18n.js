// =============================================================================
// Plik: i18n.js
// Opis: Modul tlumaczen ES — laduje JSON z /i18n/{lang}.json, lookup po
//       sciezce dot-notation, fallback do en, persistuje wybor w localStorage,
//       interpolacja {placeholderow}, lista wspieranych jezykow.
// Przyklad:
//   import { I18n } from '/js/i18n.js';
//   await I18n.init();
//   document.title = I18n.t('title');
//   const msg = I18n.t('services.delete_confirm', { name: 'foo' });
//   await I18n.setLanguage('fr');
// =============================================================================

const STORAGE_KEY = 'tentaflow_lang';
const DEFAULT_LANG = 'en';

export const SUPPORTED_LANGS = [
  { code: 'pl', label: 'Polski', flag: '🇵🇱' },
  { code: 'en', label: 'English', flag: '🇬🇧' },
  { code: 'fr', label: 'Français', flag: '🇫🇷' },
  { code: 'es', label: 'Español', flag: '🇪🇸' },
  { code: 'de', label: 'Deutsch', flag: '🇩🇪' },
];

let currentLang = DEFAULT_LANG;
let translations = {};
let fallbackTranslations = null;
const listeners = new Set();

function detectLanguage() {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored && SUPPORTED_LANGS.some((l) => l.code === stored)) {
    return stored;
  }
  const browser = (navigator.language || navigator.userLanguage || '').slice(0, 2).toLowerCase();
  if (SUPPORTED_LANGS.some((l) => l.code === browser)) {
    return browser;
  }
  return DEFAULT_LANG;
}

async function fetchTranslation(lang) {
  const response = await fetch(`/i18n/${lang}.json`);
  if (!response.ok) throw new Error(`Failed to load ${lang} translations: HTTP ${response.status}`);
  return response.json();
}

async function loadTranslations(lang) {
  try {
    translations = await fetchTranslation(lang);
    currentLang = lang;
    localStorage.setItem(STORAGE_KEY, lang);
    document.documentElement.lang = lang;
    if (lang !== DEFAULT_LANG && !fallbackTranslations) {
      try {
        fallbackTranslations = await fetchTranslation(DEFAULT_LANG);
      } catch (e) {
        console.warn('[i18n] fallback en load failed', e);
      }
    } else if (lang === DEFAULT_LANG) {
      fallbackTranslations = translations;
    }
  } catch (err) {
    console.error(`[i18n] load failed for ${lang}:`, err);
    if (lang !== DEFAULT_LANG) {
      await loadTranslations(DEFAULT_LANG);
    }
  }
}

function lookup(dict, path) {
  if (!dict) return null;
  const keys = path.split('.');
  let result = dict;
  for (const key of keys) {
    if (result && typeof result === 'object' && key in result) {
      result = result[key];
    } else {
      return null;
    }
  }
  return typeof result === 'string' ? result : null;
}

function interpolate(template, vars) {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (match, key) => {
    return key in vars ? String(vars[key]) : match;
  });
}

function applyDataI18n(root = document) {
  root.querySelectorAll('[data-i18n]').forEach((el) => {
    const key = el.getAttribute('data-i18n');
    el.textContent = t(key);
  });
  root.querySelectorAll('[data-i18n-html]').forEach((el) => {
    const key = el.getAttribute('data-i18n-html');
    el.innerHTML = t(key);
  });
  root.querySelectorAll('[data-i18n-placeholder]').forEach((el) => {
    el.placeholder = t(el.getAttribute('data-i18n-placeholder'));
  });
  root.querySelectorAll('[data-i18n-title]').forEach((el) => {
    el.title = t(el.getAttribute('data-i18n-title'));
  });
}

function t(path, vars = null) {
  const value = lookup(translations, path) ?? lookup(fallbackTranslations, path);
  if (value === null) return path;
  return interpolate(value, vars);
}

async function init() {
  const lang = detectLanguage();
  await loadTranslations(lang);
  applyDataI18n();
}

async function setLanguage(lang) {
  if (lang === currentLang) return;
  if (!SUPPORTED_LANGS.some((l) => l.code === lang)) {
    throw new Error(`Unsupported language: ${lang}`);
  }
  await loadTranslations(lang);
  applyDataI18n();
  for (const listener of listeners) {
    try {
      listener(lang);
    } catch (e) {
      console.error('[i18n] listener threw', e);
    }
  }
}

function getLanguage() {
  return currentLang;
}

function subscribe(callback) {
  listeners.add(callback);
  return () => listeners.delete(callback);
}

export const I18n = {
  init,
  t,
  setLanguage,
  getLanguage,
  subscribe,
  applyDataI18n,
  supported: SUPPORTED_LANGS,
};
