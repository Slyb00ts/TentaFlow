// =============================================================================
// Plik: modules/profiling-active-banner.js
// Opis: Banner aktywnej sesji multi-source profilingu. Pokazuje sie na node
//       detail page gdy backend raportuje aktywna sesje. REC dot pulse,
//       countdown, chipy biegnacych kolektorow, akcje stop / open report.
// =============================================================================

import '/js/components/tf-button.js';
import {
  profilingActiveInfo,
  profilingStop,
} from '/js/protocol/profiling.js';

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

function formatMS(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

// Fixture state — sztuczna sesja zaczynajaca sie przy pierwszym pollu i
// trwajaca 60s. Persisted in module scope (resetowane gdy fixture mode off).
let fixtureSessionStartedAt = 0;
let fixtureSessionStopped = false;

function fixtureActiveResponse() {
  const planned = 60_000_000_000; // 60s in ns
  if (!fixtureSessionStartedAt) fixtureSessionStartedAt = Date.now();
  const elapsedMs = Date.now() - fixtureSessionStartedAt;
  if (fixtureSessionStopped || elapsedMs >= 60_000) {
    fixtureSessionStopped = true;
    return null;
  }
  return {
    session_id: 'fixture-active-001',
    label: 'Live fixture session',
    started_at_unix_ns: fixtureSessionStartedAt * 1_000_000,
    planned_duration_ns: planned,
    elapsed_ns: elapsedMs * 1_000_000,
    collectors_running: [
      { id: 'linux.perf.cpu_sampling', label: 'perf' },
      { id: 'nvidia.nsys.gpu', label: 'nsys' },
      { id: 'linux.proc.ram', label: 'ram' },
      { id: 'linux.rapl.power', label: 'rapl' },
    ],
  };
}

// Normalizes the camelCase ProfilingActiveSessionInfo from wasm-glue into the
// snake_case shape that the banner template consumes. `collectorsRunning` is a
// flat string[] now; we wrap it back into {id,label} so the chip renderer
// keeps a single code path.
function normalizeActive(info) {
  if (info == null) return null;
  if ('session_id' in info) return info;
  return {
    session_id: info.sessionId,
    label: info.label,
    started_at_unix_ns: Number(info.startedAtUnixNs || 0),
    planned_duration_ns: Number(info.plannedDurationNs || 0),
    elapsed_ns: Number(info.elapsedNs || 0),
    collectors_running: Array.isArray(info.collectorsRunning)
      ? info.collectorsRunning.map((id) => ({ id, label: id }))
      : [],
  };
}

async function fetchActive(nodeId) {
  if (fixtureMode()) {
    return fixtureActiveResponse();
  }
  const resp = await profilingActiveInfo({ nodeId });
  return normalizeActive(resp ? resp.info : null);
}

async function stopActive(nodeId, sessionId) {
  if (fixtureMode()) {
    fixtureSessionStopped = true;
    return { session_id: 'fixture-active-001', report_url: null };
  }
  const resp = await profilingStop({ nodeId, sessionId });
  return { session_id: resp.sessionId, report_url: null };
}

// =============================================================================
// ProfilingActiveBanner — komponent.
// =============================================================================

export class ProfilingActiveBanner {
  /**
   * @param {object} opts
   * @param {string} opts.nodeId
   * @param {Function=} opts.onSessionEnded (sessionId) => void
   */
  constructor(opts = {}) {
    this.nodeId = opts.nodeId;
    this.onSessionEnded = typeof opts.onSessionEnded === 'function' ? opts.onSessionEnded : null;
    this.root = null;
    this.pollTimer = null;
    this.tickTimer = null;
    this.session = null;
    this._mountedAt = 0;
  }

  mount(parent) {
    if (!parent) throw new Error('ProfilingActiveBanner.mount requires parent');
    this.root = document.createElement('section');
    this.root.className = 'session-banner';
    this.root.style.display = 'none';
    parent.appendChild(this.root);
    this._mountedAt = Date.now();
    this._startPolling();
  }

  unmount() {
    this._stopPolling();
    if (this.root && this.root.parentNode) this.root.parentNode.removeChild(this.root);
    this.root = null;
    this.session = null;
  }

  _startPolling() {
    this._poll();
    this.pollTimer = setInterval(() => this._poll(), 1000);
    this.tickTimer = setInterval(() => this._tick(), 1000);
  }

  _stopPolling() {
    if (this.pollTimer) { clearInterval(this.pollTimer); this.pollTimer = null; }
    if (this.tickTimer) { clearInterval(this.tickTimer); this.tickTimer = null; }
  }

  async _poll() {
    if (!this.root) return;
    let sess = null;
    try {
      sess = await fetchActive(this.nodeId);
    } catch (err) {
      console.error('failed to fetch active profiling session', err);
      sess = null;
    }
    const previous = this.session;
    this.session = sess;
    if (!sess) {
      this.root.style.display = 'none';
      if (previous && this.onSessionEnded) {
        try { this.onSessionEnded(previous.session_id); }
        catch (e) { console.error('onSessionEnded callback error', e); }
      }
      return;
    }
    this.root.style.display = '';
    this._render();
  }

  _tick() {
    if (!this.session || !this.root) return;
    // Update countdown without full re-render to avoid input/handler churn.
    const cd = this.root.querySelector('.countdown');
    if (cd) cd.innerHTML = this._countdownHtml();
  }

  _countdownHtml() {
    const sess = this.session;
    if (!sess) return '';
    const startedMs = sess.started_at_unix_ns / 1_000_000;
    const plannedSec = (sess.planned_duration_ns || 0) / 1_000_000_000;
    const elapsedSec = Math.max(0, (Date.now() - startedMs) / 1000);
    if (plannedSec > 0) {
      const remaining = Math.max(0, plannedSec - elapsedSec);
      return `${formatMS(remaining)} <span class="of">/ ${formatMS(plannedSec)}</span>`;
    }
    return `${formatMS(elapsedSec)} <span class="of">elapsed (manual stop)</span>`;
  }

  _render() {
    const sess = this.session;
    if (!sess || !this.root) return;
    const collectors = Array.isArray(sess.collectors_running) ? sess.collectors_running : [];
    const chips = collectors.map((c) => `
      <span class="col-chip"><span class="dot"></span>${escapeHtml(c.label || c.id)}</span>
    `).join('');
    // Meta wg mockupu: "session a3f9c2e1b8d4 · started 02:01:18 · 9 collectors"
    const sidShort = String(sess.session_id || '').slice(0, 12);
    const startedMs = sess.started_at_unix_ns / 1_000_000;
    const startedHHMMSS = startedMs > 0
      ? new Date(startedMs).toLocaleTimeString('en-GB', { hour12: false })
      : '—';
    const colCount = collectors.length;
    const meta = `session ${sidShort} · started ${startedHHMMSS} · ${colCount} collector${colCount === 1 ? '' : 's'}`;

    this.root.innerHTML = `
      <span class="rec">REC</span>
      <div class="session-title">
        <div class="s-label">${escapeHtml(sess.label || 'profiling session')}</div>
        <div class="s-meta">${escapeHtml(meta)}</div>
      </div>
      <div class="countdown">${this._countdownHtml()}</div>
      <div class="banner-actions">
        <tf-button variant="outline" size="sm" icon="external-link" data-action="open-when-done" disabled>Open report when done</tf-button>
        <tf-button variant="danger" size="sm" icon="stop" data-action="stop">Stop now</tf-button>
      </div>
      <div class="collectors">${chips}</div>
    `;

    const stopBtn = this.root.querySelector('[data-action="stop"]');
    if (stopBtn) {
      stopBtn.addEventListener('click', () => this._handleStop());
    }
    const openBtn = this.root.querySelector('[data-action="open-when-done"]');
    if (openBtn) {
      openBtn.addEventListener('click', () => {
        if (window.Router && typeof window.Router.navigate === 'function') {
          window.Router.navigate('profile-report', { nodeId: this.nodeId, sessionId: sess.session_id });
        }
      });
    }
  }

  async _handleStop() {
    if (!this.session) return;
    const sid = this.session.session_id;
    try {
      const res = await stopActive(this.nodeId, sid);
      if (res && res.report_url) {
        location.assign(res.report_url);
        return;
      }
      // Bez report_url — nawiguj do widoku raportu przez SPA Router.
      if (window.Router && typeof window.Router.navigate === 'function') {
        window.Router.navigate('profile-report', { nodeId: this.nodeId, sessionId: sid });
      }
    } catch (err) {
      console.error('failed to stop profiling session', err);
    }
  }
}
