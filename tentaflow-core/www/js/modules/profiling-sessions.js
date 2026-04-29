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
} from '/js/protocol/profiling.js';
import '/js/components/tf-button.js';
import '/js/components/tf-searchbox.js';

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
  if (diff < 60_000) return 'just now';
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} min ago`;
  if (diff < 86400_000) return `${Math.floor(diff / 3600_000)} hours ago`;
  if (diff < 7 * 86400_000) return `${Math.floor(diff / 86400_000)} days ago`;
  return new Date(ms).toLocaleDateString();
}

function formatAbsolute(unixNs) {
  if (!Number.isFinite(unixNs) || unixNs <= 0) return '—';
  return new Date(Math.floor(unixNs / 1_000_000)).toLocaleString();
}

function statusIcon(status) {
  if (status === 'running') {
    return `<span class="row-status-ico run" title="Running">
      <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4" fill="currentColor" stroke="none"/></svg>
    </span>`;
  }
  if (status === 'failed') {
    return `<span class="row-status-ico fail" title="Failed">
      <svg viewBox="0 0 24 24"><path d="M18 6L6 18M6 6l12 12"/></svg>
    </span>`;
  }
  if (status === 'partial') {
    return `<span class="row-status-ico warn" title="Partial">
      <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 8v4M12 16h.01"/></svg>
    </span>`;
  }
  return `<span class="row-status-ico ok" title="Completed">
    <svg viewBox="0 0 24 24"><path d="M5 12l4 4L19 7"/></svg>
  </span>`;
}

function srcChips(sourcesUsed) {
  if (!Array.isArray(sourcesUsed) || sourcesUsed.length === 0) return '—';
  const max = 2;
  const visible = sourcesUsed.slice(0, max);
  const rest = sourcesUsed.length - visible.length;
  const chips = visible.map((s) => {
    const cls = s.status === 'bad' ? 'bad' : s.status === 'warn' ? 'warn' : '';
    return `<span class="src-chip-mini ${cls}">${escapeHtml(s.label || s.id)}</span>`;
  }).join('');
  const more = rest > 0 ? `<span class="src-chip-more">+${rest} more</span>` : '';
  return `<span class="src-chips">${chips}${more}</span>`;
}

const FILTERS = [
  { id: 'all', label: 'All' },
  { id: 'last_24h', label: 'Last 24h' },
  { id: 'this_week', label: 'This week' },
  { id: 'has_flamegraph', label: 'Has flamegraph' },
  { id: 'multi_gpu', label: 'Multi-GPU' },
  { id: 'failed', label: 'Failed' },
];

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
  }

  _renderShell() {
    this.root.innerHTML = `
      <div class="breadcrumb">
        <a href="#/mesh" data-nav="mesh">Mesh</a>
        <span class="sep">›</span>
        <a href="#/mesh" data-nav="mesh">Nodes</a>
        <span class="sep">›</span>
        <a href="#/mesh/${escapeHtml(this.nodeId)}" data-nav="node">${escapeHtml(this.nodeName)}</a>
        <span class="sep">›</span>
        <span>Profiling sessions</span>
      </div>
      <div class="page-title">Profiling sessions</div>

      <div class="toolbar">
        <tf-searchbox id="ps-search" placeholder="Filter by label..."></tf-searchbox>
        <div id="ps-filter-chips"></div>
        <tf-button variant="ghost" size="sm" icon="settings" id="ps-permissions">Permissions</tf-button>
        <tf-button variant="ghost" size="sm" icon="refresh" id="ps-refresh">Refresh</tf-button>
        <tf-button variant="primary" size="sm" icon="plus" id="ps-new">New session</tf-button>
      </div>

      <div id="ps-compare-bar" class="ps-compare-bar hidden">
        <span class="count">0/2 selected</span>
        <span style="color: var(--tf-text-2, #a0a8c8);">Pick two completed sessions to compare side-by-side.</span>
        <span style="flex:1"></span>
        <tf-button variant="ghost" size="sm" id="ps-compare-clear">Clear</tf-button>
        <tf-button variant="primary" size="sm" id="ps-compare-go" disabled>Compare selected</tf-button>
      </div>

      <div id="ps-table-wrap"></div>
    `;

    // Render filter chips
    const chipsWrap = this.root.querySelector('#ps-filter-chips');
    chipsWrap.style.display = 'inline-flex';
    chipsWrap.style.gap = '6px';
    chipsWrap.style.flexWrap = 'wrap';
    for (const f of FILTERS) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'filter-chip';
      btn.textContent = f.label;
      btn.setAttribute('data-filter-id', f.id);
      btn.setAttribute('data-active', String(this.activeFilter === f.id));
      btn.addEventListener('click', () => {
        this.activeFilter = f.id;
        chipsWrap.querySelectorAll('.filter-chip').forEach((c) => {
          c.setAttribute('data-active', String(c.getAttribute('data-filter-id') === this.activeFilter));
        });
        this._renderTable();
      });
      chipsWrap.appendChild(btn);
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

    const permsBtn = this.root.querySelector('#ps-permissions');
    if (permsBtn) {
      permsBtn.addEventListener('click', () => {
        if (window.Router && typeof window.Router.navigate === 'function') {
          window.Router.navigate('profile-permissions');
        }
      });
    }

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
    if (countEl) countEl.textContent = `${n}/2 selected`;
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
          <div class="es-title">No profiling sessions yet</div>
          <div class="es-sub">Launch your first multi-source profiling session to inspect CPU, GPU, memory and power together.</div>
          <tf-button variant="primary" icon="record" id="ps-empty-launch">Launch first profiling</tf-button>
        </div>
      `;
      const btn = wrap.querySelector('#ps-empty-launch');
      if (btn) btn.addEventListener('click', () => this._openLaunch());
      return;
    }

    if (filtered.length === 0) {
      wrap.innerHTML = `
        <div class="empty-state">
          <div class="es-title">No sessions match current filters</div>
          <div class="es-sub">Try clearing the search or filter chips.</div>
        </div>
      `;
      return;
    }

    const rows = filtered.map((s) => this._renderRow(s)).join('');
    wrap.innerHTML = `
      <table class="tf-table-native">
        <thead>
          <tr>
            <th class="ps-select-cell" title="Select for compare"></th>
            <th style="width: 30%">Status / Label</th>
            <th style="width: 26%">Sources</th>
            <th style="width: 14%">Duration · Size</th>
            <th style="width: 14%">Started</th>
            <th class="actions-col" style="width: 12%">Actions</th>
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
    const labelName = escapeHtml(s.label || '(no label)');
    const labelSub = `${escapeHtml(s.session_id.slice(0, 12))} · ${escapeHtml(s.status)}`;
    const sources = srcChips(s.sources_used);
    const duration = formatDuration(s.duration_seconds);
    const size = formatBytes(s.size_bytes);
    const rel = formatRelative(s.started_at_unix_ns);
    const abs = formatAbsolute(s.started_at_unix_ns);

    const isSelected = this.compareSelected.has(s.session_id);
    // Tylko ukonczone sesje moga byc porownane (running/failed maja niepelne raporty).
    const canCompare = s.status === 'completed' || s.status === 'partial';
    const checkbox = canCompare
      ? `<input type="checkbox" data-action="compare-toggle" data-sid="${sid}" ${isSelected ? 'checked' : ''} aria-label="Select for compare" />`
      : `<input type="checkbox" disabled aria-label="Compare unavailable for this status" />`;

    return `
      <tr data-session-id="${sid}" class="${isSelected ? 'selected' : ''}">
        <td class="ps-select-cell">${checkbox}</td>
        <td>
          <div class="label-cell">
            ${ico}
            <div>
              <div class="lc-name">${labelName}</div>
              <div class="lc-sub">${labelSub}</div>
            </div>
          </div>
        </td>
        <td>${sources}</td>
        <td><span style="font-family:'JetBrains Mono',monospace;">${duration} · ${size}</span></td>
        <td title="${escapeHtml(abs)}"><span style="font-family:'JetBrains Mono',monospace; color:var(--tf-text-2,#a0a8c8);">${rel}</span></td>
        <td class="actions-col">
          <span class="row-actions">
            <tf-button variant="ghost" size="sm" icon="external-link" data-action="open" data-sid="${sid}" title="Open report"></tf-button>
            <tf-button variant="ghost" size="sm" icon="download" data-action="download" data-sid="${sid}" title="Download"></tf-button>
            <tf-button variant="ghost" size="sm" icon="trash" data-action="delete" data-sid="${sid}" title="Delete"></tf-button>
          </span>
        </td>
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
      showToast('Fixture mode — download not supported', 'info');
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
      showToast('Failed to download session', 'error');
    }
  }

  async _confirmDelete(sessionId) {
    const confirmed = await TfWindow.confirm({
      title: 'Delete profiling session?',
      message: `Session ${sessionId} will be removed permanently.`,
      description: 'This cannot be undone.',
      confirmLabel: 'Delete',
      cancelLabel: 'Cancel',
      danger: true,
    });
    if (!confirmed) return;
    try {
      await deleteSession(this.nodeId, sessionId);
      showToast('Session deleted', 'success');
      await this.refresh();
    } catch (err) {
      console.error('failed to delete session', err);
      showToast('Failed to delete session', 'error');
    }
  }

  async _openLaunch() {
    if (this.availableSources.length === 0) {
      showToast('No profiling sources available on this node', 'error');
      return;
    }
    try {
      const result = await ProfilingLaunchModal.open({
        nodeId: this.nodeId,
        availableSources: this.availableSources,
        onLaunched: () => { /* refresh handled below */ },
      });
      if (result.launched) {
        showToast('Profiling session started', 'success');
        await this.refresh();
      }
    } catch (err) {
      console.error('launch modal failed', err);
      showToast(err.message || 'Failed to launch profiling', 'error');
    }
  }
}
