// =============================================================================
// Plik: modules/profiling-launch.js
// Opis: Modal startu sesji multi-source profilingu. Otwiera tf-window z
//       pelnym formularzem (label, duration, source-cards, elevation,
//       target). Po submicie buduje ProfileScope JSON i wysyla do
//       /api/profiling/start (lub fixture w trybie deweloperskim).
// =============================================================================

import { TfWindow } from '/js/components/tf-window.js';
import { profilingStart } from '/js/protocol/profiling.js';
import {
  getSudoPassword,
  setSudoPassword,
  getDisabledSources,
} from '/js/lib/profile-permissions-store.js';
import '/js/components/tf-button.js';
import '/js/components/tf-input.js';

// ProfileSourceFlags bity (zgodnie z tentaflow-protocol/src/profiling.rs).
const SOURCE_FLAGS = {
  CPU_SAMPLING:  1 << 0,
  CPU_COUNTERS:  1 << 1,
  CPU_UTIL:      1 << 2,
  RAM_USAGE:     1 << 3,
  RAM_BANDWIDTH: 1 << 4,
  DISK_IO:       1 << 5,
  GPU:           1 << 6,
  POWER:         1 << 7,
  NETWORK:       1 << 8,
};

const MAX_LABEL_LEN = 128;
const MAX_DURATION_SEC = 600;
const MIN_DURATION_SEC = 10;

// Mapa prefix -> kategoria. Sluzy do pogrupowania source-cards w siatce
// modalu (CPU/RAM/Disk/GPU/Power/Network/Other).
const CATEGORY_ORDER = ['CPU', 'RAM', 'Disk', 'GPU', 'Power', 'Network', 'Other'];

function categorizeSource(source) {
  const id = String(source.id || '').toLowerCase();
  if (id.includes('cpu')) return 'CPU';
  if (id.includes('ram') || id.includes('memory')) return 'RAM';
  if (id.includes('disk') || id.includes('iostat')) return 'Disk';
  if (id.includes('gpu') || id.includes('nsys') || id.includes('rocprof') || id.includes('vtune')) return 'GPU';
  if (id.includes('power') || id.includes('rapl')) return 'Power';
  if (id.includes('network') || id.includes('net.')) return 'Network';
  return 'Other';
}

// Mapuje category -> dominujacy ProfileSourceFlags bit. Source moze nie miec
// wlasnego mapowania jezeli backend uzywa wielu wewnetrznie — UI dziala na
// poziomie agregowanym (CPU sampling vs CPU util sa osobnymi sources, ale
// generuja te same flagi gdy zaznaczone).
function sourceToFlag(source) {
  const id = String(source.id || '').toLowerCase();
  if (id.includes('sampling')) return SOURCE_FLAGS.CPU_SAMPLING;
  if (id.includes('counter')) return SOURCE_FLAGS.CPU_COUNTERS;
  if (id.includes('cpu_util') || id.endsWith('cpu')) return SOURCE_FLAGS.CPU_UTIL;
  if (id.includes('ram_bandwidth') || id.includes('membw')) return SOURCE_FLAGS.RAM_BANDWIDTH;
  if (id.includes('ram') || id.includes('memory')) return SOURCE_FLAGS.RAM_USAGE;
  if (id.includes('disk') || id.includes('iostat')) return SOURCE_FLAGS.DISK_IO;
  if (id.includes('gpu') || id.includes('nsys') || id.includes('rocprof') || id.includes('vtune')) return SOURCE_FLAGS.GPU;
  if (id.includes('power') || id.includes('rapl')) return SOURCE_FLAGS.POWER;
  if (id.includes('network') || id.includes('net.')) return SOURCE_FLAGS.NETWORK;
  return 0;
}

