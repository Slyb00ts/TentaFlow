// =============================================================================
// Plik: modules/update-banner.js
// Opis: Baner "Nowa wersja" — pokazuje sie gdy handshake WS wykryje, ze front
//       nie zgadza sie z backendem (zmiana zasobow lub twardy mismatch protokolu
//       po aktualizacji backendu/addonu). Reload robi pelny hard-reload (kasuje
//       service worker + cache), zeby uzytkownik nie musial tego robic recznie.
//       Komunikat mozna pominac (poza sesja) — twardy mismatch i tak wymusi
//       reconnect przez connection-overlay.
// Przyklad: init() wywolywane raz z app.js; potem sam reaguje na lifecycle event.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';

let mounted = false;
let el = null;
// Dismiss trzymany per hash serwera — pominiecie jednej wersji nie ukrywa
// banera dla kolejnej, nowszej wersji.
let dismissedHash = null;
let currentServerHash = null;

function escapeHtml(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ));
}

function t(key, fallback, vars) {
  const v = I18n.t(key, vars);
  return v === key ? fallback : v;
}

function build() {
  el = document.createElement('div');
  el.className = 'update-banner';
  el.setAttribute('role', 'status');
  el.setAttribute('aria-live', 'polite');
  el.innerHTML = `
    <div class="update-banner-inner">
      <svg class="update-banner-icon" viewBox="0 0 24 24" aria-hidden="true">
        <polyline points="23 4 23 10 17 10"/>
        <path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/>
      </svg>
      <div class="update-banner-text">
        <div class="update-banner-title"></div>
        <div class="update-banner-desc"></div>
      </div>
      <div class="update-banner-actions">
        <tf-button size="sm" variant="ghost" data-action="dismiss">${escapeHtml(t('update.later', 'Później'))}</tf-button>
        <tf-button size="sm" variant="primary" icon="refresh" data-action="reload">${escapeHtml(t('update.reload', 'Odśwież'))}</tf-button>
      </div>
    </div>
  `;
  document.body.appendChild(el);

  el.querySelector('[data-action="reload"]').addEventListener('click', () => {
    hardReload();
  });
  el.querySelector('[data-action="dismiss"]').addEventListener('click', () => {
    dismissedHash = currentServerHash;
    hide();
  });
}

function show(info) {
  if (!el) return;
  const required = !!info?.required;
  const titleEl = el.querySelector('.update-banner-title');
  const descEl = el.querySelector('.update-banner-desc');
  titleEl.textContent = required
    ? t('update.title_required', 'Wymagana aktualizacja')
    : t('update.title', 'Nowa wersja aplikacji');
  descEl.textContent = required
    ? t('update.desc_required', 'Front jest niezgodny z serwerem. Odśwież, aby kontynuować.')
    : t('update.desc', 'Serwer został zaktualizowany. Odśwież, aby załadować najnowszą wersję.');
  el.classList.toggle('required', required);
  el.classList.add('visible');
}

function hide() {
  if (el) el.classList.remove('visible');
}

/** Pelny hard-reload: usun service worker + cache, potem przeladuj z sieci. */
async function hardReload() {
  try {
    if ('serviceWorker' in navigator) {
      const regs = await navigator.serviceWorker.getRegistrations();
      await Promise.all(regs.map((r) => r.unregister()));
    }
    if (window.caches) {
      const keys = await caches.keys();
      await Promise.all(keys.map((k) => caches.delete(k)));
    }
  } catch { /* ignore — reload i tak pobierze swiezy front */ }
  window.location.reload();
}

/** init() — podpina sie do ApiBinary lifecycle (event 'update-available'). */
export function init() {
  if (mounted) return;
  mounted = true;
  build();

  ApiBinary.onLifecycle((ev) => {
    if (ev.type !== 'update-available') return;
    currentServerHash = ev.info?.server ?? null;
    // Twardy mismatch pokazujemy zawsze; miekki pomijamy jesli user go odrzucil
    // dla tego samego hasha serwera.
    if (!ev.info?.required && currentServerHash && currentServerHash === dismissedHash) {
      return;
    }
    show(ev.info);
  });
}

/** Destroy — do testow / HMR. */
export function destroy() {
  if (!mounted) return;
  if (el && el.parentNode) el.parentNode.removeChild(el);
  el = null;
  mounted = false;
}
