// =============================================================================
// File: modules/profile-permissions.js
// Purpose: Profile Permissions Settings (mockup section 15). Globalny ekran
//          ustawien profiling permissions: cache hasla sudo (in-memory only),
//          auto-elevate toggle, override sciezek kolektorow, limity storage
//          (lokalne), oraz lista source-toggle dla launch modal.
// =============================================================================

import {
  getSudoPassword,
  setSudoPassword,
  clearSudoPassword,
  isSudoValidated,
  validateSudo,
  getDisabledSources,
  toggleSourceDisabled,
  getCollectorPaths,
  setCollectorPath,
  resetCollectorPaths,
} from '/js/lib/profile-permissions-store.js';
import { profilingCollectorsStatus } from '/js/protocol/profiling.js';
import { I18n } from '/js/i18n.js';
import '/js/components/tf-button.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';

function ti(key, vars, fallback) {
  const v = I18n.t(key, vars || null);
  return v === key && fallback != null ? fallback : v;
}

// Klucze localStorage dla per-browser preferencji nieobslugiwanych przez backend.
const KEY_REMEMBER_SUDO = 'tf-profile-permissions-remember-sudo';
const KEY_AUTO_ELEVATE  = 'tf-profile-permissions-auto-elevate';
const KEY_STORAGE_LIMITS = 'tf-profile-permissions-storage-limits';

function readBoolLocal(key, fallback) {
  try { const v = localStorage.getItem(key); return v == null ? fallback : v === '1'; }
  catch (_e) { return fallback; }
}
function writeBoolLocal(key, value) {
  try { localStorage.setItem(key, value ? '1' : '0'); } catch (_e) {}
}
function readJsonLocal(key, fallback) {
  try { const raw = localStorage.getItem(key); return raw ? JSON.parse(raw) : fallback; }
  catch (_e) { return fallback; }
}
function writeJsonLocal(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch (_e) {}
}

const DEFAULT_STORAGE_LIMITS = {
  capPerSession: '1 GB',
  fifoSize: 20,
  autoDeleteDays: '7 days',
};

// Domyślne kolektory pokazywane w UI (auto-discovery będzie wskazywać
// "FOUND/N/A" tylko jak backend dostarczy odpowiedź; bez backendu pokazujemy
// sam override).
const DEFAULT_COLLECTORS = [
  { id: 'nsys',          label: 'nsys',          vendor: 'nv',    defaultPath: '/usr/local/cuda/bin/nsys' },
  { id: 'rocprof',       label: 'rocprof',       vendor: 'amd',   defaultPath: '/opt/rocm/bin/rocprof' },
  { id: 'perf',          label: 'perf',          vendor: 'cpu',   defaultPath: '/usr/bin/perf' },
  { id: 'intel_gpu_top', label: 'intel_gpu_top', vendor: 'intel', defaultPath: '/usr/bin/intel_gpu_top' },
  { id: 'powermetrics',  label: 'powermetrics',  vendor: 'apple', defaultPath: '/usr/bin/powermetrics' },
];

// Lista source-id, ktore moga byc globalnie wylaczone. Dopasowana do tego co
// pokazuje launch modal (CPU sampling, GPU collectors, RAPL itd.). Nie jest
// to zrodlo prawdy o realnym capability — tylko user toggle.
const TOGGLABLE_SOURCES = [
  { id: 'linux.perf.cpu_sampling', label: 'CPU sampling (perf)' },
  { id: 'linux.perf.cpu_counters', label: 'CPU PMU counters' },
  { id: 'linux.cpu_util',          label: 'CPU utilization' },
  { id: 'linux.ram',               label: 'RAM samples' },
  { id: 'linux.disk.iostat',       label: 'Disk IO (iostat)' },
  { id: 'nvidia.nsys',             label: 'NVIDIA nsys (CUDA tracing)' },
  { id: 'amd.rocprof',             label: 'AMD rocprof' },
  { id: 'intel.gpu_top',           label: 'Intel intel_gpu_top' },
  { id: 'rapl.power',              label: 'RAPL power (sudo)' },
  { id: 'linux.network',           label: 'Network samples' },
];

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function navigateBack() {
  if (window.Router && typeof window.Router.navigate === 'function') {
    window.Router.navigate('mesh');
    return;
  }
  history.back();
}

