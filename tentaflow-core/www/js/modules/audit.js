// =============================================================================
// Plik: modules/audit.js
// Opis: Ekran dziennika audytu (admin). Lista wpisow z filtrami (severity/
//       akcja/user/data/search), paginacja, eksport CSV, cleanup starych.
//       Komunikacja wylacznie przez binary protocol (AuditLog*Request).
//       Live update laczy server-push AuditEvent (prepend nowych zdarzen)
//       z polling co 10s (fallback dla sync z baza po odlaczeniu WS).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, toast, formatDate } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';

// Stale modulu
const PAGE_SIZE = 100;
const REFRESH_MS = 10_000;

// Stan
let entries = [];
let totalCount = 0;
let offset = 0;
let filters = {
  severity: '',
  action: '',
  userId: '',
  search: '',
  fromDate: '',
  toDate: '',
};
let refreshTimer = null;
let unsubscribePush = null;

function sprite(id) {
  return `<svg class="icon"><use href="#i-${id}"/></svg>`;
}

const AuditScreen = {
  get title() { return I18n.t('audit.title'); },

  render() {
    return `
      <div class="page-header">
        <div>
          <h1>${sprite('audit')} ${escapeHtml(I18n.t('audit.title'))}</h1>
          <div class="sub" id="audit-sub">${escapeHtml(I18n.t('common.loading'))}</div>
        </div>
        <div class="actions">
          <tf-button variant="ghost" icon="download" id="audit-export">
            ${escapeHtml(I18n.t('audit.export_csv'))}
          </tf-button>
          <tf-button variant="ghost" icon="trash" id="audit-cleanup">
            ${escapeHtml(I18n.t('audit.cleanup'))}
          </tf-button>
        </div>
      </div>

      <div class="card" style="padding: 14px; margin-bottom: 14px;">
        <div class="audit-filters" style="display: grid; grid-template-columns: 2fr 1fr 1fr 1fr 1fr 1fr auto; gap: 10px; align-items: center;">
          <tf-searchbox id="audit-f-search" placeholder="${escapeAttr(I18n.t('audit.filter_search'))}" debounce="300"></tf-searchbox>
          <tf-select id="audit-f-severity" value="">
            <option value="">${escapeHtml(I18n.t('audit.severity'))}</option>
            <option value="info">info</option>
            <option value="ok">ok</option>
            <option value="warn">warn</option>
            <option value="err">err</option>
          </tf-select>
          <tf-input id="audit-f-action" placeholder="${escapeAttr(I18n.t('audit.filter_action'))}"></tf-input>
          <tf-input id="audit-f-user" type="number" placeholder="${escapeAttr(I18n.t('audit.filter_user'))}"></tf-input>
          <tf-input id="audit-f-from" type="datetime-local" placeholder="${escapeAttr(I18n.t('audit.filter_from'))}"></tf-input>
          <tf-input id="audit-f-to" type="datetime-local" placeholder="${escapeAttr(I18n.t('audit.filter_to'))}"></tf-input>
          <tf-button variant="ghost" id="audit-f-clear">${escapeHtml(I18n.t('audit.clear_filters'))}</tf-button>
        </div>
      </div>

      <div class="card" style="padding: 0; overflow: hidden;">
        <div class="audit-row" style="background: var(--bg-2); color: var(--text-3); font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; font-size: 10px;">
          <span>${escapeHtml(I18n.t('audit.timestamp'))}</span>
          <span>${escapeHtml(I18n.t('audit.severity'))}</span>
          <span>${escapeHtml(I18n.t('audit.event'))}</span>
          <span>${escapeHtml(I18n.t('audit.actor'))}</span>
          <span>${escapeHtml(I18n.t('audit.details'))}</span>
        </div>
        <div id="audit-body"></div>
      </div>

      <div id="audit-pagination" style="display: flex; justify-content: center; align-items: center; gap: 12px; margin-top: 14px;"></div>
    `;
  },

  async mount() {
    attachFilterHandlers();
    byId('audit-export')?.addEventListener('click', onExport);
    byId('audit-cleanup')?.addEventListener('click', onCleanup);

    renderBody();
    renderPagination();
    await loadEntries();

    refreshTimer = setInterval(loadEntries, REFRESH_MS);

    try {
      const client = await ApiBinary.client();
      unsubscribePush = client.addUnsolicitedListener(({ body }) => {
        if (body?.variant === 'AuditEvent' && offset === 0 && filtersAreEmpty()) {
          prependLiveEvent(body);
        }
      });
    } catch (_) {
      // brak server-push — polling i tak podnosi nowe zdarzenia.
    }
  },

  unmount() {
    if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null; }
    if (unsubscribePush) { unsubscribePush(); unsubscribePush = null; }
    entries = [];
    totalCount = 0;
    offset = 0;
    filters = { severity: '', action: '', userId: '', search: '', fromDate: '', toDate: '' };
  },
};

