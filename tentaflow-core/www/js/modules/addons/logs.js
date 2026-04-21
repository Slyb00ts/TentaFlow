// =============================================================================
// Plik: modules/addons/logs.js
// Opis: Tab Logs dla detail addona (admin). Przeglad wpisow audytu addona
//       z wyszukiwaniem, filtrem level i paginacja. Backend: AddonLogsRequest.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

const PAGE_SIZE = 50;

let currentAddonId = null;
let currentSearch = '';
let currentLevel = '';
let currentOffset = 0;
let currentTotal = 0;
let currentEntries = [];

export const LogsTab = {
  async mount(container, addonId) {
    currentAddonId = addonId;
    currentSearch = '';
    currentLevel = '';
    currentOffset = 0;
    renderShell(container);
    await loadAndRender(container);
  },

  unmount() {
    currentAddonId = null;
    currentEntries = [];
  },
};

function renderShell(container) {
  container.innerHTML = `
    <div class="card" style="padding:14px;margin-bottom:12px;display:flex;gap:10px;flex-wrap:wrap;align-items:center;">
      <tf-searchbox id="logs-search" placeholder="${escapeAttr(I18n.t('addon_logs.search_placeholder'))}" debounce="250"></tf-searchbox>
      <tf-select id="logs-level">
        <tf-option value="" selected>${escapeHtml(I18n.t('addon_logs.all_levels'))}</tf-option>
        <tf-option value="info">${escapeHtml(I18n.t('addon_logs.info'))}</tf-option>
        <tf-option value="warn">${escapeHtml(I18n.t('addon_logs.warn'))}</tf-option>
        <tf-option value="error">${escapeHtml(I18n.t('addon_logs.error'))}</tf-option>
      </tf-select>
    </div>
    <div id="logs-table-wrap"></div>
    <div id="logs-pagination" style="display:flex;justify-content:space-between;align-items:center;margin-top:12px;color:var(--text-3);font-size:12px;"></div>
  `;

  container.querySelector('#logs-search')?.addEventListener('search', (e) => {
    currentSearch = String(e.detail?.value ?? '').trim();
    currentOffset = 0;
    loadAndRender(container);
  });
  container.querySelector('#logs-level')?.addEventListener('change', (e) => {
    currentLevel = String(e.detail?.value ?? '');
    currentOffset = 0;
    loadAndRender(container);
  });
}