// =============================================================================
// Public API.
// =============================================================================

export class ProfilePermissionsView {
  static async render(container) {
    if (!container) throw new Error('container is required');
    container.innerHTML = renderShell();
    bind(container);
    // Background — discovery binarek przez binary protocol. Jesli odpowie,
    // FOUND/N/A pills i wersje ustawiane sa async (no-op gdy backend brak).
    refreshCollectorStatus(container).catch(() => {});
  }
}

export default ProfilePermissionsView;

function renderShell() {
  const sudoCached = !!getSudoPassword();
  const validated = isSudoValidated();
  const disabled = new Set(getDisabledSources());
  const overrides = getCollectorPaths();
  const rememberSudo = readBoolLocal(KEY_REMEMBER_SUDO, true);
  const autoElevate  = readBoolLocal(KEY_AUTO_ELEVATE, false);
  const limits = { ...DEFAULT_STORAGE_LIMITS, ...(readJsonLocal(KEY_STORAGE_LIMITS, {}) || {}) };

  const collectorsHtml = DEFAULT_COLLECTORS.map((c) => {
    const cur = overrides[c.id] || c.defaultPath;
    return `
      <div class="pp-path-row">
        <div class="pp-path-name"><span class="pp-vendor pp-v-${escapeHtml(c.vendor)}">${escapeHtml(c.vendor.toUpperCase())}</span>${escapeHtml(c.label)}</div>
        <input class="pp-path-input" type="text" data-collector-id="${escapeHtml(c.id)}" value="${escapeHtml(cur)}" placeholder="${escapeHtml(c.defaultPath)}" />
        <span class="pp-path-status" data-collector-status="${escapeHtml(c.id)}">${escapeHtml(ti('profiling.permissions.status_pending', null, 'PENDING'))}</span>
      </div>
    `;
  }).join('');

  const sourcesHtml = TOGGLABLE_SOURCES.map((s) => {
    const isDisabled = disabled.has(s.id);
    return `
      <div class="pp-row">
        <div class="pp-row-meta">
          <div class="pp-row-name">${escapeHtml(s.label)}</div>
          <div class="pp-row-desc mono">${escapeHtml(s.id)}</div>
        </div>
        <tf-toggle data-source-toggle="${escapeHtml(s.id)}" ${isDisabled ? '' : 'checked'}></tf-toggle>
      </div>
    `;
  }).join('');

  const sudoStatusBadge = sudoCached
    ? (validated
        ? `<span class="pp-badge ok">${escapeHtml(ti('profiling.permissions.badge_cached_validated', null, 'Cached · validated'))}</span>`
        : `<span class="pp-badge warn">${escapeHtml(ti('profiling.permissions.badge_cached_unvalidated', null, 'Cached · unvalidated'))}</span>`)
    : `<span class="pp-badge muted">${escapeHtml(ti('profiling.permissions.badge_not_set', null, 'Not set'))}</span>`;

  return `
    <div class="profile-permissions">
      <nav class="pr-breadcrumb" aria-label="Breadcrumb">
        <a href="#" data-action="back-mesh">${escapeHtml(ti('profiling.permissions.breadcrumb_mesh', null, 'Mesh'))}</a>
        <span class="sep">/</span>
        <span>${escapeHtml(ti('profiling.permissions.breadcrumb_profiling', null, 'Profiling'))}</span>
        <span class="sep">/</span>
        <span>${escapeHtml(ti('profiling.permissions.breadcrumb_permissions', null, 'Permissions'))}</span>
      </nav>

      <header class="pp-header">
        <h1 class="pp-title">${escapeHtml(ti('profiling.permissions.title', null, 'Profile permissions'))}</h1>
        <div class="pp-sub">${escapeHtml(ti('profiling.permissions.subtitle', null, 'Per-tab settings — sudo password lives in memory only and is cleared when the tab is closed.'))}</div>
        <div class="pp-actions"><tf-button variant="ghost" size="sm" data-action="back-mesh">${escapeHtml(ti('profiling.permissions.back', null, 'Back'))}</tf-button></div>
      </header>

      <section class="pp-card">
        <h3 class="pp-h3">
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>
          ${escapeHtml(ti('profiling.permissions.section_caching_h', null, 'Privilege caching'))}
        </h3>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">${escapeHtml(ti('profiling.permissions.remember_sudo_name', null, 'Remember sudo for current session'))}</div>
            <div class="pp-row-desc">${escapeHtml(ti('profiling.permissions.remember_sudo_desc', null, 'Cache password in memory (never on disk) until tentaflow process exits. Convenient for back-to-back captures.'))}</div>
          </div>
          <tf-toggle data-pref-toggle="remember-sudo" ${rememberSudo ? 'checked' : ''}></tf-toggle>
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">${escapeHtml(ti('profiling.permissions.auto_elevate_name', null, 'Auto-elevate when profiling started'))}</div>
            <div class="pp-row-desc">${escapeHtml(ti('profiling.permissions.auto_elevate_desc', null, 'Automatically prompt for sudo when starting any profile session that requires it.'))}</div>
          </div>
          <tf-toggle data-pref-toggle="auto-elevate" ${autoElevate ? 'checked' : ''}></tf-toggle>
        </div>

        <div class="pp-alert danger">
          ${ti('profiling.permissions.security_warning', null, '<strong>Security warning:</strong> caching sudo credentials grants the tentaflow process the ability to spawn privileged collectors without re-authentication. Disable this toggle on shared workstations.')}
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">${escapeHtml(ti('profiling.permissions.sudo_password_name', null, 'Sudo password (in-memory)'))}</div>
            <div class="pp-row-desc">${escapeHtml(ti('profiling.permissions.sudo_password_desc', null, 'Used by collectors that require root (RAPL, kernel tracing).'))} ${sudoStatusBadge}</div>
          </div>
          <div class="pp-row-control">
            <input id="pp-sudo-input" type="password" autocomplete="off" placeholder="—" value="${escapeHtml(getSudoPassword())}" />
            <tf-button variant="ghost" size="sm" data-action="sudo-validate">${escapeHtml(ti('profiling.permissions.sudo_validate', null, 'Validate'))}</tf-button>
            <tf-button variant="ghost" size="sm" data-action="sudo-clear">${escapeHtml(ti('profiling.permissions.sudo_clear', null, 'Clear'))}</tf-button>
          </div>
        </div>

        <div id="pp-sudo-feedback" class="pp-feedback" hidden></div>
      </section>

      <section class="pp-card">
        <h3 class="pp-h3">
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="M7 10l5 5 5-5"/><path d="M12 15V3"/></svg>
          ${escapeHtml(ti('profiling.permissions.section_paths_h', null, 'Collector binary paths'))}
        </h3>
        <div class="pp-sub">${escapeHtml(ti('profiling.permissions.section_paths_sub', null, 'Auto-discovered from PATH. Override if you have a non-standard installation. Paths are stored locally per-browser.'))}</div>
        ${collectorsHtml}
        <div class="pp-actions-row">
          <tf-button variant="ghost" size="sm" data-action="paths-reset">${escapeHtml(ti('profiling.permissions.btn_paths_reset', null, 'Reset to defaults'))}</tf-button>
        </div>
      </section>

      <section class="pp-card">
        <h3 class="pp-h3">${escapeHtml(ti('profiling.permissions.section_sources_h', null, 'Source enablement'))}</h3>
        <div class="pp-sub">${escapeHtml(ti('profiling.permissions.section_sources_sub', null, 'Disabled sources are hidden from the launch modal on this browser. Backend capability detection still applies.'))}</div>
        ${sourcesHtml}
      </section>

      <section class="pp-card">
        <h3 class="pp-h3">
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/></svg>
          ${escapeHtml(ti('profiling.permissions.section_storage_h', null, 'Storage limits'))}
        </h3>
        <div class="pp-sub">${escapeHtml(ti('profiling.permissions.section_storage_sub', null, 'Backend does not yet expose limit-management API; settings live per-browser. Backend hardcoded: FIFO 20 sessions per node, max 600s duration, label ≤ 128 chars.'))}</div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">${escapeHtml(ti('profiling.permissions.cap_per_session_name', null, 'Storage cap per session'))}</div>
            <div class="pp-row-desc">${escapeHtml(ti('profiling.permissions.cap_per_session_desc', null, 'Maximum size of a single session. Sessions exceeding the cap are stopped and marked as truncated.'))}</div>
          </div>
          <input class="pp-field-input" id="pp-cap-session" type="text" value="${escapeHtml(String(limits.capPerSession))}" />
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">${escapeHtml(ti('profiling.permissions.fifo_size_name', null, 'FIFO size (sessions per node)'))}</div>
            <div class="pp-row-desc">${escapeHtml(ti('profiling.permissions.fifo_size_desc', null, 'Number of sessions kept on disk. The oldest is rotated when the limit is reached.'))}</div>
          </div>
          <input class="pp-field-input" id="pp-fifo-size" type="number" min="5" max="50" value="${escapeHtml(String(limits.fifoSize))}" />
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">${escapeHtml(ti('profiling.permissions.auto_delete_name', null, 'Auto-delete failed sessions after'))}</div>
            <div class="pp-row-desc">${escapeHtml(ti('profiling.permissions.auto_delete_desc', null, 'Failed sessions are deleted after this many days.'))}</div>
          </div>
          <input class="pp-field-input" id="pp-autodelete-days" type="text" value="${escapeHtml(String(limits.autoDeleteDays))}" />
        </div>

        <div class="pp-actions-row">
          <tf-button variant="ghost" size="sm" data-action="limits-reset">${escapeHtml(ti('profiling.permissions.btn_limits_reset', null, 'Reset defaults'))}</tf-button>
          <tf-button variant="primary" size="sm" data-action="limits-save">${escapeHtml(ti('profiling.permissions.btn_limits_save', null, 'Save settings'))}</tf-button>
        </div>
      </section>
    </div>
  `;
}

