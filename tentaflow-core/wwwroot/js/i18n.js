/**
 * i18n.js - Internationalization module for TentaFlow Dashboard
 */

const I18n = (() => {
  'use strict';

  let currentLang = localStorage.getItem('selected_lang') || 'en';
  let translations = {};

  async function init() {
    await loadTranslations(currentLang);
    translatePage();
  }

  async function loadTranslations(lang) {
    try {
      const response = await fetch(`i18n/${lang}.json`);
      if (!response.ok) throw new Error(`Could not load translations for ${lang}`);
      translations = await response.json();
      currentLang = lang;
      localStorage.setItem('selected_lang', lang);
      document.documentElement.lang = lang;
    } catch (error) {
      console.error('[I18n] Error loading translations:', error);
      // Fallback to English if Polish fails, or vice versa
      if (lang !== 'en') {
        await loadTranslations('en');
      }
    }
  }

  function t(path, fallback = null) {
    const keys = path.split('.');
    let result = translations;
    
    for (const key of keys) {
      if (result && result[key] !== undefined) {
        result = result[key];
      } else {
        return fallback || path;
      }
    }
    
    return result;
  }

  function translatePage() {
    // Basic text content and input placeholders
    const elements = document.querySelectorAll('[data-i18n]');
    elements.forEach(el => {
      const key = el.getAttribute('data-i18n');
      const translation = t(key);
      
      if (el.tagName === 'INPUT' && (el.type === 'text' || el.type === 'password' || el.type === 'search' || el.type === 'number')) {
        el.placeholder = translation;
      } else {
        el.textContent = translation;
      }
    });

    // Handle elements with explicit placeholder attribute
    const placeholders = document.querySelectorAll('[data-i18n-placeholder]');
    placeholders.forEach(el => {
      const key = el.getAttribute('data-i18n-placeholder');
      el.placeholder = t(key);
    });

    // Handle elements with title attribute
    const titles = document.querySelectorAll('[data-i18n-title]');
    titles.forEach(el => {
      const key = el.getAttribute('data-i18n-title');
      el.title = t(key);
    });
  }

  async function setLanguage(lang) {
    if (lang === currentLang) return;
    await loadTranslations(lang);
    translatePage();
    
    // Re-render current view to apply translations to dynamic content (Canvas, tables, etc.)
    if (typeof ViewRouter !== 'undefined') {
      const current = ViewRouter.getCurrentView();
      if (current) {
        // Ponowne wywolanie nawigacji wymusi odswiezenie widoku
        ViewRouter.navigate(current);
      }
    }
  }

  function getLanguage() {
    return currentLang;
  }

  return {
    init,
    t,
    setLanguage,
    getLanguage,
    translatePage
  };
})();
