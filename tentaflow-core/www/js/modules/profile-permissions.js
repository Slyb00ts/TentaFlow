// =============================================================================
// File: modules/profile-permissions.js
// Purpose: Profile Permissions Settings (mockup section 15). Globalny ekran
//          ustawien profiling permissions: cache hasla sudo (in-memory only),
//          override sciezek kolektorow, wylaczone sources, oraz lista
//          status-detektorow (Available / Needs sudo / Limited / Disabled).
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
import '/js/components/tf-button.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-input.js';

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

  const collectorsHtml = DEFAULT_COLLECTORS.map((c) => {
    const cur = overrides[c.id] || c.defaultPath;
    return `
      <div class="pp-path-row">
        <div class="pp-path-name"><span class="pp-vendor pp-v-${escapeHtml(c.vendor)}">${escapeHtml(c.vendor.toUpperCase())}</span>${escapeHtml(c.label)}</div>
        <input class="pp-path-input" type="text" data-collector-id="${escapeHtml(c.id)}" value="${escapeHtml(cur)}" placeholder="${escapeHtml(c.defaultPath)}" />
        <span class="pp-path-status" data-collector-status="${escapeHtml(c.id)}">PENDING</span>
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
        ? `<span class="pp-badge ok">Cached · validated</span>`
        : `<span class="pp-badge warn">Cached · unvalidated</span>`)
    : `<span class="pp-badge muted">Not set</span>`;

  return `
    <div class="profile-permissions">
      <nav class="pr-breadcrumb" aria-label="Breadcrumb">
        <a href="#" data-action="back-mesh">Mesh</a>
        <span class="sep">/</span>
        <span>Profiling</span>
        <span class="sep">/</span>
        <span>Permissions</span>
      </nav>

      <header class="pp-header">
        <h1 class="pp-title">Profile permissions</h1>
        <div class="pp-sub">Per-tab settings — sudo password lives in memory only and is cleared when the tab is closed.</div>
        <div class="pp-actions"><tf-button variant="ghost" size="sm" data-action="back-mesh">Back</tf-button></div>
      </header>

      <section class="pp-card">
        <h3 class="pp-h3">Privilege caching</h3>
        <div class="pp-alert danger">
          <strong>Security:</strong> sudo password is held in JavaScript memory only.
          It is never written to disk, never sent to the server unless you start a
          session, and is wiped when this browser tab closes.
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">Sudo password (in-memory)</div>
            <div class="pp-row-desc">Used by collectors that require root (RAPL, kernel tracing). ${sudoStatusBadge}</div>
          </div>
          <div class="pp-row-control">
            <input id="pp-sudo-input" type="password" autocomplete="off" placeholder="—" value="${escapeHtml(getSudoPassword())}" />
            <tf-button variant="ghost" size="sm" data-action="sudo-validate">Validate</tf-button>
            <tf-button variant="ghost" size="sm" data-action="sudo-clear">Clear</tf-button>
          </div>
        </div>

        <div id="pp-sudo-feedback" class="pp-feedback" hidden></div>
      </section>

      <section class="pp-card">
        <h3 class="pp-h3">Collector binary paths</h3>
        <div class="pp-sub">Override defaults if collectors are installed in non-standard locations. Paths are stored locally per-browser.</div>
        ${collectorsHtml}
        <div class="pp-actions-row">
          <tf-button variant="ghost" size="sm" data-action="paths-reset">Reset to defaults</tf-button>
        </div>
      </section>

      <section class="pp-card">
        <h3 class="pp-h3">Source enablement</h3>
        <div class="pp-sub">Disabled sources are hidden from the launch modal on this browser. Backend capability detection still applies.</div>
        ${sourcesHtml}
      </section>

      <section class="pp-card">
        <h3 class="pp-h3">Storage limits</h3>
        <div class="pp-sub">Mockup #15 — limity zapisu sesji. Wartosci sa lokalne (per browser); zmiana realnej polityki backend wymaga settings.toml.</div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">Storage cap per session</div>
            <div class="pp-row-desc">Maksymalny rozmiar pojedynczej sesji. Sesje przekraczajace cap sa zatrzymane i oznaczone jako truncated.</div>
          </div>
          <input class="pp-path-input" id="pp-cap-session" type="text" value="1 GB" style="width:120px;" />
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">FIFO size (sessions per node)</div>
            <div class="pp-row-desc">Liczba sesji trzymanych na dysku. Najstarsza jest rotowana gdy limit osiagniety.</div>
          </div>
          <input class="pp-path-input" id="pp-fifo-size" type="number" value="20" style="width:120px;" />
        </div>

        <div class="pp-row">
          <div class="pp-row-meta">
            <div class="pp-row-name">Auto-delete failed sessions after</div>
            <div class="pp-row-desc">Sesje zakonczone bledem sa usuwane po tylu dniach.</div>
          </div>
          <input class="pp-path-input" id="pp-autodelete-days" type="text" value="7 days" style="width:120px;" />
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
        badge.textContent = `FOUND${status.version ? ' · ' + status.version.split('\n')[0].slice(0, 24) : ''}`;
        badge.style.background = 'rgba(34,197,94,0.14)';
        badge.style.color = 'var(--tf-success, #22c55e)';
        badge.removeAttribute('title');
      } else {
        badge.textContent = 'N/A';
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
          hintEl.innerHTML = `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" style="vertical-align:-2px; margin-right:6px;"><path d="M12 2L2 22h20L12 2z"/><path d="M12 9v6M12 18h.01"/></svg>Install on this system: <code>${m[1].replace(/</g, '&lt;')}</code>`;
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
      fb.textContent = 'Validating…';
      const res = await validateSudo(pwd);
      if (res.ok) {
        fb.className = 'pp-feedback ok';
        fb.textContent = 'Sudo password accepted by backend.';
      } else if (!res.backendAvailable) {
        fb.className = 'pp-feedback warn';
        fb.textContent = 'Backend nieosiagalny przez binary ValidateSudoRequest — haslo cached lokalnie, walidacja przy starcie sesji.';
      } else if (res.reason === 'empty') {
        fb.className = 'pp-feedback warn';
        fb.textContent = 'Empty password — nothing to validate.';
      } else {
        fb.className = 'pp-feedback bad';
        fb.textContent = `Validation failed (${res.reason || 'unknown'}).`;
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
        fb.textContent = 'Sudo password cleared from memory.';
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
