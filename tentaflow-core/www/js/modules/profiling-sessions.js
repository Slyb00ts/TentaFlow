// =============================================================================
// Plik: modules/profiling-sessions.js
// Opis: Widok listy sesji multi-source profilingu. Renderuje toolbar z search
//       + filter chips, tabele sesji, akcje (open/download/delete) oraz
//       empty-state. Zrodlo danych: /api/profiling/sessions lub fixture.
// =============================================================================

import { TfWindow } from '/js/components/tf-window.js';
import { ProfilingLaunchModal } from '/js/modules/profiling-launch.js';
import {
  profilingSessions,
  profilingDelete,
  profilingDownload,
  profilingStop,
} from '/js/protocol/profiling.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-chip.js';

// Krotki helper i18n z fallbackiem do angielskiego stringa.
function t(key, vars, fallback) {
  const v = I18n.t(key, vars || null);
  return v === key && fallback != null ? fallback : v;
}

function fixtureMode() {
  return typeof window !== 'undefined' && window.__TF_PROFILING_FIXTURE === true;
}

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function formatBytes(b) {
  if (!Number.isFinite(b) || b <= 0) return '—';
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(0)} MB`;
  return `${(b / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatDuration(sec) {
  if (!Number.isFinite(sec) || sec < 0) return '—';
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

function formatRelative(unixNs) {
  if (!Number.isFinite(unixNs) || unixNs <= 0) return '—';
  const ms = Math.floor(unixNs / 1_000_000);
  const diff = Date.now() - ms;
  if (diff < 60_000) return t('profiling.sessions.rel_just_now', null, 'just now');
  if (diff < 3600_000) return t('profiling.sessions.rel_min_ago', { n: Math.floor(diff / 60_000) }, `${Math.floor(diff / 60_000)} min ago`);
  if (diff < 86400_000) return t('profiling.sessions.rel_hours_ago', { n: Math.floor(diff / 3600_000) }, `${Math.floor(diff / 3600_000)} hours ago`);
  if (diff < 7 * 86400_000) return t('profiling.sessions.rel_days_ago', { n: Math.floor(diff / 86400_000) }, `${Math.floor(diff / 86400_000)} days ago`);
  return new Date(ms).toLocaleDateString();
}

function formatAbsolute(unixNs) {
  if (!Number.isFinite(unixNs) || unixNs <= 0) return '—';
  return new Date(Math.floor(unixNs / 1_000_000)).toLocaleString();
}

// Mockup-style "started" — top line w kolumnie Started.
//  - dzis: HH:MM:SS
//  - wczoraj: "Yesterday HH:MM"
//  - 2-6 dni: "N days ago"
//  - >7 dni: lokalna data
function formatStarted(unixNs) {
  if (!Number.isFinite(unixNs) || unixNs <= 0) return '—';
  const ms = Math.floor(unixNs / 1_000_000);
  const date = new Date(ms);
  const now = new Date();
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const startOfYesterday = startOfToday - 86400_000;
  if (ms >= startOfToday) {
    const hh = String(date.getHours()).padStart(2, '0');
    const mm = String(date.getMinutes()).padStart(2, '0');
    const ss = String(date.getSeconds()).padStart(2, '0');
    return `${hh}:${mm}:${ss}`;
  }
  if (ms >= startOfYesterday) {
    const hh = String(date.getHours()).padStart(2, '0');
    const mm = String(date.getMinutes()).padStart(2, '0');
    return t('profiling.sessions.rel_yesterday', { time: `${hh}:${mm}` }, `Yesterday ${hh}:${mm}`);
  }
  const daysAgo = Math.floor((startOfToday - ms) / 86400_000) + 1;
  if (daysAgo < 7) return t('profiling.sessions.rel_days_ago', { n: daysAgo }, `${daysAgo} days ago`);
  return date.toLocaleDateString();
}

// Bottom "lc-sub" line — pokazujemy zegarek HH:MM dla starszych wpisow,
// a dla swiezych "N min ago" / "N hours ago" (mockup ma oba warianty).
function formatStartedSub(unixNs) {
  if (!Number.isFinite(unixNs) || unixNs <= 0) return '';
  const ms = Math.floor(unixNs / 1_000_000);
  const diff = Date.now() - ms;
  if (diff < 86400_000) return formatRelative(unixNs);
  // dla starszych: zegarek w lokalnym czasie
  const date = new Date(ms);
  const hh = String(date.getHours()).padStart(2, '0');
  const mm = String(date.getMinutes()).padStart(2, '0');
  return `${hh}:${mm}`;
}

// Live elapsed dla running session (sekundy od start).
function elapsedSeconds(unixNs) {
  if (!Number.isFinite(unixNs) || unixNs <= 0) return 0;
  return Math.max(0, Math.floor((Date.now() - unixNs / 1_000_000) / 1000));
}

function statusIcon(status) {
  if (status === 'running') {
    return `<span class="row-status-ico run" title="${escapeHtml(t('profiling.sessions.status_running', null, 'Running'))}">
      <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/></svg>
    </span>`;
  }
  if (status === 'failed') {
    return `<span class="row-status-ico fail" title="${escapeHtml(t('profiling.sessions.status_failed', null, 'Failed'))}">
      <svg viewBox="0 0 24 24"><path d="M18 6L6 18M6 6l12 12"/></svg>
    </span>`;
  }
  if (status === 'partial') {
    return `<span class="row-status-ico warn" title="${escapeHtml(t('profiling.sessions.status_partial', null, 'Partial'))}">
      <svg viewBox="0 0 24 24"><path d="M12 2L2 22h20L12 2z"/><path d="M12 9v6M12 18h.01"/></svg>
    </span>`;
  }
  return `<span class="row-status-ico ok" title="${escapeHtml(t('profiling.sessions.status_completed', null, 'Completed'))}">
    <svg viewBox="0 0 24 24"><path d="M5 13l4 4L19 7"/></svg>
  </span>`;
}

function srcChips(sourcesUsed) {
  if (!Array.isArray(sourcesUsed) || sourcesUsed.length === 0) return '';
  // Mockup #03: do 5 chipow + "+N more" gdy wiecej.
  const max = 5;
  const visible = sourcesUsed.slice(0, max);
  const rest = sourcesUsed.length - visible.length;
  const chips = visible.map((s) => {
    const isSkipped = s.status === 'skipped' || s.status === 'warn';
    const isBad = s.status === 'bad' || s.status === 'failed';
    const cls = isBad ? 'bad' : isSkipped ? 'warn' : '';
    let label = s.label || s.id;
    // Dla skipped sources doklej "(skipped)" zgodnie z mockupem, jezeli backend
    // jeszcze nie dolozyl tego do labela.
    if (isSkipped && !/skipped/i.test(label)) label = `${label} (${t('profiling.sessions.skipped_suffix', null, 'skipped')})`;
    return `<span class="src-chip-mini ${cls}">${escapeHtml(label)}</span>`;
  }).join('');
  const more = rest > 0 ? `<span class="src-chip-more">${escapeHtml(t('profiling.sessions.more_count', { n: rest }, `+${rest} more`))}</span>` : '';
  return chips + more;
}

function getFilters() {
  return [
    { id: 'all', label: t('profiling.sessions.filter_all', null, 'All') },
    { id: 'last_24h', label: t('profiling.sessions.filter_last_24h', null, 'Last 24h') },
    { id: 'this_week', label: t('profiling.sessions.filter_this_week', null, 'This week') },
    { id: 'has_flamegraph', label: t('profiling.sessions.filter_has_flamegraph', null, 'Has flamegraph') },
    { id: 'multi_gpu', label: t('profiling.sessions.filter_multi_gpu', null, 'Multi-GPU') },
    { id: 'failed', label: t('profiling.sessions.filter_failed', null, 'Failed') },
  ];
}

function applyFilters(sessions, activeFilter, searchTerm) {
  const term = (searchTerm || '').toLowerCase().trim();
  const now = Date.now();
  return sessions.filter((s) => {
    if (term && !String(s.label || '').toLowerCase().includes(term)) return false;
    switch (activeFilter) {
      case 'last_24h': {
        const ms = Math.floor((s.started_at_unix_ns || 0) / 1_000_000);
        return ms >= now - 86400_000;
      }
      case 'this_week': {
        const ms = Math.floor((s.started_at_unix_ns || 0) / 1_000_000);
        return ms >= now - 7 * 86400_000;
      }
      case 'has_flamegraph':
        return !!s.has_flamegraph;
      case 'multi_gpu':
        return (s.gpu_count || 0) >= 2;
      case 'failed':
        return s.status === 'failed';
      default:
        return true;
    }
  });
}

// Maps a binary-protocol entry (camelCase, ProfilingSessionEntry) to the
// snake_case shape used by the table renderer. Fixture JSON is already in
// snake_case so we only normalize when fields are absent.
function normalizeEntry(e) {
  if (e == null) return e;
  if ('session_id' in e) return e;
  const startedMs = e.startedAt ? Date.parse(e.startedAt) : 0;
  const startedNs = Number.isFinite(startedMs) && startedMs > 0
    ? startedMs * 1_000_000
    : 0;
  return {
    session_id: e.sessionId,
    label: e.label,
    status: 'completed',
    started_at_unix_ns: startedNs,
    duration_seconds: Number(e.durationNs || 0) / 1_000_000_000,
    size_bytes: Number(e.sizeBytes || 0),
    sources_used: Array.isArray(e.collectorsUsed)
      ? e.collectorsUsed.map((id) => ({ id, label: id, status: 'ok' }))
      : [],
    has_flamegraph: false,
    gpu_count: 0,
    kind: e.kind,
  };
}

async function fetchSessions(nodeId) {
  if (fixtureMode()) {
    const resp = await fetch('/js/modules/__fixtures__/profiling-sessions.json', { cache: 'no-store' });
    if (!resp.ok) throw new Error(`fixture HTTP ${resp.status}`);
    const data = await resp.json();
    // honor local "deleted" mark from prior delete actions
    const deleted = readDeletedSet();
    return (data.sessions || []).filter((s) => !deleted.has(s.session_id));
  }
  const data = await profilingSessions({ nodeId });
  return (data.entries || []).map(normalizeEntry);
}

async function deleteSession(nodeId, sessionId) {
  if (fixtureMode()) {
    const set = readDeletedSet();
    set.add(sessionId);
    writeDeletedSet(set);
    return;
  }
  await profilingDelete({ nodeId, sessionId });
}

const DELETED_KEY = 'tf-profiling-fixture-deleted';

function readDeletedSet() {
  try {
    const raw = localStorage.getItem(DELETED_KEY);
    if (!raw) return new Set();
    return new Set(JSON.parse(raw));
  } catch (_e) {
    return new Set();
  }
}

function writeDeletedSet(set) {
  try {
    localStorage.setItem(DELETED_KEY, JSON.stringify(Array.from(set)));
  } catch (_e) {
    // ignore quota issues; fixture-only state
  }
}

function showToast(msg, type = 'info') {
  // Lekki, lokalny toast — globalny system toastow nie jest tu importowany,
  // by uniknac sztywnej zaleznosci. Pokazujemy banner u dolu strony.
  const el = document.createElement('div');
  el.textContent = msg;
  el.style.cssText = `
    position: fixed; bottom: 24px; left: 50%; transform: translateX(-50%);
    background: var(--tf-bg-2, #0a0d24); color: var(--tf-text, #e8ebf5);
    border: 1px solid var(--tf-border, #1f2548); border-radius: 8px;
    padding: 10px 14px; font-size: 12.5px; z-index: 9999;
    box-shadow: 0 12px 32px rgba(0,0,0,0.5);
  `;
  if (type === 'error') el.style.borderColor = 'var(--tf-danger, #ef4444)';
  if (type === 'success') el.style.borderColor = 'var(--tf-success, #22c55e)';
  document.body.appendChild(el);
  setTimeout(() => el.remove(), 3200);
}

// =============================================================================
// ProfilingSessionsView — controller widoku listy sesji.
// =============================================================================

export class ProfilingSessionsView {
  /**
   * @param {object} opts
   * @param {string} opts.nodeId
   * @param {string=} opts.nodeName
   * @param {Array=} opts.availableSources do uzycia w launch modal
   * @param {Function=} opts.onOpenReport (sessionId) => void
   */
  constructor(opts = {}) {
    this.nodeId = opts.nodeId;
    this.nodeName = opts.nodeName || opts.nodeId || 'node';
    this.availableSources = Array.isArray(opts.availableSources) ? opts.availableSources : [];
    this.onOpenReport = typeof opts.onOpenReport === 'function' ? opts.onOpenReport : null;

    this.sessions = [];
    this.activeFilter = 'all';
    this.searchTerm = '';
    this.refreshTimer = null;
    // Tick co 1s — odswieza live elapsed (M:SS / M:SS) dla running sesji bez
    // pelnego refetcha.
    this.tickTimer = null;
    this.root = null;
    // Set<sessionId> wybranych do porownania (Compare). Max 2 — przy 3.
    // wybraniu pierwszy zostaje zdjety.
    this.compareSelected = new Set();
  }

  async mount(parent) {
    if (!parent) throw new Error('ProfilingSessionsView.mount requires a parent element');
    this.root = document.createElement('div');
    this.root.className = 'profiling-sessions';
    parent.appendChild(this.root);
    this._renderShell();
    await this.refresh();
    this._maybeStartAutoRefresh();
  }

  unmount() {
    if (this.refreshTimer) {
      clearInterval(this.refreshTimer);
      this.refreshTimer = null;
    }
    if (this.tickTimer) {
      clearInterval(this.tickTimer);
      this.tickTimer = null;
    }
    if (this.root && this.root.parentNode) {
      this.root.parentNode.removeChild(this.root);
    }
    this.root = null;
  }

  async refresh() {
    try {
      this.sessions = await fetchSessions(this.nodeId);
    } catch (err) {
      console.error('failed to load profiling sessions', err);
      this.sessions = [];
    }
    this._renderTable();
    this._maybeStartAutoRefresh();
  }

  _maybeStartAutoRefresh() {
    const hasRunning = this.sessions.some((s) => s.status === 'running');
    if (hasRunning && !this.refreshTimer) {
      this.refreshTimer = setInterval(() => { this.refresh(); }, 5000);
    } else if (!hasRunning && this.refreshTimer) {
      clearInterval(this.refreshTimer);
      this.refreshTimer = null;
    }
    if (hasRunning && !this.tickTimer) {
      this.tickTimer = setInterval(() => this._tickRunning(), 1000);
    } else if (!hasRunning && this.tickTimer) {
      clearInterval(this.tickTimer);
      this.tickTimer = null;
    }
  }

  // Aktualizuje DOM tylko dla running sesji (M:SS / M:SS) — bez przerysowania
  // calej tabeli. Dzieki temu countdown jest plynny, a uzytkownik nie traci
  // hover/focus na akcjach.
  _tickRunning() {
    if (!this.root) return;
    for (const s of this.sessions) {
      if (s.status !== 'running') continue;
      const cell = this.root.querySelector(`tr[data-session-id="${CSS.escape(s.session_id)}"] [data-running-elapsed]`);
      if (cell) {
        const elapsed = elapsedSeconds(s.started_at_unix_ns);
        const planned = Number.isFinite(s.duration_seconds) && s.duration_seconds > 0
          ? formatDuration(s.duration_seconds)
          : '∞';
        cell.textContent = `${formatDuration(elapsed)} / ${planned}`;
      }
    }
  }

  _renderShell() {
    // Layout 1:1 z mockup #03 (sekcja 03 SESSIONS LIST).
    // Toolbar: search | tf-filter-group (chipy) | (margin-auto) Refresh | New
    // session | Export. Brak breadcrumb/page-title — sa czescia parent screen.
    const searchPh = escapeHtml(t('profiling.sessions.search_placeholder', null, 'Search by label, session id…'));
    const compareCount = escapeHtml(t('profiling.sessions.compare_count', { n: 0 }, '0/2 selected'));
    const compareHint = escapeHtml(t('profiling.sessions.compare_hint', null, 'Pick two completed sessions to compare side-by-side.'));
    this.root.innerHTML = `
      <div class="tf-section-card">
        <div class="toolbar">
          <tf-searchbox id="ps-search" placeholder="${searchPh}"></tf-searchbox>
          <div class="tf-filter-group" id="ps-filter-chips"></div>
          <tf-button variant="ghost" size="sm" icon="refresh" id="ps-refresh" style="margin-left:auto;">${escapeHtml(t('profiling.sessions.btn_refresh', null, 'Refresh'))}</tf-button>
          <tf-button variant="primary" size="sm" icon="plus" id="ps-new">${escapeHtml(t('profiling.sessions.btn_new', null, 'New session'))}</tf-button>
          <tf-button variant="outline" size="sm" icon="download" id="ps-export">${escapeHtml(t('profiling.sessions.btn_export', null, 'Export'))}</tf-button>
        </div>

        <div id="ps-compare-bar" class="ps-compare-bar hidden">
          <span class="count">${compareCount}</span>
          <span style="color: var(--text-2, #a0a8c8);">${compareHint}</span>
          <span style="flex:1"></span>
          <tf-button variant="ghost" size="sm" id="ps-compare-clear">${escapeHtml(t('profiling.sessions.compare_clear', null, 'Clear'))}</tf-button>
          <tf-button variant="primary" size="sm" id="ps-compare-go" disabled>${escapeHtml(t('profiling.sessions.compare_go', null, 'Compare selected'))}</tf-button>
        </div>

        <div id="ps-table-wrap"></div>
      </div>
    `;

    // Render filter chips zgodnie z mockupem przez komponent tf-chip
    // (clickable + atrybut active). Globalny styl z addons.css obsluguje
    // wizualny stan aktywny.
    const chipsWrap = this.root.querySelector('#ps-filter-chips');
    for (const f of getFilters()) {
      const chip = document.createElement('tf-chip');
      chip.className = 'filter-chip';
      chip.setAttribute('clickable', '');
      chip.textContent = f.label;
      chip.setAttribute('data-filter-id', f.id);
      if (this.activeFilter === f.id) chip.setAttribute('active', '');
      chip.addEventListener('click', () => {
        this.activeFilter = f.id;
        chipsWrap.querySelectorAll('tf-chip.filter-chip').forEach((c) => {
          if (c.getAttribute('data-filter-id') === this.activeFilter) c.setAttribute('active', '');
          else c.removeAttribute('active');
        });
        this._renderTable();
      });
      chipsWrap.appendChild(chip);
    }

    const search = this.root.querySelector('#ps-search');
    if (search) {
      search.addEventListener('input', (ev) => {
        this.searchTerm = ev.target?.value || ev.detail?.value || '';
        this._renderTable();
      });
      search.addEventListener('change', (ev) => {
        this.searchTerm = ev.target?.value || ev.detail?.value || '';
        this._renderTable();
      });
    }
    const refreshBtn = this.root.querySelector('#ps-refresh');
    if (refreshBtn) refreshBtn.addEventListener('click', () => { this.refresh(); });
    const newBtn = this.root.querySelector('#ps-new');
    if (newBtn) newBtn.addEventListener('click', () => this._openLaunch());
    const exportBtn = this.root.querySelector('#ps-export');
    if (exportBtn) exportBtn.addEventListener('click', () => this._exportAll());

    const cmpClear = this.root.querySelector('#ps-compare-clear');
    if (cmpClear) cmpClear.addEventListener('click', () => {
      this.compareSelected.clear();
      this._renderTable();
      this._updateCompareBar();
    });
    const cmpGo = this.root.querySelector('#ps-compare-go');
    if (cmpGo) cmpGo.addEventListener('click', () => this._launchCompare());
  }

  _updateCompareBar() {
    const bar = this.root?.querySelector('#ps-compare-bar');
    if (!bar) return;
    const n = this.compareSelected.size;
    if (n === 0) {
      bar.classList.add('hidden');
      return;
    }
    bar.classList.remove('hidden');
    const countEl = bar.querySelector('.count');
    if (countEl) countEl.textContent = t('profiling.sessions.compare_count', { n }, `${n}/2 selected`);
    const goBtn = bar.querySelector('#ps-compare-go');
    if (goBtn) {
      if (n === 2) goBtn.removeAttribute('disabled');
      else goBtn.setAttribute('disabled', '');
    }
  }

  _toggleCompare(sessionId) {
    if (this.compareSelected.has(sessionId)) {
      this.compareSelected.delete(sessionId);
    } else {
      // Max 2 — gdy juz dwa, zdejmij najstarszy.
      if (this.compareSelected.size >= 2) {
        const first = this.compareSelected.values().next().value;
        this.compareSelected.delete(first);
      }
      this.compareSelected.add(sessionId);
    }
    this._renderTable();
    this._updateCompareBar();
  }

  _launchCompare() {
    if (this.compareSelected.size !== 2) return;
    const [sessionA, sessionB] = Array.from(this.compareSelected);
    if (window.Router && typeof window.Router.navigate === 'function') {
      window.Router.navigate('profile-compare', { nodeId: this.nodeId, sessionA, sessionB });
    }
  }

  _renderTable() {
    const wrap = this.root.querySelector('#ps-table-wrap');
    if (!wrap) return;
    const filtered = applyFilters(this.sessions, this.activeFilter, this.searchTerm);

    if (filtered.length === 0 && this.sessions.length === 0) {
      wrap.innerHTML = `
        <div class="empty-state">
          <div class="es-ico">
            <svg viewBox="0 0 24 24" fill="none" stroke-linecap="round" stroke-linejoin="round">
              <path d="M3 3v18h18"/><path d="M7 14l3-4 4 5 7-9"/>
            </svg>
          </div>
          <div class="es-title">${escapeHtml(t('profiling.sessions.empty_title', null, 'No profiling sessions yet'))}</div>
          <div class="es-sub">${escapeHtml(t('profiling.sessions.empty_sub', null, 'Launch your first multi-source profiling session to inspect CPU, GPU, memory and power together.'))}</div>
          <tf-button variant="primary" icon="record" id="ps-empty-launch">${escapeHtml(t('profiling.sessions.empty_btn', null, 'Launch first profiling'))}</tf-button>
        </div>
      `;
      const btn = wrap.querySelector('#ps-empty-launch');
      if (btn) btn.addEventListener('click', () => this._openLaunch());
      return;
    }

    if (filtered.length === 0) {
      wrap.innerHTML = `
        <div class="empty-state">
          <div class="es-title">${escapeHtml(t('profiling.sessions.empty_filtered_title', null, 'No sessions match current filters'))}</div>
          <div class="es-sub">${escapeHtml(t('profiling.sessions.empty_filtered_sub', null, 'Try clearing the search or filter chips.'))}</div>
        </div>
      `;
      return;
    }

    const rows = filtered.map((s) => this._renderRow(s)).join('');
    wrap.innerHTML = `
      <table class="tf-table">
        <thead>
          <tr>
            <th style="width: 40px"></th>
            <th>${escapeHtml(t('profiling.sessions.col_label_sources', null, 'Label / sources'))}</th>
            <th style="width: 14%">${escapeHtml(t('profiling.sessions.col_duration_size', null, 'Duration · Size'))}</th>
            <th style="width: 14%">${escapeHtml(t('profiling.sessions.col_started', null, 'Started'))}</th>
            <th class="actions-col" style="width: 16%">${escapeHtml(t('profiling.sessions.col_actions', null, 'Actions'))}</th>
          </tr>
        </thead>
        <tbody>${rows}</tbody>
      </table>
    `;
    this._attachRowListeners(wrap);
    this._updateCompareBar();
  }

  _renderRow(s) {
    const sid = escapeHtml(s.session_id);
    const ico = statusIcon(s.status);
    const labelName = escapeHtml(s.label || t('profiling.sessions.no_label', null, '(no label)'));
    const size = formatBytes(s.size_bytes);
    const startedTop = formatStarted(s.started_at_unix_ns);
    const startedSub = formatStartedSub(s.started_at_unix_ns);
    const abs = formatAbsolute(s.started_at_unix_ns);

    const isSelected = this.compareSelected.has(s.session_id);
    const canCompare = s.status === 'completed' || s.status === 'partial';
    const isRunning = s.status === 'running';
    const isFailed = s.status === 'failed';
    const isPartial = s.status === 'partial';

    // Status pill widoczny przy nazwie (REC / FAILED / PARTIAL).
    let statusPill = '';
    if (isRunning) {
      statusPill = `<span class="status-pill danger" style="margin-left:4px;">${escapeHtml(t('profiling.sessions.pill_rec', null, 'REC'))}</span>`;
    } else if (isFailed) {
      statusPill = `<span class="status-pill danger" style="margin-left:4px;">${escapeHtml(t('profiling.sessions.pill_failed', null, 'FAILED'))}</span>`;
    } else if (isPartial) {
      statusPill = `<span class="status-pill warn" style="margin-left:4px;">${escapeHtml(t('profiling.sessions.pill_partial', null, 'PARTIAL'))}</span>`;
    }

    // Duration · Size cell (mockup):
    //  Running -> "M:SS / M:SS" (live elapsed / planned), size = "~210 MB".
    //  Inne -> "M:SS", size = sformatowany rozmiar.
    let durationCell;
    if (isRunning) {
      const elapsed = elapsedSeconds(s.started_at_unix_ns);
      const planned = Number.isFinite(s.duration_seconds) && s.duration_seconds > 0
        ? formatDuration(s.duration_seconds)
        : '∞';
      const sizeLabel = Number.isFinite(s.size_bytes) && s.size_bytes > 0
        ? `~${size}`
        : '—';
      durationCell = `<span class="ds-duration" data-running-elapsed>${formatDuration(elapsed)} / ${planned}</span><div class="lc-sub">${sizeLabel}</div>`;
    } else {
      durationCell = `<span class="ds-duration">${formatDuration(s.duration_seconds)}</span><div class="lc-sub">${size}</div>`;
    }

    // Mockup #03: failed wiersz pokazuje czerwona wiadomosc bledu zamiast
    // listy chipow zrodlowych. Fallback: pierwsza pozycja z sources_used z
    // statusem "failed", a w ostatecznosci ogolny komunikat.
    let labelDetail;
    if (isFailed) {
      let errMsg = s.error_message || s.failure_reason || '';
      if (!errMsg && Array.isArray(s.sources_used)) {
        const failedSrc = s.sources_used.find((x) => x.status === 'failed' || x.status === 'bad');
        if (failedSrc) errMsg = `${failedSrc.id || failedSrc.label}: ${failedSrc.message || 'failed'}`;
      }
      if (!errMsg) errMsg = t('profiling.sessions.session_failed', null, 'session failed');
      labelDetail = `<div class="lc-sub lc-err">${escapeHtml(errMsg)}</div>`;
    } else {
      const chips = srcChips(s.sources_used);
      labelDetail = chips ? `<div class="src-chips">${chips}</div>` : '';
    }

    // Akcje per status (mockup #03):
    //  Running   -> Watch live + Stop
    //  Completed -> Open + Compare + Download + Delete
    //  Partial   -> Open + Re-run with sudo
    //  Failed    -> View logs + Delete
    const aWatch = escapeHtml(t('profiling.sessions.action_watch', null, 'Watch live'));
    const aStop = escapeHtml(t('profiling.sessions.action_stop', null, 'Stop'));
    const aLogs = escapeHtml(t('profiling.sessions.action_logs', null, 'View logs'));
    const aDelete = escapeHtml(t('profiling.sessions.action_delete', null, 'Delete'));
    const aOpen = escapeHtml(t('profiling.sessions.action_open', null, 'Open report'));
    const aCompare = escapeHtml(t('profiling.sessions.action_compare', null, 'Compare'));
    const aCompareSel = escapeHtml(t('profiling.sessions.action_compare_selected', null, 'Selected for compare'));
    const aDownload = escapeHtml(t('profiling.sessions.action_download', null, 'Download'));
    const aRerunSudo = escapeHtml(t('profiling.sessions.action_rerun_sudo', null, 'Re-run with sudo'));
    let actions = '';
    if (isRunning) {
      actions = `
        <tf-button variant="ghost" size="sm" icon="eye" data-action="watch" data-sid="${sid}" title="${aWatch}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="stop" data-action="stop" data-sid="${sid}" title="${aStop}"></tf-button>
      `;
    } else if (isFailed) {
      actions = `
        <tf-button variant="ghost" size="sm" icon="file-text" data-action="logs" data-sid="${sid}" title="${aLogs}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="delete" data-sid="${sid}" title="${aDelete}"></tf-button>
      `;
    } else if (isPartial) {
      actions = `
        <tf-button variant="ghost" size="sm" icon="external-link" data-action="open" data-sid="${sid}" title="${aOpen}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="refresh" data-action="rerun-sudo" data-sid="${sid}" title="${aRerunSudo}"></tf-button>
      `;
    } else {
      const cmpClass = isSelected ? 'primary' : 'ghost';
      actions = `
        <tf-button variant="ghost" size="sm" icon="external-link" data-action="open" data-sid="${sid}" title="${aOpen}"></tf-button>
        <tf-button variant="${cmpClass}" size="sm" icon="copy" data-action="compare-toggle" data-sid="${sid}" title="${isSelected ? aCompareSel : aCompare}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="download" data-action="download" data-sid="${sid}" title="${aDownload}"></tf-button>
        <tf-button variant="ghost" size="sm" icon="trash" data-action="delete" data-sid="${sid}" title="${aDelete}"></tf-button>
      `;
    }
    if (!canCompare && !isRunning && !isFailed) {
      actions = `
        <tf-button variant="ghost" size="sm" icon="external-link" data-action="open" data-sid="${sid}" title="${aOpen}"></tf-button>
      `;
    }

    return `
      <tr data-session-id="${sid}" class="${isSelected ? 'selected' : ''}">
        <td>${ico}</td>
        <td>
          <div class="label-cell"><div>
            <div class="lc-name">${labelName}${statusPill}</div>
            ${labelDetail}
          </div></div>
        </td>
        <td>${durationCell}</td>
        <td title="${escapeHtml(abs)}"><span class="started-top">${escapeHtml(startedTop)}</span><div class="lc-sub">${escapeHtml(startedSub)}</div></td>
        <td class="actions-col"><span class="row-actions">${actions}</span></td>
      </tr>
    `;
  }

  _attachRowListeners(wrap) {
    wrap.querySelectorAll('tr[data-session-id]').forEach((tr) => {
      tr.addEventListener('click', (ev) => {
        // Ignore clicks on action buttons / checkboxes.
        if (ev.target.closest('[data-action]')) return;
        if (ev.target.tagName === 'INPUT') return;
        const sid = tr.getAttribute('data-session-id');
        this._openReport(sid);
      });
    });
    wrap.querySelectorAll('[data-action]').forEach((btn) => {
      const handler = (ev) => {
        ev.stopPropagation();
        const action = btn.getAttribute('data-action');
        const sid = btn.getAttribute('data-sid');
        if (!sid) return;
        if (action === 'compare-toggle') {
          this._toggleCompare(sid);
          return;
        }
        if (action === 'open') this._openReport(sid);
        else if (action === 'download') this._downloadSession(sid);
        else if (action === 'delete') this._confirmDelete(sid);
        else if (action === 'watch') this._openReport(sid);
        else if (action === 'stop') this._stopRunning(sid);
        else if (action === 'logs') this._openReport(sid);
        else if (action === 'rerun-sudo') this._openLaunch();
      };
      // Checkboxy reaguja na 'change' (tez na klawiature); buttons na 'click'.
      if (btn.tagName === 'INPUT') {
        btn.addEventListener('change', handler);
        btn.addEventListener('click', (ev) => ev.stopPropagation());
      } else {
        btn.addEventListener('click', handler);
      }
    });
  }

  _openReport(sessionId) {
    if (this.onOpenReport) {
      try { this.onOpenReport(sessionId); }
      catch (err) { console.error('onOpenReport callback error', err); }
      return;
    }
    // Default: SPA Router (no hash routes in TentaFlow). Wymaga ze nodeId
    // jest dostepne (konstruktor ustawia this.nodeId).
    if (window.Router && typeof window.Router.navigate === 'function') {
      window.Router.navigate('profile-report', { nodeId: this.nodeId, sessionId });
    }
  }

  async _downloadSession(sessionId) {
    if (fixtureMode()) {
      showToast(t('profiling.sessions.toast_fixture_download', null, 'Fixture mode — download not supported'), 'info');
      return;
    }
    try {
      const resp = await profilingDownload({ nodeId: this.nodeId, sessionId });
      const bytes = resp.tarballBytes instanceof Uint8Array
        ? resp.tarballBytes
        : new Uint8Array(resp.tarballBytes || []);
      const filename = resp.filename || `profiling-${sessionId}.tar.gz`;
      const blob = new Blob([bytes], { type: 'application/gzip' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = filename;
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (err) {
      console.error('failed to download session', err);
      showToast(t('profiling.sessions.toast_download_failed', null, 'Failed to download session'), 'error');
    }
  }

  async _stopRunning(sessionId) {
    if (fixtureMode()) {
      showToast(t('profiling.sessions.toast_fixture_stop', null, 'Fixture mode — stop not supported'), 'info');
      return;
    }
    try {
      await profilingStop({ nodeId: this.nodeId, sessionId });
      showToast(t('profiling.sessions.toast_session_stopped', null, 'Session stopped'), 'success');
      await this.refresh();
    } catch (err) {
      console.error('failed to stop session', err);
      showToast(t('profiling.sessions.toast_stop_failed', null, 'Failed to stop session'), 'error');
    }
  }

  async _confirmDelete(sessionId) {
    const confirmed = await TfWindow.confirm({
      title: t('profiling.sessions.delete_confirm_title', null, 'Delete profiling session?'),
      message: t('profiling.sessions.delete_confirm_msg', { sid: sessionId }, `Session ${sessionId} will be removed permanently.`),
      description: t('profiling.sessions.delete_confirm_desc', null, 'This cannot be undone.'),
      confirmLabel: t('profiling.sessions.delete_confirm_ok', null, 'Delete'),
      cancelLabel: t('profiling.sessions.delete_confirm_cancel', null, 'Cancel'),
      danger: true,
    });
    if (!confirmed) return;
    try {
      await deleteSession(this.nodeId, sessionId);
      showToast(t('profiling.sessions.toast_session_deleted', null, 'Session deleted'), 'success');
      await this.refresh();
    } catch (err) {
      console.error('failed to delete session', err);
      showToast(t('profiling.sessions.toast_delete_failed', null, 'Failed to delete session'), 'error');
    }
  }

  // Export all eligible sessions — iteruje completed/partial i wyzwala
  // pojedyncze tarballe. Backend endpoint dla bulk-zip nie istnieje, ale
  // pojedyncze download'y leca przez juz dziala'jacy profilingDownload.
  async _exportAll() {
    if (fixtureMode()) {
      showToast(t('profiling.sessions.toast_fixture_export', null, 'Fixture mode — export not supported'), 'info');
      return;
    }
    const eligible = this.sessions.filter((s) => s.status === 'completed' || s.status === 'partial');
    if (eligible.length === 0) {
      showToast(t('profiling.sessions.toast_no_completed', null, 'No completed sessions to export'), 'info');
      return;
    }
    showToast(t('profiling.sessions.toast_exporting', { n: eligible.length }, `Exporting ${eligible.length} session(s)…`), 'info');
    let ok = 0;
    for (const s of eligible) {
      try {
        await this._downloadSession(s.session_id);
        ok += 1;
      } catch (err) {
        console.error('export: failed for session', s.session_id, err);
      }
    }
    showToast(t('profiling.sessions.toast_exported', { ok, total: eligible.length }, `Exported ${ok}/${eligible.length} sessions`), ok === eligible.length ? 'success' : 'error');
  }

  async _openLaunch() {
    if (this.availableSources.length === 0) {
      showToast(t('profiling.sessions.toast_no_sources', null, 'No profiling sources available on this node'), 'error');
      return;
    }
    try {
      const result = await ProfilingLaunchModal.open({
        nodeId: this.nodeId,
        availableSources: this.availableSources,
        onLaunched: () => { /* refresh handled below */ },
      });
      if (result.launched) {
        showToast(t('profiling.sessions.toast_session_started', null, 'Profiling session started'), 'success');
        await this.refresh();
      }
    } catch (err) {
      console.error('launch modal failed', err);
      showToast(err.message || t('profiling.sessions.toast_launch_failed', null, 'Failed to launch profiling'), 'error');
    }
  }
}
