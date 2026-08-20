// =============================================================================
// Plik: utils.js
// Opis: Helpery: escapeHtml, formatDate, byId, toast, fmtCompact/fmtExact/fmtCurrency/fmtPct/fmtMs/fmtDuration.
// =============================================================================

export function escapeHtml(s) {
  if (s === null || s === undefined) return '';
  return String(s)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

/// Dla wartosci w atrybutach HTML — ten sam escape co escapeHtml, alias dla czytelnosci.
export function escapeAttr(s) {
  return escapeHtml(s);
}

export function formatDate(epochSeconds) {
  if (!epochSeconds) return '—';
  const d = new Date(Number(epochSeconds) * 1000);
  if (isNaN(d.getTime())) return '—';
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

// Intl carries the grammar of every unit in all five shipped locales, so the
// dashboard needs no key per unit and no hardcoded Polish suffix. i18n.js keeps
// the document language current, which is why it is the source here.
const RELATIVE_UNITS = [
  ['second', 1, 60],
  ['minute', 60, 3600],
  ['hour', 3600, 86400],
  ['day', 86400, 2592000],
  ['month', 2592000, 31536000],
  ['year', 31536000, Infinity],
];

export function formatRelative(epochSeconds) {
  if (!epochSeconds) return '—';
  const seconds = Number(epochSeconds);
  if (!Number.isFinite(seconds)) return '—';
  const diffSec = Math.floor(Date.now() / 1000) - seconds;
  const abs = Math.abs(diffSec);
  const [unit, size] = RELATIVE_UNITS.find(([, , limit]) => abs < limit);
  const value = Math.floor(abs / size);
  return new Intl.RelativeTimeFormat(document.documentElement.lang || undefined, { numeric: 'auto' })
    .format(diffSec >= 0 ? -value : value, unit);
}

export function byId(id) {
  return document.getElementById(id);
}

export function patchInner(host, html) {
  if (!host) return;
  host.innerHTML = html;
}

export function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === 'class') node.className = v;
    else if (k === 'html') node.innerHTML = v;
    else if (k === 'text') node.textContent = v;
    else if (k.startsWith('on') && typeof v === 'function') {
      node.addEventListener(k.slice(2).toLowerCase(), v);
    } else if (v !== false && v !== null && v !== undefined) {
      node.setAttribute(k, v);
    }
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    if (typeof child === 'string') node.appendChild(document.createTextNode(child));
    else node.appendChild(child);
  }
  return node;
}

let toastContainer = null;
function ensureToastContainer() {
  if (toastContainer) return toastContainer;
  toastContainer = document.createElement('div');
  toastContainer.className = 'toast-container';
  document.body.appendChild(toastContainer);
  return toastContainer;
}

// Dedupe: identyczne komunikaty w oknie 6s sa mergowane — istniejacy toast
// dostaje odnowiony timer oraz licznik "× N" zamiast tworzyc nowy. Bez tego
// przy rozlaczeniu serwera dostajemy sciane 50+ toastow "Failed to fetch".
const activeToasts = new Map(); // key = `${kind}|${message}` → { el, count, hideTimer, removeTimer }

export function toast(message, kind = 'info', timeoutMs = 4000) {
  const cn = ensureToastContainer();
  const key = `${kind}|${message}`;
  const existing = activeToasts.get(key);

  if (existing) {
    existing.count += 1;
    const cntEl = existing.el.querySelector('.toast-count');
    if (cntEl) cntEl.textContent = `× ${existing.count}`;
    else {
      const span = document.createElement('span');
      span.className = 'toast-count';
      span.style.cssText = 'margin-left:8px;opacity:0.7;font-size:11px;font-weight:700;';
      span.textContent = `× ${existing.count}`;
      existing.el.appendChild(span);
    }
    // Odnow timery
    if (existing.hideTimer) clearTimeout(existing.hideTimer);
    if (existing.removeTimer) clearTimeout(existing.removeTimer);
    scheduleHide(existing, key, timeoutMs);
    return;
  }

  const t = document.createElement('div');
  t.className = `toast toast-${kind}`;
  t.textContent = message;
  cn.appendChild(t);
  const entry = { el: t, count: 1, hideTimer: null, removeTimer: null };
  activeToasts.set(key, entry);
  scheduleHide(entry, key, timeoutMs);
}

function scheduleHide(entry, key, timeoutMs) {
  entry.hideTimer = setTimeout(() => {
    entry.el.style.opacity = '0';
    entry.removeTimer = setTimeout(() => {
      entry.el.remove();
      activeToasts.delete(key);
    }, 200);
  }, timeoutMs);
}