async function loadAndRender(container) {
  const wrap = container.querySelector('#logs-table-wrap');
  const pg = container.querySelector('#logs-pagination');
  if (wrap) wrap.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('common.loading'))}</div>`;
  try {
    const payload = {
      addonId: currentAddonId,
      limit: PAGE_SIZE,
      offset: currentOffset,
    };
    if (currentLevel) payload.level = currentLevel;
    if (currentSearch) payload.search = currentSearch;
    const resp = await ApiBinary.one('addonLogsRequest', payload);
    currentEntries = Array.isArray(resp.entries) ? resp.entries : [];
    currentTotal = Number(resp.total ?? 0);
    renderTable(wrap);
    renderPagination(pg, container);
  } catch (err) {
    if (wrap) {
      wrap.innerHTML = `<div class="addons-empty" style="color:var(--danger);">${escapeHtml(err.message)}</div>`;
    }
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function renderTable(wrap) {
  if (!wrap) return;
  if (currentEntries.length === 0) {
    wrap.innerHTML = `<div class="addons-empty">${escapeHtml(I18n.t('addon_logs.empty'))}</div>`;
    return;
  }
  const rows = currentEntries.map((e) => {
    const ts = formatTimestamp(e.timestamp);
    const level = String(e.level ?? 'info').toLowerCase();
    const user = e.userName ?? e.user_name ?? (e.userId ?? e.user_id ? `#${e.userId ?? e.user_id}` : '—');
    const details = String(e.details ?? '');
    const detailsAttr = details ? `data-details="${escapeAttr(details)}"` : '';
    return `
      <tr ${detailsAttr}>
        <td style="font-family:'SF Mono',monospace;font-size:12px;white-space:nowrap;">${escapeHtml(ts)}</td>
        <td><tf-chip status="${levelStatus(level)}">${escapeHtml(I18n.t('addon_logs.' + (level || 'info')))}</tf-chip></td>
        <td style="font-family:'SF Mono',monospace;font-size:12px;">${escapeHtml(String(e.action ?? ''))}</td>
        <td>${escapeHtml(String(user))}</td>
        <td>${escapeHtml(String(e.message ?? ''))}</td>
        <td>${details ? `<tf-button variant="ghost" icon="info" data-action="details">${escapeHtml(I18n.t('addon_logs.details'))}</tf-button>` : ''}</td>
      </tr>
    `;
  }).join('');

  wrap.innerHTML = `
    <div class="card" style="padding:0;overflow:auto;">
      <table class="tf-table" style="width:100%;border-collapse:collapse;">
        <thead>
          <tr>
            <th style="text-align:left;padding:8px 10px;">${escapeHtml(I18n.t('addon_logs.col_timestamp'))}</th>
            <th style="text-align:left;padding:8px 10px;">${escapeHtml(I18n.t('addon_logs.col_level'))}</th>
            <th style="text-align:left;padding:8px 10px;">${escapeHtml(I18n.t('addon_logs.col_action'))}</th>
            <th style="text-align:left;padding:8px 10px;">${escapeHtml(I18n.t('addon_logs.col_user'))}</th>
            <th style="text-align:left;padding:8px 10px;">${escapeHtml(I18n.t('addon_logs.col_message'))}</th>
            <th style="text-align:right;padding:8px 10px;"></th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    </div>
  `;

  wrap.querySelectorAll('tr[data-details]').forEach((tr) => {
    tr.querySelector('tf-button[data-action="details"]')?.addEventListener('click', () => {
      showDetailsWindow(tr.dataset.details || '');
    });
  });
}

function renderPagination(pg, container) {
  if (!pg) return;
  const from = currentTotal === 0 ? 0 : currentOffset + 1;
  const to = Math.min(currentOffset + currentEntries.length, currentTotal);
  const hasPrev = currentOffset > 0;
  const hasNext = currentOffset + currentEntries.length < currentTotal;

  pg.innerHTML = `
    <div>${from}-${to} / ${currentTotal}</div>
    <div style="display:flex;gap:8px;">
      <tf-button variant="ghost" id="logs-prev" ${hasPrev ? '' : 'disabled'}>‹</tf-button>
      <tf-button variant="ghost" id="logs-next" ${hasNext ? '' : 'disabled'}>›</tf-button>
    </div>
  `;

  pg.querySelector('#logs-prev')?.addEventListener('click', () => {
    if (currentOffset > 0) {
      currentOffset = Math.max(0, currentOffset - PAGE_SIZE);
      loadAndRender(container);
    }
  });
  pg.querySelector('#logs-next')?.addEventListener('click', () => {
    if (currentOffset + currentEntries.length < currentTotal) {
      currentOffset += PAGE_SIZE;
      loadAndRender(container);
    }
  });
}

function showDetailsWindow(details) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('addon_logs.details'));
  win.setAttribute('icon', 'info');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '560');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = `<pre style="white-space:pre-wrap;font-family:'SF Mono',monospace;font-size:12px;margin:0;">${escapeHtml(details)}</pre>`;
  win.appendChild(body);
  document.body.appendChild(win);
}

function levelStatus(level) {
  if (level === 'error') return 'err';
  if (level === 'warn' || level === 'warning') return 'warn';
  return 'info';
}

function formatTimestamp(ts) {
  if (ts == null) return '—';
  // backend moze wysylac ISO string lub unix seconds/millis
  if (typeof ts === 'number') {
    const ms = ts > 1e12 ? ts : ts * 1000;
    return new Date(ms).toISOString().replace('T', ' ').slice(0, 19);
  }
  const s = String(ts);
  const d = new Date(s);
  if (!Number.isNaN(d.getTime())) {
    return d.toISOString().replace('T', ' ').slice(0, 19);
  }
  return s;
}