function attachFilterHandlers() {
  const apply = () => { offset = 0; loadEntries(); };

  byId('audit-f-search')?.addEventListener('search', (e) => {
    filters.search = e.detail?.value ?? '';
    apply();
  });
  byId('audit-f-severity')?.addEventListener('change', (e) => {
    filters.severity = e.detail?.value ?? '';
    apply();
  });
  byId('audit-f-action')?.addEventListener('change', (e) => {
    filters.action = e.detail?.value ?? '';
    apply();
  });
  byId('audit-f-user')?.addEventListener('change', (e) => {
    filters.userId = e.detail?.value ?? '';
    apply();
  });
  byId('audit-f-from')?.addEventListener('change', (e) => {
    filters.fromDate = e.detail?.value ?? '';
    apply();
  });
  byId('audit-f-to')?.addEventListener('change', (e) => {
    filters.toDate = e.detail?.value ?? '';
    apply();
  });
  byId('audit-f-clear')?.addEventListener('click', () => {
    filters = { severity: '', action: '', userId: '', search: '', fromDate: '', toDate: '' };
    for (const id of ['audit-f-search', 'audit-f-severity', 'audit-f-action', 'audit-f-user', 'audit-f-from', 'audit-f-to']) {
      const el = byId(id);
      if (el) el.setAttribute('value', '');
    }
    apply();
  });
}

function filtersAreEmpty() {
  return !filters.severity && !filters.action && !filters.userId && !filters.search && !filters.fromDate && !filters.toDate;
}

// Buduje payload dla binary requesta. Wartosci puste = null po stronie protokolu.
function buildFilterPayload() {
  const f = {
    userId: filters.userId !== '' ? Number(filters.userId) || null : null,
    addonId: null,
    action: filters.action || null,
    fromDate: filters.fromDate ? new Date(filters.fromDate).toISOString() : null,
    toDate: filters.toDate ? new Date(filters.toDate).toISOString() : null,
    search: filters.search || null,
  };
  return f;
}