export function bytesToHex(bytes) {
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, '0')).join('');
}

export function shortHex(bytes, len = 8) {
  return bytesToHex(bytes).slice(0, len);
}

const JWT_STORAGE_KEY = 'tentaflow_jwt';

/// REST GET z naglowkiem JWT z localStorage. Rzuca blad przy non-2xx.
export async function apiGet(path) {
  const jwt = localStorage.getItem(JWT_STORAGE_KEY);
  const resp = await fetch(path, {
    headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
  });
  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`${resp.status} ${resp.statusText}${text ? `: ${text}` : ''}`);
  }
  return resp.json();
}

/// REST POST z JSON body i JWT.
export async function apiPost(path, body) {
  const jwt = localStorage.getItem(JWT_STORAGE_KEY);
  const headers = { 'Content-Type': 'application/json' };
  if (jwt) headers.Authorization = `Bearer ${jwt}`;
  const resp = await fetch(path, {
    method: 'POST',
    headers,
    body: body != null ? JSON.stringify(body) : undefined,
  });
  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`${resp.status} ${resp.statusText}${text ? `: ${text}` : ''}`);
  }
  const ct = resp.headers.get('content-type') || '';
  return ct.includes('application/json') ? resp.json() : resp.text();
}

/// REST PUT z JSON body i JWT.
export async function apiPut(path, body) {
  const jwt = localStorage.getItem(JWT_STORAGE_KEY);
  const headers = { 'Content-Type': 'application/json' };
  if (jwt) headers.Authorization = `Bearer ${jwt}`;
  const resp = await fetch(path, {
    method: 'PUT',
    headers,
    body: body != null ? JSON.stringify(body) : undefined,
  });
  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`${resp.status} ${resp.statusText}${text ? `: ${text}` : ''}`);
  }
  const ct = resp.headers.get('content-type') || '';
  return ct.includes('application/json') ? resp.json() : resp.text();
}

/// REST DELETE.
export async function apiDelete(path) {
  const jwt = localStorage.getItem(JWT_STORAGE_KEY);
  const resp = await fetch(path, {
    method: 'DELETE',
    headers: jwt ? { Authorization: `Bearer ${jwt}` } : {},
  });
  if (!resp.ok) {
    const text = await resp.text().catch(() => '');
    throw new Error(`${resp.status} ${resp.statusText}${text ? `: ${text}` : ''}`);
  }
  return resp;
}

/// Formatuje bajty jako "12.3 MB" / "456 KB" / "8.2 GB".
export function formatBytes(bytes) {
  if (bytes == null) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = Math.abs(bytes);
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}

/// Formatuje MB jako czytelna wartosc: "2.1 GB" / "512 MB".
export function formatMb(mb) {
  if (mb == null) return '—';
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  return `${Math.round(mb)} MB`;
}

// Guard used to distinguish local-origin mutations from remote ones.
// When a handler applies an optimistic update and fires a request, the
// server typically broadcasts an event back. Without a guard the listener
// would reload the whole view for an update we already applied. Call
// markLocal(key) at the moment of the local change; the echo arriving
// within windowMs is ignored by isOwnEcho(key).
export function createEchoGuard(windowMs = 1500) {
  const record = new Map();
  return {
    markLocal(key) {
      record.set(String(key), Date.now() + windowMs);
    },
    isOwnEcho(key) {
      const k = String(key);
      const exp = record.get(k);
      if (exp == null) return false;
      if (Date.now() > exp) {
        record.delete(k);
        return false;
      }
      record.delete(k);
      return true;
    },
    clear() {
      record.clear();
    },
  };
}

// ---------------------------------------------------------------------------
// Number formatting (analytics). utils.js deliberately does NOT import
// `I18n` from /js/i18n.js: that module pulls in the binary protocol codec
// (WASM glue) and would make every consumer of utils.js — and its unit
// tests — depend on it. The current UI language is read from the DOM/storage
// instead (i18n.js keeps `document.documentElement.lang` and
// `localStorage.tentaflow_lang` in sync with its own state).
// ---------------------------------------------------------------------------

function resolveLang(lang) {
  if (lang) return lang;
  if (typeof document !== 'undefined' && document.documentElement?.lang) {
    return document.documentElement.lang;
  }
  try {
    if (typeof localStorage !== 'undefined' && localStorage.getItem('tentaflow_lang')) {
      return localStorage.getItem('tentaflow_lang');
    }
  } catch {
    // Storage access can throw (privacy mode); fall through to the default.
  }
  return 'pl';
}

