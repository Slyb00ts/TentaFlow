// =============================================================================
// Plik: modules/catalog/LicenseBadge.js
// Opis: Helper do sprawdzania tier licencji uzytkownika (free/pro/enterprise)
//       i renderowania badge. Cache odpowiedzi /api/license/info na 60 s.
// Przyklad: const info = await LicenseBadge.fetchInfo(); LicenseBadge.isProAllowed(info);
// =============================================================================

const LicenseBadge = (() => {
  'use strict';

  const CACHE_TTL_MS = 60000;
  const FALLBACK_INFO = { tier: 'free', allows_pro: false, allows_enterprise: false };

  let cachedInfo = null;
  let cachedAt = 0;
  let inflight = null;

  // Pobiera informacje o licencji z backendu. Cache 60 s.
  // force=true wymusza odswiezenie z pominieciem cache.
  function fetchInfo(force) {
    const now = Date.now();
    if (!force && cachedInfo && (now - cachedAt) < CACHE_TTL_MS) {
      return Promise.resolve(cachedInfo);
    }
    if (inflight) return inflight;

    inflight = (async () => {
      try {
        let resp;
        if (typeof ApiClient !== 'undefined' && typeof ApiClient.get === 'function') {
          resp = await ApiClient.get('/api/license/info');
          cachedInfo = resp && typeof resp === 'object' ? resp : FALLBACK_INFO;
        } else {
          const r = await fetch('/api/license/info');
          if (!r.ok) throw new Error('HTTP ' + r.status);
          cachedInfo = await r.json();
        }
        cachedAt = Date.now();
        return cachedInfo;
      } catch (err) {
        console.error('[LicenseBadge] nie udalo sie pobrac /api/license/info:', err);
        cachedInfo = Object.assign({}, FALLBACK_INFO);
        cachedAt = Date.now();
        return cachedInfo;
      } finally {
        inflight = null;
      }
    })();

    return inflight;
  }

  function isProAllowed(info) {
    return !!(info && info.allows_pro === true);
  }

  function isEnterpriseAllowed(info) {
    return !!(info && info.allows_enterprise === true);
  }

  // Render HTML badge dla tier (free/pro/enterprise).
  function renderTierBadge(tier) {
    const t = String(tier || 'free').toLowerCase();
    const colors = { free: '#888', pro: '#4caf50', enterprise: '#9c27b0' };
    const label = t.charAt(0).toUpperCase() + t.slice(1);
    const color = colors[t] || '#888';
    return '<span class="tier-badge" style="background:' + color +
      ';color:#fff;padding:2px 8px;border-radius:4px;font-size:0.85em;">' +
      Utils.escapeHtml(label) + '</span>';
  }

  // Czysci cache info licencji. Powinno byc wywolane przy:
  //   - zmianie licencji w Settings (gdy zostanie zaimplementowany endpoint)
  //   - wylogowaniu i zalogowaniu (potencjalnie inny user / inna licencja)
  //   - manualnym refresh przez admina
  // TODO[License v2]: invalidate() musi byc wywolane po zmianie licencji w Settings
  //   gdy zostanie zaimplementowany endpoint zmiany licencji (obecnie backend zwraca
  //   stub Free i nie ma flow zmiany).
  function invalidate() {
    cachedInfo = null;
    cachedAt = 0;
  }

  return {
    fetchInfo,
    isProAllowed,
    isEnterpriseAllowed,
    renderTierBadge,
    invalidate
  };
})();