async function loadEntries() {
  try {
    const payload = { ...buildFilterPayload(), offset, limit: PAGE_SIZE };
    const resp = await ApiBinary.one('auditLogListRequest', payload);
    let rows = resp.entries || [];
    if (filters.severity) {
      rows = rows.filter(e => severityForAction(e.action) === filters.severity);
    }
    entries = rows;
    totalCount = Number(resp.totalCount ?? rows.length);
    renderBody();
    renderPagination();
    updateSubtitle();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

function prependLiveEvent(ev) {
  entries.unshift({
    id: 0,
    timestamp: new Date((ev.tsEpoch || 0) * 1000).toISOString(),
    action: ev.eventKind,
    userId: null,
    addonId: null,
    resource: ev.resourceId ?? null,
    details: ev.message ?? null,
    ipAddress: null,
    nodeId: null,
  });
  if (entries.length > PAGE_SIZE) entries.length = PAGE_SIZE;
  totalCount += 1;
  renderBody();
  renderPagination();
  updateSubtitle();
}

// Mapowanie nazwy akcji na severity chip. Wzorzec z wireframe + istniejacy
// stary kod (modules/audit/AuditBinary.js chipStatusForKind).
function severityForAction(action) {
  const a = String(action || '').toLowerCase();
  if (a.includes('error') || a.includes('fail') || a.includes('deny') || a.includes('reject')) return 'err';
  if (a.includes('warn') || a.includes('rate') || a.includes('offline')) return 'warn';
  if (a.includes('success') || a.includes('ok') || a.includes('create') || a.includes('pair') || a.includes('rotation') || a.includes('accept')) return 'ok';
  return 'info';
}

function renderBody() {
  const host = byId('audit-body');
  if (!host) return;
  if (entries.length === 0) {
    host.innerHTML = `<div class="empty-state" style="padding: 36px;">
      <div class="empty-state-text">${escapeHtml(I18n.t('audit.no_entries'))}</div>
    </div>`;
    return;
  }
  host.innerHTML = entries.map((e, idx) => {
    const sev = severityForAction(e.action);
    const event = buildEventText(e);
    const actor = buildActorText(e);
    const details = buildDetailsText(e);
    return `
      <div class="audit-row" data-idx="${idx}" style="cursor: pointer;">
        <span class="ts">${escapeHtml(formatTimestamp(e.timestamp))}</span>
        <span class="audit-sev ${sev}">${escapeHtml(I18n.t('audit.sev_' + sev))}</span>
        <span>${escapeHtml(event)}</span>
        <span>${escapeHtml(actor)}</span>
        <span style="color: var(--text-3);">${escapeHtml(details)}</span>
      </div>`;
  }).join('');

  host.querySelectorAll('.audit-row').forEach((el) => {
    el.addEventListener('click', () => {
      const idx = Number(el.dataset.idx);
      if (!Number.isNaN(idx) && entries[idx]) openEntryDetail(entries[idx]);
    });
  });
}

function buildEventText(e) {
  const parts = [e.action];
  if (e.resource) parts.push(e.resource);
  return parts.filter(Boolean).join(' · ');
}

function buildActorText(e) {
  if (e.userId != null) return `user #${e.userId}`;
  if (e.addonId) return `addon ${e.addonId}`;
  if (e.nodeId) return `node ${String(e.nodeId).slice(0, 12)}`;
  return 'system';
}

function buildDetailsText(e) {
  const bits = [];
  if (e.ipAddress) bits.push(`IP: ${e.ipAddress}`);
  if (e.details) {
    const compact = String(e.details).replace(/\s+/g, ' ').trim();
    bits.push(compact.length > 80 ? compact.slice(0, 77) + '…' : compact);
  }
  return bits.join(' · ') || '—';
}

function formatTimestamp(ts) {
  if (!ts) return '-';
  const d = new Date(ts.includes('T') ? ts : ts.replace(' ', 'T') + 'Z');
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString('pl-PL', {
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
  });
}

function renderPagination() {
  const pag = byId('audit-pagination');
  if (!pag) return;
  const pages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE));
  const current = Math.floor(offset / PAGE_SIZE) + 1;
  pag.innerHTML = `
    <tf-button variant="ghost" size="sm" id="audit-prev" ${offset === 0 ? 'disabled' : ''}>
      ${escapeHtml(I18n.t('audit.prev'))}
    </tf-button>
    <span style="color: var(--text-3); font-size: 12px;">
      ${escapeHtml(I18n.t('audit.page_of', { page: current, pages }))}
    </span>
    <tf-button variant="ghost" size="sm" id="audit-next" ${offset + PAGE_SIZE >= totalCount ? 'disabled' : ''}>
      ${escapeHtml(I18n.t('audit.next'))}
    </tf-button>
  `;
  byId('audit-prev')?.addEventListener('click', () => {
    offset = Math.max(0, offset - PAGE_SIZE);
    loadEntries();
  });
  byId('audit-next')?.addEventListener('click', () => {
    if (offset + PAGE_SIZE < totalCount) {
      offset += PAGE_SIZE;
      loadEntries();
    }
  });
}