const COMPACT_SCALES = [1e9, 1e6, 1e3];

/// Returns the locale's compact unit word for `scale` (e.g. pl 1e3 → "tys",
/// en 1e6 → "M") together with the literal that separates digits from the
/// unit, or null when the locale has no compact form for that scale
/// (e.g. de has none for thousands, es none for billions).
function compactUnit(lang, scale) {
  const parts = new Intl.NumberFormat(lang, { notation: 'compact', compactDisplay: 'short' })
    .formatToParts(scale);
  const compactIdx = parts.findIndex((p) => p.type === 'compact');
  if (compactIdx < 0) return null;
  const integer = parts.filter((p) => p.type === 'integer').map((p) => p.value).join('');
  if (integer !== '1') return null;
  const prev = parts[compactIdx - 1];
  const separator = prev && prev.type === 'literal' ? prev.value : '';
  return { separator, word: parts[compactIdx].value.replace(/\.$/, '') };
}

/// Compact number: |n| < 10 000 → full grouped integer ("8 421"), then
/// thousands / millions / billions with the locale's compact unit word
/// ("12,4 tys", "121 mln", "3,2 mld" / "12.4K", "121M", "3.2B"). One decimal
/// while the scaled value is < 100, none otherwise; trailing ",0" is never
/// emitted. Locales without a compact word for a scale fall back to the raw
/// Intl compact output.
export function fmtCompact(n, lang) {
  const value = Number(n);
  if (!Number.isFinite(value)) return '—';
  const locale = resolveLang(lang);
  const abs = Math.abs(value);
  if (abs < 1e4) {
    return new Intl.NumberFormat(locale, { useGrouping: 'always', maximumFractionDigits: 0 }).format(value);
  }
  const scale = COMPACT_SCALES.find((s) => abs >= s);
  const unit = compactUnit(locale, scale);
  if (!unit) {
    return new Intl.NumberFormat(locale, {
      notation: 'compact',
      compactDisplay: 'short',
      maximumFractionDigits: 1,
    })
      .formatToParts(value)
      .map((p) => (p.type === 'compact' ? p.value.replace(/\.$/, '') : p.value))
      .join('');
  }
  const scaled = value / scale;
  const digits = new Intl.NumberFormat(locale, { maximumFractionDigits: Math.abs(scaled) < 100 ? 1 : 0 }).format(scaled);
  return `${digits}${unit.separator}${unit.word}`;
}

/// Exact integer with locale thousands separators ("52 108 440").
export function fmtExact(n, lang) {
  const value = Number(n);
  if (!Number.isFinite(value)) return '—';
  return new Intl.NumberFormat(resolveLang(lang), { useGrouping: 'always', maximumFractionDigits: 0 }).format(value);
}

/// Currency amount in the locale's format ("1 044,18 zł").
export function fmtCurrency(n, currency = 'PLN', lang) {
  const value = Number(n);
  if (!Number.isFinite(value)) return '—';
  return new Intl.NumberFormat(resolveLang(lang), { style: 'currency', currency, useGrouping: 'always' }).format(value);
}

/// Fraction (0..1) as a percentage string ("0,08%" for fmtPct(0.0008, 2)).
export function fmtPct(fraction, digits = 1, lang) {
  const value = Number(fraction);
  if (!Number.isFinite(value)) return '—';
  const text = (value * 100).toLocaleString(resolveLang(lang), {
    maximumFractionDigits: digits,
    minimumFractionDigits: 0,
  });
  return `${text}%`;
}

/// Latency: "204 ms" below one second, otherwise seconds with one decimal ("1,4 s").
export function fmtMs(ms, lang) {
  const value = Number(ms);
  if (!Number.isFinite(value)) return '—';
  const locale = resolveLang(lang);
  if (Math.abs(value) < 1000) {
    return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value)} ms`;
  }
  return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(value / 1000)} s`;
}

/// Duration (audio length etc.): "4,1 h" / "12 min" / "40 s".
export function fmtDuration(ms, lang) {
  const value = Number(ms);
  if (!Number.isFinite(value)) return '—';
  const locale = resolveLang(lang);
  const abs = Math.abs(value);
  if (abs >= 3600e3) {
    return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 1 }).format(value / 3600e3)} h`;
  }
  if (abs >= 60e3) {
    return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value / 60e3)} min`;
  }
  return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 0 }).format(value / 1000)} s`;
}