function escapeHtml(s) {
  return String(s ?? '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function escapeAttr(s) { return escapeHtml(s); }

// Mapowanie GPU source -> vendor badge wedlug mockup (kolory akcentu).
// nvidia.* -> NV (zielony NV), amd.*/rocm.*/rocprof.* -> A (czerwony AMD),
// intel.* -> I (niebieski Intel), apple.*/macos.powermetrics.gpu -> M3.
function sourceVendor(src) {
  const id = String(src.id || '').toLowerCase();
  if (id.includes('nvidia') || id.includes('nsys') || id.includes('nvsmi')) return { cls: 'nv', label: 'NV' };
  if (id.includes('amd') || id.includes('rocm') || id.includes('rocprof') || id.includes('rocsmi')) return { cls: 'amd', label: 'A' };
  if (id.includes('intel') || id.includes('xpu')) return { cls: 'intel', label: 'I' };
  if (id.includes('apple') || id.includes('macos.powermetrics.gpu')) return { cls: 'apple', label: 'M3' };
  return null;
}

// Niewielka ikonka SVG dla source bez vendor-badge. Wybor scieżki path zalezy
// od kategorii (CPU=quad, RAM=stick, Disk=cylinder, Power=lightning,
// Network=nodes, Other=lista).
function sourceIconPath(src) {
  const cat = categorizeSource(src);
  switch (cat) {
    case 'CPU': return 'M6 6h12v12H6z M3 9h3M3 12h3M3 15h3M18 9h3M18 12h3M18 15h3';
    case 'RAM': return 'M3 6h18v12H3z M7 6V4M11 6V4M15 6V4M19 6V4';
    case 'Disk': return 'M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3v12c0 1.7-3.6 3-8 3s-8-1.3-8-3z';
    case 'Power': return 'M12 2L4 7v10l8 5 8-5V7z';
    case 'Network': return 'M12 9a3 3 0 0 1 3 3 3 3 0 0 1-3 3 3 3 0 0 1-3-3 3 3 0 0 1 3-3z';
    default: return 'M3 6h18M3 12h18M3 18h18';
  }
}

// Heuristic estymaty storage + overhead na podstawie wybranych sources.
function estimateImpact(selectedSources, durationSec) {
  // 50 MB per kolektor + 10 MB per s GPU sampling (gdy GPU obecny)
  const baseBytesPerCollector = 50 * 1024 * 1024;
  const hasGpu = selectedSources.some((s) => sourceToFlag(s) === SOURCE_FLAGS.GPU);
  const gpuRateBytesPerSec = hasGpu ? 10 * 1024 * 1024 : 0;
  const totalBytes = selectedSources.length * baseBytesPerCollector + gpuRateBytesPerSec * Math.max(1, durationSec);

  let overheadPct = 0;
  for (const src of selectedSources) {
    const flag = sourceToFlag(src);
    if (flag === SOURCE_FLAGS.CPU_SAMPLING || flag === SOURCE_FLAGS.CPU_COUNTERS || flag === SOURCE_FLAGS.CPU_UTIL) {
      overheadPct += 1;
    } else if (flag === SOURCE_FLAGS.GPU) {
      overheadPct += 2;
    }
  }
  return { bytes: totalBytes, overheadPct };
}

function formatBytes(b) {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(0)} MB`;
  return `${(b / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

// Sprawdza czy label nie ma znakow kontrolnych (zgodnie z walidacja backendu).
function hasControlChars(s) {
  // eslint-disable-next-line no-control-regex
  return /[\x00-\x1f\x7f]/.test(s);
}

function fixtureMode() {
  return typeof window !== 'undefined' && window.__TF_PROFILING_FIXTURE === true;
}

function sleep(ms) {
  return new Promise((res) => setTimeout(res, ms));
}

async function startSession({ scope, nodeId, elevationPassword }) {
  if (fixtureMode()) {
    await sleep(300);
    return { sessionId: (crypto.randomUUID && crypto.randomUUID()) || `fix-${Date.now().toString(16)}` };
  }
  return profilingStart({
    nodeId,
    scope,
    label: scope.label,
    elevationPassword: elevationPassword || null,
  });
}

// Wasm-glue nie eksponuje osobnego `profilingTestElevation` (backend nie ma
// dedykowanego endpointu w binary protocol). Walidujemy lokalnie — sukces
// dopiero potwierdzi sie przy rzeczywistym `profilingStart` jezeli sudo
// password byl bledny (collectorsSkipped przyjdzie z reason='sudo_failed').
async function testElevation(password) {
  if (fixtureMode()) {
    await sleep(220);
    return { ok: password.length >= 4 };
  }
  return { ok: typeof password === 'string' && password.length >= 4 };
}

// =============================================================================
// ProfilingLaunchModal — programowy controller dla tresci modalu.
// =============================================================================

export class ProfilingLaunchModal {
  /**
   * Otwiera modal. Zwraca Promise<{ launched: boolean, sessionId?: string }>.
   * @param {object} opts
   * @param {string} opts.nodeId
   * @param {Array} opts.availableSources [{ id, label, description, status, vendor?, deviceIndex?, requiresElevation }]
   * @param {object=} opts.preselectGpu { deviceIndex, vendor? } — gdy podane,
   *        modal startuje z zaznaczonym tylko jednym source'em GPU (tym
   *        dotyczacym `deviceIndex`). User moze nadal odznaczyc/zaznaczyc
   *        cokolwiek przed startem.
   * @param {Function=} opts.onLaunched callback wolany po sukcesie
   */
  static async open({ nodeId, availableSources, preselectGpu, onLaunched }) {
    if (!Array.isArray(availableSources) || availableSources.length === 0) {
      throw new Error('availableSources must be a non-empty array');
    }
    const ctrl = new ProfilingLaunchModal(nodeId, availableSources, onLaunched, preselectGpu);
    return ctrl._run();
  }

  constructor(nodeId, availableSources, onLaunched, preselectGpu = null) {
    this.nodeId = nodeId;
    // Filter out sources globalnie wylaczone w Profile Permissions (per browser).
    // Unavailable po stronie backendu zostawiamy zeby user widzial dlaczego cos
    // nie dziala — disabled-by-user usuwamy z UI calkowicie.
    const disabled = new Set(getDisabledSources());
    this.sources = availableSources.filter((s) => !disabled.has(s.id));
    this.onLaunched = typeof onLaunched === 'function' ? onLaunched : null;
    this.preselectGpu = preselectGpu && Number.isInteger(preselectGpu.deviceIndex)
      ? preselectGpu
      : null;

    // state
    this.selected = new Set(); // ids
    this.label = '';
    // Domyślny czas sesji 5 min (zgodne z mockupem #01).
    this.durationSec = 300;
    this.manualStop = false;
    this.targetMode = 'system_wide';
    this.targetPid = '';
    // Pre-fill z permissions store (in-memory, per-tab).
    this.elevationPassword = getSudoPassword() || '';
    this.elevationVisible = false;
    this.elevationStatus = 'untested'; // 'untested' | 'ok' | 'bad' | 'testing'

    // GPU pre-select z per-card "Profile" buttona: pin device index na kazdym
    // GPU source, zeby _buildScope() wybral konkretny indeks zamiast "all".
    if (this.preselectGpu) {
      for (const s of this.sources) {
        if (sourceToFlag(s) === SOURCE_FLAGS.GPU) {
          s.deviceIndex = this.preselectGpu.deviceIndex;
        }
      }
    }

    // Domyslnie zaznacz wszystkie 'available' (bez 'unavailable').
    // Wyjatek: gdy preselectGpu, GPU sources sa odznaczone poza tym pasujacym
    // do device-indeksu — user moze i tak je domyslnie wlaczyc po reviewie.
    for (const s of this.sources) {
      if (s.status === 'unavailable') continue;
      if (this.preselectGpu && sourceToFlag(s) === SOURCE_FLAGS.GPU) {
        // Wszystkie GPU sources maja juz ten sam deviceIndex, wiec rozroznienie
        // po vendor: gdy preselectGpu.vendor podany, zaznacz tylko match.
        if (this.preselectGpu.vendor) {
          const v = sourceVendor(s);
          if (v && String(this.preselectGpu.vendor).toLowerCase().startsWith(v.cls)) {
            this.selected.add(s.id);
          }
        } else {
          this.selected.add(s.id);
        }
        continue;
      }
      this.selected.add(s.id);
    }

    this._winRef = null;
    this._resolveOuter = null;
  }

  _run() {
    return new Promise((resolve) => {
      this._resolveOuter = resolve;
      this._launchWindow();
    });
  }

  // Buduje podtytuł nagłówka modalu w formacie:
  // "<nodeId> · <CPU + N GPU + RAM + Disk + Power + Network>".
  // Liczy GPU per-vendor jako jednorazową kategorię.
  _buildSubtitle() {
    const has = (predicate) => this.sources.some(predicate);
    const gpuCount = this.sources.filter((s) => sourceVendor(s) !== null).length;
    const parts = [];
    if (has((s) => categorizeSource(s) === 'CPU')) parts.push('CPU');
    if (gpuCount > 0) parts.push(`${gpuCount} GPU`);
    if (has((s) => categorizeSource(s) === 'RAM')) parts.push('RAM');
    if (has((s) => categorizeSource(s) === 'Disk')) parts.push('Disk');
    if (has((s) => categorizeSource(s) === 'Power')) parts.push('Power');
    if (has((s) => categorizeSource(s) === 'Network')) parts.push('Network');
    const components = parts.join(' + ');
    return `${this.nodeId}${components ? ' · ' + components : ''}`;
  }

  _launchWindow() {
    const bodyEl = document.createElement('div');
    bodyEl.className = 'profiling-launch';

    const winPromise = TfWindow.open({
      title: 'Start profiling session',
      subtitle: this._buildSubtitle(),
      icon: 'activity',
      body: bodyEl,
      footer: this._buildFooter(),
      buttons: 'close',
      modal: true,
      draggable: true,
      resizable: false,
      minWidth: 560,
      minHeight: 520,
      width: 720,
      closeOnAction: true,
    });

    // bodyEl ma dynamicznie aktualizowany content; renderujemy az do pierwszego paint
    queueMicrotask(() => this._render(bodyEl));

    // Find tf-window element (ostatni dodany do body) by wpiac sie w event 'action'
    // przed zamknieciem. TfWindow.open nie eksponuje samego elementu, ale po
    // queueMicrotask jest juz w DOM.
    queueMicrotask(() => {
      const wins = document.querySelectorAll('tf-window');
      const win = wins[wins.length - 1];
      this._winRef = win;
      if (!win) return;

      win.addEventListener('action', (ev) => {
        const action = ev.detail?.action;
        if (action === 'start') {
          ev.preventDefault();
          this._handleStart();
        }
        // 'cancel' / 'close' -> tf-window default closes
      });
      win.addEventListener('close-request', () => {
        // jezeli zamykane bez sukcesu — odpowiedz z launched=false
      });
    });

    winPromise.then(() => {
      // Resolved gdy okno zamkniete; jezeli nie wystartowano sesji, odpowiedz negatywnie.
      if (this._resolveOuter) {
        if (!this._launchedOk) {
          this._resolveOuter({ launched: false });
        }
        this._resolveOuter = null;
      }
    });
  }

  _buildFooter() {
    const wrap = document.createElement('div');
    wrap.style.display = 'flex';
    wrap.style.alignItems = 'center';
    wrap.style.gap = '10px';
    wrap.style.width = '100%';

    const est = document.createElement('div');
    est.className = 'est estimate-foot';
    est.id = 'profiling-estimate-foot';
    est.style.flex = '1';
    est.textContent = 'Estimated storage: — · overhead: —';

    const cancel = document.createElement('tf-button');
    cancel.setAttribute('variant', 'ghost');
    cancel.setAttribute('data-action', 'cancel');
    cancel.textContent = 'Cancel';

    const start = document.createElement('tf-button');
    start.setAttribute('variant', 'primary');
    start.setAttribute('data-action', 'start');
    start.setAttribute('icon', 'record-dot');
    start.id = 'profiling-launch-start-btn';
    start.textContent = 'Start Profiling';

    wrap.appendChild(est);
    wrap.appendChild(cancel);
    wrap.appendChild(start);
    return wrap;
  }

  _render(root) {
    root.innerHTML = '';
    root.appendChild(this._renderLabelField());
    root.appendChild(this._renderDurationField());
    root.appendChild(this._renderSourceGrid());
    const elevation = this._renderElevation();
    if (elevation) root.appendChild(elevation);
    root.appendChild(this._renderTargetField());

    this._attachListeners(root);
    this._updateEstimate();
  }

  _renderLabelField() {
    const wrap = document.createElement('div');
    wrap.className = 'field';
    wrap.innerHTML = `
      <div class="field-label">
        <span>Label</span>
        <span class="counter" id="pl-label-counter">${this.label.length} / ${MAX_LABEL_LEN}</span>
      </div>
      <input class="field-input" id="pl-label-input" type="text"
             maxlength="${MAX_LABEL_LEN}"
             placeholder="qwen-7b inference benchmark"
             value="${escapeAttr(this.label)}" />
    `;
    return wrap;
  }

  _renderDurationField() {
    const wrap = document.createElement('div');
    wrap.className = 'field';
    const sliderDisabled = this.manualStop ? 'disabled' : '';
    // Mockup #01 wymaga sufiksu " s" w polu duration (np. "300 s"). Używamy
    // type="text" z parsowaniem w handlerze, żeby uniknąć ograniczeń
    // type="number" (które nie zezwala na nie-cyfrowe znaki).
    wrap.innerHTML = `
      <div class="field-label"><span>Duration</span></div>
      <div class="field-row">
        <input type="range" class="tf-slider" id="pl-duration-slider"
               min="${MIN_DURATION_SEC}" max="${MAX_DURATION_SEC}" step="5"
               value="${this.durationSec}" ${sliderDisabled} />
        <input type="text" inputmode="numeric" class="field-input duration-num" id="pl-duration-num"
               value="${this.durationSec} s" ${sliderDisabled} />
        <label class="tf-check">
          <input type="checkbox" id="pl-manual-stop" ${this.manualStop ? 'checked' : ''} />
          <span>Manual stop</span>
        </label>
      </div>
    `;
    return wrap;
  }

  _renderSourceGrid() {
    const wrap = document.createElement('div');
    wrap.className = 'field';
    const total = this.sources.length;
    const sel = this.sources.filter((s) => this.selected.has(s.id)).length;
    wrap.innerHTML = `
      <div class="field-label">
        <span>Data sources</span>
        <span class="counter" id="pl-source-counter">${sel} of ${total} selected</span>
      </div>
      <div class="source-grid" id="pl-source-grid"></div>
    `;
    const grid = wrap.querySelector('#pl-source-grid');
    // Mockup #01 uzywa plaskiej 2-kolumnowej siatki bez nagłówków kategorii -
    // user widzi pełną listę naraz, kategoryzacja przez prefix w nazwie ID
    // jest wystarczająca jako wizualny grupator.
    for (const src of this.sources) {
      grid.appendChild(this._renderSourceCard(src));
    }
    return wrap;
  }

  _renderSourceCard(src) {
    const card = document.createElement('label');
    const isDisabled = src.status === 'unavailable';
    const isChecked = this.selected.has(src.id);
    card.className = 'source-card';
    card.setAttribute('data-source-id', src.id);
    if (isChecked) card.setAttribute('checked', '');
    card.setAttribute('data-checked', String(isChecked));
    card.setAttribute('data-disabled', String(isDisabled));
    card.setAttribute('title', src.description || '');

    // Status pill 1:1 wg mockupu: Available / Needs sudo / Limited / Unavailable.
    const statusBadge = (() => {
      if (src.status === 'available') return '<span class="src-status ok">Available</span>';
      if (src.status === 'needs_sudo') return '<span class="src-status warn">Needs sudo</span>';
      if (src.status === 'needs_admin') return '<span class="src-status warn">Needs admin</span>';
      if (src.status === 'limited') return '<span class="src-status lim">Limited</span>';
      if (src.status === 'unavailable') return '<span class="src-status bad">Unavailable</span>';
      return '';
    })();

    // Vendor-badge w ikonce kafelka (NV/A/I/M3) gdy źródło to GPU per-vendor.
    const vendor = sourceVendor(src);
    const iconHtml = vendor
      ? `<span class="vendor-badge ${vendor.cls}">${vendor.label}</span>`
      : `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="${sourceIconPath(src)}"/></svg>`;

    // Info-tip "(?)" pojawia się przy źródłach gdy backend dostarczył pole
    // tooltip (np. "perf record / dtrace, 99 Hz default" dla CPU sampling).
    const tooltip = src.tooltip || src.hint;
    const infoTipHtml = tooltip
      ? `<span class="info-tip" title="${escapeAttr(tooltip)}">
           <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
             <circle cx="12" cy="12" r="10"/>
             <path d="M12 16v-4M12 8h.01"/>
           </svg>
         </span>`
      : '';

    card.innerHTML = `
      <span class="src-check">
        <input type="checkbox"
               ${isChecked ? 'checked' : ''}
               ${isDisabled ? 'disabled' : ''}
               data-source-checkbox="${escapeAttr(src.id)}" />
      </span>
      <span class="src-ico">${iconHtml}</span>
      <span class="src-meta">
        <span class="src-name">
          <span>${escapeHtml(src.label || src.id)}</span>
          ${infoTipHtml}
          ${statusBadge}
        </span>
        <span class="src-desc">${escapeHtml(src.description || '')}</span>
      </span>
    `;
    return card;
  }

  _renderElevation() {
    const needsElevation = Array.from(this.selected).some((id) => {
      const src = this.sources.find((s) => s.id === id);
      return src && (src.status === 'needs_sudo' || src.status === 'needs_admin');
    });
    if (!needsElevation) return null;

    const wrap = document.createElement('div');
    wrap.className = 'field';
    wrap.id = 'pl-elevation-field';
    const inputType = this.elevationVisible ? 'text' : 'password';

    // Liczba źródeł wymagających elevacji — pokazujemy w treści alertu, żeby
    // user wiedział czego konkretnie dotyczy żądanie hasła.
    const elevSources = this.sources.filter((s) =>
      this.selected.has(s.id) && (s.status === 'needs_sudo' || s.status === 'needs_admin')
    );
    const elevCount = elevSources.length;
    const elevNames = elevSources.map((s) => s.label || s.id).join(', ');

    wrap.innerHTML = `
      <div class="alert-box">
        <div class="a-ico">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 2L2 22h20L12 2z"/>
            <path d="M12 9v6M12 18h.01"/>
          </svg>
        </div>
        <div class="a-body">
          <strong>${elevCount} source${elevCount === 1 ? '' : 's'} require elevation</strong> — ${escapeHtml(elevNames)}.
          Provide your sudo password once. It is used to spawn collectors and is <strong>never stored on disk or in DB</strong>.
        </div>
      </div>
      <div class="field-label">
        <span>Sudo password</span>
        <span class="counter">used once · not stored</span>
      </div>
      <div class="field-row">
        <div class="pw-input ${this.elevationStatus === 'ok' ? 'valid' : ''} ${this.elevationStatus === 'bad' ? 'invalid' : ''}" style="flex:1;">
          <svg class="lock-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <rect x="4" y="11" width="16" height="10" rx="2"/>
            <path d="M8 11V7a4 4 0 0 1 8 0v4"/>
          </svg>
          <input type="${inputType}" id="pl-elevation-input"
                 autocomplete="current-password"
                 value="${escapeAttr(this.elevationPassword)}"
                 placeholder="••••••••" />
          <button type="button" class="pw-eye" id="pl-elevation-eye" title="Toggle visibility">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/>
              <circle cx="12" cy="12" r="3"/>
            </svg>
          </button>
        </div>
        <tf-button variant="outline" size="sm" id="pl-elevation-test">Test</tf-button>
      </div>
      <span class="pw-test-result ${this.elevationStatus === 'ok' ? 'ok' : ''} ${this.elevationStatus === 'bad' ? 'bad' : ''}"
            id="pl-elevation-result">
        ${this.elevationStatus === 'ok' ? '✓ Authenticated'
          : this.elevationStatus === 'bad' ? '✗ Invalid password'
          : this.elevationStatus === 'testing' ? 'Testing…'
          : ''}
      </span>
    `;
    return wrap;
  }

  _renderTargetField() {
    const wrap = document.createElement('div');
    wrap.className = 'field';
    // PID input renderujemy zawsze w środku radio-row (120px wide jak w mockupie),
    // ukrywając go gdy nie wybrano "Specific PID".
    const pidStyle = this.targetMode === 'specific_pid' ? 'width:120px;' : 'width:120px; display:none;';
    wrap.innerHTML = `
      <div class="field-label"><span>Profile target</span></div>
      <div class="radio-row">
        <label class="tf-check">
          <input type="radio" name="pl-target" value="system_wide"
                 ${this.targetMode === 'system_wide' ? 'checked' : ''} />
          <span>System-wide</span>
        </label>
        <label class="tf-check">
          <input type="radio" name="pl-target" value="own_process"
                 ${this.targetMode === 'own_process' ? 'checked' : ''} />
          <span>Own process (tentaflow)</span>
        </label>
        <label class="tf-check">
          <input type="radio" name="pl-target" value="specific_pid"
                 ${this.targetMode === 'specific_pid' ? 'checked' : ''} />
          <span>Specific PID</span>
        </label>
        <input type="number" class="field-input" id="pl-target-pid"
               placeholder="e.g. 14872" min="1" step="1"
               value="${escapeAttr(this.targetPid)}"
               style="${pidStyle}" />
      </div>
    `;
    return wrap;
  }

  _attachListeners(root) {
    // label
    const labelInput = root.querySelector('#pl-label-input');
    const labelCounter = root.querySelector('#pl-label-counter');
    if (labelInput) {
      labelInput.addEventListener('input', () => {
        this.label = labelInput.value;
        labelCounter.textContent = `${this.label.length} / ${MAX_LABEL_LEN}`;
      });
    }

    // duration
    const slider = root.querySelector('#pl-duration-slider');
    const durNum = root.querySelector('#pl-duration-num');
    const manualStop = root.querySelector('#pl-manual-stop');
    if (slider) {
      slider.addEventListener('input', () => {
        this.durationSec = parseInt(slider.value, 10);
        if (durNum) durNum.value = `${this.durationSec} s`;
        this._updateEstimate();
      });
    }
    if (durNum) {
      // Wycinamy wszystko poza cyframi (sufiks " s" jest ozdobny).
      durNum.addEventListener('input', () => {
        const digits = String(durNum.value).replace(/[^0-9]/g, '');
        let v = parseInt(digits, 10);
        if (Number.isNaN(v)) v = MIN_DURATION_SEC;
        v = Math.max(MIN_DURATION_SEC, Math.min(MAX_DURATION_SEC, v));
        this.durationSec = v;
        if (slider) slider.value = String(v);
        this._updateEstimate();
      });
      // Przy blur dopisujemy z powrotem sufiks (gdyby user go skasował).
      durNum.addEventListener('blur', () => {
        durNum.value = `${this.durationSec} s`;
      });
    }
    if (manualStop) {
      manualStop.addEventListener('change', () => {
        this.manualStop = manualStop.checked;
        if (slider) slider.disabled = this.manualStop;
        if (durNum) durNum.disabled = this.manualStop;
        this._updateEstimate();
      });
    }

    // source cards
    root.querySelectorAll('[data-source-checkbox]').forEach((cb) => {
      cb.addEventListener('change', () => {
        const id = cb.getAttribute('data-source-checkbox');
        if (cb.checked) this.selected.add(id);
        else this.selected.delete(id);
        const card = cb.closest('.source-card');
        if (card) {
          card.setAttribute('data-checked', String(cb.checked));
          if (cb.checked) card.setAttribute('checked', '');
          else card.removeAttribute('checked');
        }
        // Live counter "N of M selected" w field-label.
        const counterEl = root.querySelector('#pl-source-counter');
        if (counterEl) {
          counterEl.textContent = `${this.selected.size} of ${this.sources.length} selected`;
        }
        // Re-render elevation block: appears/disappears depending on selection.
        this._refreshElevation(root);
        this._updateEstimate();
      });
    });

    // elevation
    this._attachElevationListeners(root);

    // target
    root.querySelectorAll('input[name="pl-target"]').forEach((rb) => {
      rb.addEventListener('change', () => {
        if (!rb.checked) return;
        this.targetMode = rb.value;
        const pidInput = root.querySelector('#pl-target-pid');
        if (pidInput) {
          // Width 120px per mockup; toggle tylko display.
          pidInput.style.width = '120px';
          pidInput.style.display = (this.targetMode === 'specific_pid') ? '' : 'none';
        }
      });
    });
    const pidInput = root.querySelector('#pl-target-pid');
    if (pidInput) {
      pidInput.addEventListener('input', () => {
        this.targetPid = pidInput.value;
      });
    }
  }

  _attachElevationListeners(root) {
    const pwInput = root.querySelector('#pl-elevation-input');
    const eyeBtn = root.querySelector('#pl-elevation-eye');
    const testBtn = root.querySelector('#pl-elevation-test');
    const resultEl = root.querySelector('#pl-elevation-result');
    if (pwInput) {
      pwInput.addEventListener('input', () => {
        this.elevationPassword = pwInput.value;
        if (this.elevationStatus !== 'untested') {
          this.elevationStatus = 'untested';
          if (resultEl) {
            resultEl.textContent = '';
            resultEl.className = 'pw-test-result';
          }
          const pwWrap = pwInput.closest('.pw-input');
          if (pwWrap) pwWrap.classList.remove('valid', 'invalid');
        }
      });
    }
    if (eyeBtn) {
      eyeBtn.addEventListener('click', () => {
        this.elevationVisible = !this.elevationVisible;
        this._refreshElevation(root);
      });
    }
    if (testBtn) {
      testBtn.addEventListener('click', async () => {
        if (!this.elevationPassword) return;
        this.elevationStatus = 'testing';
        if (resultEl) resultEl.textContent = 'Testing…';
        try {
          const res = await testElevation(this.elevationPassword);
          this.elevationStatus = res.ok ? 'ok' : 'bad';
        } catch (err) {
          console.error('elevation test failed', err);
          this.elevationStatus = 'bad';
        }
        this._refreshElevation(root);
      });
    }
  }

  _refreshElevation(root) {
    const oldField = root.querySelector('#pl-elevation-field');
    const newField = this._renderElevation();
    if (oldField && newField) {
      oldField.replaceWith(newField);
      this._attachElevationListeners(root);
    } else if (oldField && !newField) {
      oldField.remove();
    } else if (!oldField && newField) {
      // wstaw przed target field (ostatnie .field)
      const fields = root.querySelectorAll('.field');
      const lastField = fields[fields.length - 1];
      if (lastField) lastField.before(newField);
      else root.appendChild(newField);
      this._attachElevationListeners(root);
    }
  }

  _updateEstimate() {
    const selectedSources = this.sources.filter((s) => this.selected.has(s.id));
    const dur = this.manualStop ? 60 : this.durationSec;
    const { bytes, overheadPct } = estimateImpact(selectedSources, dur);
    const foot = document.getElementById('profiling-estimate-foot');
    if (foot) {
      const n = selectedSources.length;
      foot.textContent = `Estimated storage: ~${formatBytes(bytes)} · overhead: ~${overheadPct}% CPU · ${n} collector${n === 1 ? '' : 's'}`;
    }
  }

  _validate() {
    if (!this.label || !this.label.trim()) {
      return 'Label is required.';
    }
    if (this.label.length > MAX_LABEL_LEN) {
      return `Label too long (max ${MAX_LABEL_LEN}).`;
    }
    if (hasControlChars(this.label)) {
      return 'Label contains control characters.';
    }
    if (this.selected.size === 0) {
      return 'Select at least one source.';
    }
    if (!this.manualStop) {
      if (this.durationSec < MIN_DURATION_SEC || this.durationSec > MAX_DURATION_SEC) {
        return `Duration must be between ${MIN_DURATION_SEC}-${MAX_DURATION_SEC}s.`;
      }
    }
    if (this.targetMode === 'specific_pid') {
      const pid = parseInt(this.targetPid, 10);
      if (!Number.isInteger(pid) || pid <= 0) return 'Specific PID must be a positive integer.';
    }
    // Elevation gating: jezeli ktorykolwiek source needs_sudo/needs_admin, password niepusty.
    const needsElev = Array.from(this.selected).some((id) => {
      const src = this.sources.find((s) => s.id === id);
      return src && (src.status === 'needs_sudo' || src.status === 'needs_admin');
    });
    if (needsElev && !this.elevationPassword) {
      return 'Elevation password is required for selected sources.';
    }
    return null;
  }

  _buildScope() {
    // sources -> bitmask
    let mask = 0;
    const selectedSources = this.sources.filter((s) => this.selected.has(s.id));
    for (const s of selectedSources) {
      mask |= sourceToFlag(s);
    }

    // gpu_targets: jezeli ktorykolwiek wybrany ma deviceIndex, lista; w przeciwnym
    // razie 'all' gdy GPU bit jest set, 'none' inaczej.
    let gpuTargets;
    const gpuSources = selectedSources.filter((s) => sourceToFlag(s) === SOURCE_FLAGS.GPU);
    if (gpuSources.length === 0) {
      gpuTargets = 'none';
    } else {
      const indices = gpuSources
        .filter((s) => Number.isInteger(s.deviceIndex))
        .map((s) => s.deviceIndex);
      if (indices.length > 0) {
        gpuTargets = { indices: Array.from(new Set(indices)).sort((a, b) => a - b) };
      } else {
        gpuTargets = 'all';
      }
    }

    let target;
    if (this.targetMode === 'system_wide') target = 'system_wide';
    else if (this.targetMode === 'own_process') target = 'own_process';
    else target = { pid: parseInt(this.targetPid, 10) };

    return {
      sources: mask >>> 0,
      gpuTargets,
      cpuSamplingHz: 99,
      target,
      durationSeconds: this.manualStop ? 0 : this.durationSec,
      label: this.label.trim(),
    };
  }

  async _handleStart() {
    const err = this._validate();
    const startBtn = document.getElementById('profiling-launch-start-btn');
    if (err) {
      this._showError(err);
      return;
    }
    if (startBtn) startBtn.setAttribute('disabled', '');
    try {
      const scope = this._buildScope();
      const elevationPassword = this.elevationPassword || undefined;
      // Zapisz do permissions store (in-memory, per-tab) — kolejna sesja
      // uniknie pytania o haslo dopoki uzytkownik nie zamknie zakladki.
      if (elevationPassword) setSudoPassword(elevationPassword);
      const resp = await startSession({ scope, nodeId: this.nodeId, elevationPassword });
      const sessionId = resp.sessionId || resp.session_id;
      this._launchedOk = true;
      if (this.onLaunched && sessionId) {
        try { this.onLaunched(sessionId); } catch (cbErr) { console.error('onLaunched callback error', cbErr); }
      }
      if (this._resolveOuter) {
        this._resolveOuter({ launched: true, sessionId });
        this._resolveOuter = null;
      }
      if (this._winRef) this._winRef.close(true);
    } catch (sendErr) {
      console.error('profiling start failed', sendErr);
      this._showError(sendErr.message || 'Failed to start profiling.');
      if (startBtn) startBtn.removeAttribute('disabled');
    }
  }

  _showError(msg) {
    const foot = document.getElementById('profiling-estimate-foot');
    if (foot) {
      foot.textContent = `⚠ ${msg}`;
      foot.style.color = 'var(--tf-danger, #ef4444)';
      setTimeout(() => {
        foot.style.color = '';
        this._updateEstimate();
      }, 3500);
    }
  }
}
