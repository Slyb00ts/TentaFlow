// =============================================================================
// Plik: modules/profile-rec-pill.js
// Opis: Kompaktowa pigulka REC dla topbara mesh-detail. Polluje
//       `profilingActiveInfo` co 1s; gdy aktywna sesja istnieje, renderuje
//       pulsujacy REC + label + countdown + przycisk Stop. Sticky widocznosc
//       (siedzi w topbarze d-actions, niezaleznie od scroll).
// =============================================================================

import '/js/components/tf-button.js';
import {
  profilingActiveInfo,
  profilingStop,
} from '/js/protocol/profiling.js';
import { I18n } from '/js/i18n.js';

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

function normalizeActive(info) {
  if (info == null) return null;
  if ('session_id' in info) return info;
  return {
    session_id: info.sessionId,
    label: info.label,
    started_at_unix_ns: Number(info.startedAtUnixNs || 0),
    planned_duration_ns: Number(info.plannedDurationNs || 0),
    elapsed_ns: Number(info.elapsedNs || 0),
  };
}

async function fetchActive(nodeId) {
  if (fixtureMode()) {
    // Reuzywamy fixture banner state przez globalna flage; pill po prostu
    // wola `profilingActiveInfo` w zwykym trybie.
    return null;
  }
  const resp = await profilingActiveInfo({ nodeId });
  return normalizeActive(resp ? resp.info : null);
}

// =============================================================================
// ProfileRecPill — komponent. Renderuje sie w istniejacym slot DOM
// (topbar d-actions). Auto-show/hide w zaleznosci od stanu sesji.
// =============================================================================

export class ProfileRecPill {
  /**
   * @param {object} opts
   * @param {string} opts.nodeId
   */
  constructor(opts = {}) {
    this.nodeId = opts.nodeId;
    this.root = null;
    this.pollTimer = null;
    this.tickTimer = null;
    this.session = null;
  }

  mount(parent) {
    if (!parent) throw new Error('ProfileRecPill.mount requires parent');
    this.root = document.createElement('div');
    this.root.className = 'profile-rec-pill';
    this.root.style.display = 'none';
    parent.appendChild(this.root);
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
    } catch (_e) {
      sess = null;
    }
    if (!this.root) return;
    this.session = sess;
    if (!sess) {
      this.root.style.display = 'none';
      this.root.innerHTML = '';
      return;
    }
    this.root.style.display = '';
    this._render();
  }

  _tick() {
    if (!this.session || !this.root) return;
    const cd = this.root.querySelector('.rp-countdown');
    if (cd) cd.textContent = this._countdownText();
  }

  _countdownText() {
    const sess = this.session;
    if (!sess) return '';
    const startedMs = sess.started_at_unix_ns / 1_000_000;
    const plannedSec = (sess.planned_duration_ns || 0) / 1_000_000_000;
    const elapsedSec = Math.max(0, (Date.now() - startedMs) / 1000);
    if (plannedSec > 0) {
      const remaining = Math.max(0, plannedSec - elapsedSec);
      return `${formatMS(remaining)} / ${formatMS(plannedSec)}`;
    }
    return formatMS(elapsedSec);
  }

  _render() {
    const sess = this.session;
    if (!sess || !this.root) return;
    const recLabel = I18n.t('profiling.banner.rec') || 'REC';
    const stopLabel = I18n.t('mesh.profile_rec_stop') || I18n.t('profiling.banner.stop_now') || 'Stop';
    const sessLabel = sess.label || (I18n.t('profiling.banner.default_label') || 'profiling session');

    this.root.innerHTML = `
      <span class="rp-rec" title="${escapeHtml(sessLabel)}">
        <span class="rp-dot"></span>
        ${escapeHtml(recLabel)}
      </span>
      <span class="rp-label" title="${escapeHtml(sessLabel)}">${escapeHtml(sessLabel)}</span>
      <span class="rp-countdown">${this._countdownText()}</span>
      <tf-button size="sm" variant="danger-outline" data-action="rp-stop">${escapeHtml(stopLabel)}</tf-button>
    `;
    const stopBtn = this.root.querySelector('[data-action="rp-stop"]');
    if (stopBtn) stopBtn.addEventListener('click', () => this._handleStop());
  }

  async _handleStop() {
    if (!this.session) return;
    const sid = this.session.session_id;
    try {
      await profilingStop({ nodeId: this.nodeId, sessionId: sid });
      this.session = null;
      if (this.root) {
        this.root.style.display = 'none';
        this.root.innerHTML = '';
      }
    } catch (err) {
      console.error('failed to stop profiling session (pill)', err);
    }
  }
}