// Auto-discovery sciezek kolektorow przez binary protocol. Po renderze wola
// profilingCollectorsStatus() i aktualizuje pp-path-status badges (FOUND/N/A
// + wersja) oraz placeholder w inputach.
async function refreshCollectorStatus(container) {
  let resp;
  try {
    resp = await profilingCollectorsStatus({ nodeId: '' });
  } catch (err) {
    console.warn('[profile-permissions] collectors-status binary call failed:', err?.message || err);
    return;
  }
  const arr = Array.isArray(resp.collectors) ? resp.collectors : [];
  // Map collector backend id -> path/version. Klucze sa pelne id (np.
  // 'nvidia.nsys.gpu') a UI mappuje binarki przez prefiksy.
  const idMap = {
    nsys: arr.find((c) => c.id === 'nvidia.nsys.gpu'),
    rocprof: arr.find((c) => c.id === 'linux.rocprof.gpu_kernels'),
    perf: arr.find((c) => c.id === 'linux.perf.cpu_sampling'),
    intel_gpu_top: arr.find((c) => c.id === 'linux.intel_gpu_top.gpu'),
    powermetrics: arr.find((c) => c.id === 'macos.powermetrics.power'),
  };
  for (const [uiId, status] of Object.entries(idMap)) {
    if (!status) continue;
    const badge = container.querySelector(`[data-collector-status="${uiId}"]`);
    const input = container.querySelector(`[data-collector-id="${uiId}"]`);
    if (badge) {
      if (status.available && status.path) {
        const found = ti('profiling.permissions.status_found', null, 'FOUND');
        badge.textContent = `${found}${status.version ? ' · ' + status.version.split('\n')[0].slice(0, 24) : ''}`;
        badge.style.background = 'rgba(34,197,94,0.14)';
        badge.style.color = 'var(--tf-success, #22c55e)';
        badge.removeAttribute('title');
      } else {
        badge.textContent = ti('profiling.permissions.status_na', null, 'N/A');
        badge.style.background = 'rgba(245,158,11,0.14)';
        badge.style.color = 'var(--tf-warning, #f59e0b)';
        // Backend wbudowuje konkretna komende instalacji per-distro w note
        // (linia po '\n\nInstall: '). Pokaz w tooltipie + pod inputem.
        if (status.note) {
          badge.title = status.note;
          badge.style.cursor = 'help';
        }
      }
    }
    // Pod inputem - jezeli backend zwrocil sciezke binarki, pokaz jako placeholder.
    // Jezeli brak - pokaz install hint pod row.
    if (input && status.path && !input.value) {
      input.placeholder = status.path;
    }
    // Render install hint inline pod row gdy collector niedostepny.
    if (!status.available && status.note) {
      const row = badge?.closest('.pp-path-row');
      if (row) {
        // Sprawdz czy hint juz istnieje, jak nie - dodaj.
        let hintEl = row.querySelector('.pp-install-hint');
        if (!hintEl) {
          hintEl = document.createElement('div');
          hintEl.className = 'pp-install-hint';
          row.insertAdjacentElement('afterend', hintEl);
        }
        // Wyciagnij linie 'Install: <command>' z note.
        const m = String(status.note).match(/Install:\s*(.+)$/);
        if (m) {
          const cmd = m[1].replace(/</g, '&lt;');
          hintEl.innerHTML = `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" style="vertical-align:-2px; margin-right:6px;"><path d="M12 2L2 22h20L12 2z"/><path d="M12 9v6M12 18h.01"/></svg>${ti('profiling.permissions.install_hint', { cmd }, `Install on this system: <code>${cmd}</code>`)}`;
        }
      }
    }
  }
}

