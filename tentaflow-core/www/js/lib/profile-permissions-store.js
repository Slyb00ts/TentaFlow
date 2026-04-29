// =============================================================================
// File: lib/profile-permissions-store.js
// Purpose: In-memory singleton trzymajacy sudo password (NIGDY na dysku) plus
//          per-tab listee wylaczonych source-id (persistowana w localStorage,
//          ale nie hasla). Wspolny stan dla Profile Permissions Settings,
//          Profiling Launch Modal i Compare/Permissions screens.
// =============================================================================

// Hasło sudo żyje tylko dopóki ta zakładka przeglądarki jest otwarta.
// Brak żadnej formy serializacji - to jest twarda gwarancja security.
let _sudoPassword = '';
let _sudoValidatedAt = 0; // unix ms; gdy != 0, hasło zostało potwierdzone przez backend

const DISABLED_SOURCES_KEY = 'tf-profile-permissions-disabled-sources';
const COLLECTOR_PATHS_KEY = 'tf-profile-permissions-collector-paths';

function readJsonLocal(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw);
    return parsed ?? fallback;
  } catch (_e) {
    return fallback;
  }
}

function writeJsonLocal(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch (_e) {
    // limit przekroczony albo storage zablokowany - cicho ignorujemy
  }
}

// =============================================================================
// Sudo password — strictly in-memory.
// =============================================================================

export function getSudoPassword() {
  return _sudoPassword;
}

export function setSudoPassword(value) {
  _sudoPassword = typeof value === 'string' ? value : '';
  // ustawienie nowego hasła unieważnia poprzednią walidację
  _sudoValidatedAt = 0;
}

export function clearSudoPassword() {
  _sudoPassword = '';
  _sudoValidatedAt = 0;
}

export function isSudoValidated() {
  return _sudoValidatedAt > 0;
}

export function markSudoValidated() {
  _sudoValidatedAt = Date.now();
}

// Wywoluje backend `POST /api/profiling/validate-sudo`. Backend może nie
// istnieć — wtedy zwracamy pseudoreport "not implemented" i zostawiamy decyzję
// callerowi. NIE udajemy sukcesu.
export async function validateSudo(password) {
  const payload = typeof password === 'string' ? password : _sudoPassword;
  if (!payload) {
    return { ok: false, reason: 'empty', backendAvailable: true };
  }
  try {
    const resp = await fetch('/api/profiling/validate-sudo', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ password: payload }),
    });
    if (resp.status === 404 || resp.status === 405) {
      console.warn('[profile-permissions] backend nie udostępnia /api/profiling/validate-sudo — walidacja zostanie wykonana dopiero przy starcie sesji');
      return { ok: false, reason: 'backend_missing', backendAvailable: false };
    }
    if (!resp.ok) {
      return { ok: false, reason: `http_${resp.status}`, backendAvailable: true };
    }
    const data = await resp.json().catch(() => ({}));
    const ok = !!data.ok;
    if (ok) markSudoValidated();
    return { ok, reason: data.reason || '', backendAvailable: true };
  } catch (err) {
    console.warn('[profile-permissions] validateSudo network error:', err?.message || err);
    return { ok: false, reason: 'network', backendAvailable: false };
  }
}

// =============================================================================
// Disabled source ids — persistowane w localStorage (per-browser).
// =============================================================================

export function getDisabledSources() {
  const arr = readJsonLocal(DISABLED_SOURCES_KEY, []);
  return Array.isArray(arr) ? arr.slice() : [];
}

export function setDisabledSources(arr) {
  const clean = Array.isArray(arr) ? arr.filter((x) => typeof x === 'string') : [];
  writeJsonLocal(DISABLED_SOURCES_KEY, clean);
}

export function isSourceDisabled(sourceId) {
  return getDisabledSources().includes(sourceId);
}

export function toggleSourceDisabled(sourceId, disabled) {
  const set = new Set(getDisabledSources());
  if (disabled) set.add(sourceId);
  else set.delete(sourceId);
  setDisabledSources(Array.from(set));
}

// =============================================================================
// Collector binary path overrides — localStorage (auto-discovery hint UI).
// =============================================================================

export function getCollectorPaths() {
  return readJsonLocal(COLLECTOR_PATHS_KEY, {}) || {};
}

export function setCollectorPath(collectorId, pathStr) {
  const cur = getCollectorPaths();
  if (pathStr) cur[collectorId] = String(pathStr);
  else delete cur[collectorId];
  writeJsonLocal(COLLECTOR_PATHS_KEY, cur);
}

export function resetCollectorPaths() {
  writeJsonLocal(COLLECTOR_PATHS_KEY, {});
}