function updateSubtitle() {
  const sub = byId('audit-sub');
  if (sub) sub.textContent = I18n.t('audit.subtitle', { total: totalCount });
}

function openEntryDetail(entry) {
  const prettyDetails = entry.details ? tryPrettyJson(entry.details) : '—';
  const rows = [
    ['id', entry.id],
    [I18n.t('audit.timestamp'), formatTimestamp(entry.timestamp)],
    [I18n.t('audit.action'), entry.action],
    [I18n.t('audit.user'), entry.userId ?? '—'],
    [I18n.t('audit.addon'), entry.addonId ?? '—'],
    [I18n.t('audit.resource'), entry.resource ?? '—'],
    [I18n.t('audit.ip'), entry.ipAddress ?? '—'],
    [I18n.t('audit.node'), entry.nodeId ?? '—'],
  ];
  const html = `
    <div style="display: grid; grid-template-columns: 140px 1fr; gap: 8px 16px; font-size: 13px;">
      ${rows.map(([k, v]) => `
        <div style="color: var(--text-3);">${escapeHtml(String(k))}</div>
        <div style="font-family: 'SF Mono', monospace; word-break: break-all;">${escapeHtml(String(v))}</div>
      `).join('')}
    </div>
    <div style="margin-top: 14px;">
      <div style="color: var(--text-3); font-size: 12px; margin-bottom: 6px;">${escapeHtml(I18n.t('audit.details'))}</div>
      <pre style="background: var(--bg-2); padding: 10px; border-radius: 6px; overflow: auto; max-height: 320px; font-size: 12px; white-space: pre-wrap; word-break: break-all;">${escapeHtml(prettyDetails)}</pre>
    </div>
  `;
  TfWindow.open({
    title: I18n.t('audit.detail_title', { id: entry.id }),
    content: html,
    width: 640,
  });
}

function tryPrettyJson(s) {
  try {
    const parsed = JSON.parse(s);
    return JSON.stringify(parsed, null, 2);
  } catch {
    return String(s);
  }
}

async function onExport() {
  try {
    const resp = await ApiBinary.one('auditLogExportRequest', buildFilterPayload());
    const csv = resp.csv || '';
    const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    const stamp = new Date().toISOString().slice(0, 10).replace(/-/g, '');
    a.href = url;
    a.download = `audit-${stamp}.csv`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    toast(I18n.t('audit.export_done', { rows: resp.rowCount ?? 0 }), 'success');
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

async function onCleanup() {
  const raw = window.prompt(I18n.t('audit.cleanup_prompt'), '90');
  if (raw == null) return;
  const days = parseInt(raw, 10);
  if (Number.isNaN(days) || days < 1) {
    toast(I18n.t('common.invalid_input') || 'invalid', 'error');
    return;
  }
  const ok = await TfWindow.confirm({
    title: I18n.t('audit.cleanup_confirm_title'),
    message: I18n.t('audit.cleanup_confirm_msg', { days }),
    confirmLabel: I18n.t('audit.cleanup'),
    cancelLabel: I18n.t('common.cancel'),
    danger: true,
  });
  if (!ok) return;
  try {
    const resp = await ApiBinary.one('auditLogCleanupRequest', { keepDays: days });
    toast(I18n.t('audit.cleanup_done', { count: resp.deletedCount ?? 0 }), 'success');
    offset = 0;
    await loadEntries();
  } catch (err) {
    toast(`${I18n.t('common.error')}: ${err.message}`, 'error');
  }
}

export default AuditScreen;
