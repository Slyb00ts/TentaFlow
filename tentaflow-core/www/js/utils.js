// =============================================================================
// Plik: utils.js
// Opis: Helpery: escapeHtml, formatDate, byId, toast.
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

export function formatRelative(epochSeconds) {
  if (!epochSeconds) return '—';
  const diffSec = Math.floor(Date.now() / 1000) - Number(epochSeconds);
  if (diffSec < 60) return `${diffSec}s temu`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m temu`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h temu`;
  return `${Math.floor(diffSec / 86400)}d temu`;
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
