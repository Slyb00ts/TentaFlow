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

export function toast(message, kind = 'info', timeoutMs = 4000) {
  const cn = ensureToastContainer();
  const t = document.createElement('div');
  t.className = `toast toast-${kind}`;
  t.textContent = message;
  cn.appendChild(t);
  setTimeout(() => {
    t.style.opacity = '0';
    setTimeout(() => t.remove(), 200);
  }, timeoutMs);
}

export function bytesToHex(bytes) {
  return Array.from(bytes).map((b) => b.toString(16).padStart(2, '0')).join('');
}

export function shortHex(bytes, len = 8) {
  return bytesToHex(bytes).slice(0, len);
}