function bind(container) {
  container.addEventListener('click', async (e) => {
    const t = e.target.closest('[data-action]');
    if (!t) return;
    const action = t.dataset.action;
    if (action === 'back-mesh') {
      e.preventDefault();
      navigateBack();
      return;
    }
    if (action === 'sudo-validate') {
      const input = container.querySelector('#pp-sudo-input');
      const fb = container.querySelector('#pp-sudo-feedback');
      const pwd = input?.value || '';
      setSudoPassword(pwd);
      if (!fb) return;
      fb.hidden = false;
      fb.className = 'pp-feedback info';
      fb.textContent = ti('profiling.permissions.feedback_validating', null, 'Validating…');
      const res = await validateSudo(pwd);
      if (res.ok) {
        fb.className = 'pp-feedback ok';
        fb.textContent = ti('profiling.permissions.feedback_accepted', null, 'Sudo password accepted by backend.');
      } else if (!res.backendAvailable) {
        fb.className = 'pp-feedback warn';
        fb.textContent = ti('profiling.permissions.feedback_backend_unreachable', null, 'Backend unreachable via binary ValidateSudoRequest — password cached locally, validation will run on session start.');
      } else if (res.reason === 'empty') {
        fb.className = 'pp-feedback warn';
        fb.textContent = ti('profiling.permissions.feedback_empty', null, 'Empty password — nothing to validate.');
      } else {
        fb.className = 'pp-feedback bad';
        fb.textContent = ti('profiling.permissions.feedback_failed', { reason: res.reason || 'unknown' }, `Validation failed (${res.reason || 'unknown'}).`);
      }
      return;
    }
    if (action === 'sudo-clear') {
      clearSudoPassword();
      const input = container.querySelector('#pp-sudo-input');
      if (input) input.value = '';
      const fb = container.querySelector('#pp-sudo-feedback');
      if (fb) {
        fb.hidden = false;
        fb.className = 'pp-feedback info';
        fb.textContent = ti('profiling.permissions.feedback_cleared', null, 'Sudo password cleared from memory.');
      }
      return;
    }
    if (action === 'paths-reset') {
      resetCollectorPaths();
      container.querySelectorAll('input[data-collector-id]').forEach((inp) => {
        const id = inp.getAttribute('data-collector-id');
        const def = DEFAULT_COLLECTORS.find((c) => c.id === id);
        if (def) inp.value = def.defaultPath;
      });
      return;
    }
    if (action === 'limits-reset') {
      writeJsonLocal(KEY_STORAGE_LIMITS, {});
      const cap = container.querySelector('#pp-cap-session');
      const fifo = container.querySelector('#pp-fifo-size');
      const autodel = container.querySelector('#pp-autodelete-days');
      if (cap) cap.value = DEFAULT_STORAGE_LIMITS.capPerSession;
      if (fifo) fifo.value = String(DEFAULT_STORAGE_LIMITS.fifoSize);
      if (autodel) autodel.value = DEFAULT_STORAGE_LIMITS.autoDeleteDays;
      return;
    }
    if (action === 'limits-save') {
      const next = {
        capPerSession: container.querySelector('#pp-cap-session')?.value || DEFAULT_STORAGE_LIMITS.capPerSession,
        fifoSize: Number(container.querySelector('#pp-fifo-size')?.value) || DEFAULT_STORAGE_LIMITS.fifoSize,
        autoDeleteDays: container.querySelector('#pp-autodelete-days')?.value || DEFAULT_STORAGE_LIMITS.autoDeleteDays,
      };
      writeJsonLocal(KEY_STORAGE_LIMITS, next);
      const fb = container.querySelector('#pp-sudo-feedback');
      if (fb) {
        fb.hidden = false;
        fb.className = 'pp-feedback warn';
        fb.textContent = ti('profiling.permissions.feedback_limits_saved', null, 'Storage limits saved locally. Backend does not yet expose an API to change real policy — limits act as a hint for the GUI (FIFO 20, duration 600s are hardcoded in backend).');
      }
      return;
    }
  });

  // Privilege caching toggles — per-browser preference (backend nie ma jeszcze
  // API kontroli polityki sudo, wiec to lokalne hinty dla launch-modal logiki).
  container.querySelectorAll('[data-pref-toggle]').forEach((tog) => {
    const id = tog.getAttribute('data-pref-toggle');
    tog.addEventListener('change', (ev) => {
      const checked = !!(ev.detail?.checked ?? tog.checked);
      if (id === 'remember-sudo') writeBoolLocal(KEY_REMEMBER_SUDO, checked);
      else if (id === 'auto-elevate') writeBoolLocal(KEY_AUTO_ELEVATE, checked);
    });
  });

  // Sudo input — sync na blur.
  const sudoInput = container.querySelector('#pp-sudo-input');
  if (sudoInput) {
    sudoInput.addEventListener('change', () => setSudoPassword(sudoInput.value));
    sudoInput.addEventListener('blur', () => setSudoPassword(sudoInput.value));
  }

  // Collector paths — zapis na change.
  container.querySelectorAll('input[data-collector-id]').forEach((inp) => {
    inp.addEventListener('change', () => {
      const id = inp.getAttribute('data-collector-id');
      setCollectorPath(id, inp.value || '');
    });
  });

  // Source toggles.
  container.querySelectorAll('[data-source-toggle]').forEach((tog) => {
    const id = tog.getAttribute('data-source-toggle');
    tog.addEventListener('change', (ev) => {
      const enabled = !!(ev.detail?.checked ?? tog.checked);
      // Toggle = "enabled" w UI, czyli disabled = !enabled.
      toggleSourceDisabled(id, !enabled);
    });
  });
}
