// =============================================================================
// Plik: modules/mesh-detail-nsight.js
// Opis: Integracja Nsight Systems w widoku szczegolow noda mesh. Modal startu
//       sesji (tf-window), badge "REC" w topbarze z countdown/elapsed,
//       lista sesji w tf-table z akcjami (otworz raport / pobierz / usun).
//       Stan modulu (activeSession + interval) jest scope'owany do biezacego
//       node id i czyszczony przez `cleanup()`.
// =============================================================================

import { escapeHtml, escapeAttr, toast, formatBytes } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { TfWindow } from '/js/components/tf-window.js';
import { nsightStart, nsightStop, nsightSessions, nsightDelete, nsightDownload } from '/js/protocol/nsight.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-select.js';
import '/js/components/tf-input.js';
import '/js/components/tf-table.js';
import '/js/components/tf-menu.js';
import '/js/components/tf-window.js';

// ---- Stan -----------------------------------------------------------------
// activeSession trzymamy w module zamiast na obiekcie noda — pozwala to
// odporniej przezyc rerender mesh-detail (loadNode -> renderDetail co 2-5s).
// Czyszczone w cleanupNsight() gdy uzytkownik wraca do listy meshu.
let activeSession = null;       // { sessionId, startedAtMs, scope, label, durationSecs, autoStopAt }
let activeNodeId = null;        // node id ktorego dotyczy activeSession
let countdownInterval = null;   // setInterval do odswiezania badge co 1s
let pollSessionsInterval = null; // setInterval do polling listy sesji po auto-stop
let cachedSessions = [];        // ostatnio pobrana lista sesji (snapshot)
let lastSessionsNodeId = null;  // dla ktorego noda cachedSessions zostalo pobrane

let pendingActionsTarget = null; // tf-menu: ktora sesja aktualnie pokazana
let onChangeCallback = null;     // wywolanie do mesh-detail.js po start/stop/delete
let boundActionsRoot = null;     // root na ktorym wisi nasz click listener
let boundActionsHandler = null;  // referencja handlera do removeEventListener

// ---- Public API ------------------------------------------------------------

export function initNsight({ onChange } = {}) {
  onChangeCallback = typeof onChange === 'function' ? onChange : null;
}

export function cleanupNsight() {
  if (countdownInterval) { clearInterval(countdownInterval); countdownInterval = null; }
  if (pollSessionsInterval) { clearInterval(pollSessionsInterval); pollSessionsInterval = null; }
  if (boundActionsRoot && boundActionsHandler) {
    boundActionsRoot.removeEventListener('click', boundActionsHandler);
  }
  boundActionsRoot = null;
  boundActionsHandler = null;
  activeSession = null;
  activeNodeId = null;
  cachedSessions = [];
  lastSessionsNodeId = null;
}

// Pobiera liste sesji z backendu i cache'uje. Wolane z mesh-detail przy loadNode.
export async function loadSessions(nodeId) {
  if (!nodeId) return [];
  try {
    const resp = await nsightSessions({ nodeId });
    cachedSessions = Array.isArray(resp.sessions) ? resp.sessions : [];
    lastSessionsNodeId = nodeId;
  } catch (_err) {
    // Bez sesji — np. brak nsys, brak handlera; nie ubijaj calego widoku.
    cachedSessions = [];
    lastSessionsNodeId = nodeId;
  }
  return cachedSessions;
}

// Czy node ma jakikolwiek GPU NVIDIA (warunek wstepny — nsys ma sens tylko na NVIDIA).
// Case-insensitive zeby wytrzymac drift backendu (lowercase / Capital / NVIDIA).
function hasNvidiaGpu(node) {
  if (!node) return false;
  const gpus = Array.isArray(node.gpus) ? node.gpus : [];
  return gpus.some((g) => g && String(g.vendor || '').toLowerCase() === 'nvidia');
}

// Czy node wspiera klasyczne nsight profilowanie (NVIDIA + nsys w PATH).
export function isNsightCapable(node) {
  if (!node) return false;
  if (node.nsys_available !== true) return false;
  return hasNvidiaGpu(node);
}

// Czy node ma JAKIEKOLWIEK kolektory profilowania multi-source dostepne
// (Linux/macOS/Windows CPU/RAM/Disk/Power/GPU). Heartbeat advertise'uje liste
// id-ow kolektorow (np. 'macos.powermetrics.gpu', 'linux.proc.cpu_util').
// Pusta lista = peer nie obsluguje multi-source profiling V2.
export function hasProfilingCollectors(node) {
  if (!node) return false;
  const list = node.profiling_collectors_available;
  return Array.isArray(list) && list.length > 0;
}

// Wspolny gate na pokazanie przycisku Profile w GUI: albo legacy nsys
// (NVIDIA host) albo multi-source collectors (kazda platforma).
export function isProfileCapable(node) {
  return isNsightCapable(node) || hasProfilingCollectors(node);
}

// Wykrywa platforme docelowa do wyboru komendy instalacji. Preferuje `node.platform`
// z heartbeatu (linux/macos/windows/android/ios). Dla local node fallback do
// navigator.platform — heartbeat moze nie zdazyc dolecziec przy pierwszym renderze.
function detectPlatformForInstall(node) {
  const raw = String(node?.platform || '').toLowerCase();
  if (raw === 'linux' || raw === 'macos' || raw === 'windows' || raw === 'android' || raw === 'ios') {
    return raw;
  }
  if (typeof navigator !== 'undefined' && navigator.platform) {
    const np = navigator.platform.toLowerCase();
    if (np.includes('mac')) return 'macos';
    if (np.includes('win')) return 'windows';
    if (np.includes('linux')) return 'linux';
  }
  return 'linux';
}

// HTML do wstrzykniecia w `card-head` GPU. Pokazuje sie tylko dla NVIDIA + nsys.
export function gpuProfileButtonHtml(node, gpu, idx) {
  if (!node || node.nsys_available !== true) return '';
  if (!gpu || String(gpu.vendor || '').toLowerCase() !== 'nvidia') return '';
  return `
    <tf-button size="sm" variant="ghost" data-action="nsight-profile-card" data-gpu-idx="${idx}" title="${escapeAttr(I18n.t('nsight.profile_btn'))}">
      <svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-record"/></svg>
      <span>${escapeHtml(I18n.t('nsight.profile_btn'))}</span>
    </tf-button>
  `;
}

// Tylko etykieta countdown/elapsed dla aktywnej sesji REC. Pozwala mesh-detail
// odswiezyc tekst chipa (1Hz) bez pelnego rerender'u widoku.
export function activeRecLabel(node) {
  if (!activeSession || !node || activeNodeId !== node.node_id) return null;
  const elapsed = Math.max(0, Math.floor((Date.now() - activeSession.startedAtMs) / 1000));
  return activeSession.durationSecs > 0
    ? formatCountdown(Math.max(0, activeSession.durationSecs - elapsed))
    : formatElapsed(elapsed);
}

// HTML do wstrzykniecia w `mesh-detail-actions`. Profile node + badge gdy aktywna sesja.
export function topbarHtml(node) {
  const parts = [];
  // Badge aktywnej sesji ma priorytet — jak nagrywa, pokazujemy stop.
  if (activeSession && activeNodeId === node.node_id) {
    const elapsed = Math.max(0, Math.floor((Date.now() - activeSession.startedAtMs) / 1000));
    const label = activeSession.durationSecs > 0
      ? formatCountdown(Math.max(0, activeSession.durationSecs - elapsed))
      : formatElapsed(elapsed);
    parts.push(`
      <span class="nsight-rec-wrap">
        <tf-chip status="recording" dot data-nsight-rec-chip>REC ${escapeHtml(label)}</tf-chip>
        <tf-button size="sm" variant="danger" data-action="nsight-stop-session">
          <svg width="12" height="12" fill="currentColor" aria-hidden="true"><use href="#i-stop"/></svg>
          <span>${escapeHtml(I18n.t('nsight.stop'))}</span>
        </tf-button>
      </span>
    `);
  } else if (isProfileCapable(node)) {
    // Pokazujemy Profile button gdy node ma ALBO nsys (NVIDIA) ALBO
    // dostepne kolektory multi-source (Linux/macOS/Windows CPU/GPU/IO).
    parts.push(`
      <tf-button size="sm" variant="ghost" data-action="nsight-profile-node" title="${escapeAttr(I18n.t('nsight.profile_node_btn'))}">
        <svg width="12" height="12" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="#i-record"/></svg>
        <span>${escapeHtml(I18n.t('nsight.profile_node_btn'))}</span>
      </tf-button>
    `);
  } else if (hasNvidiaGpu(node) && node.nsys_available === false) {
    // NVIDIA jest, ale brak nsys w PATH — chip dziala jak link "scroll do install card".
    parts.push(`
      <tf-chip status="warn" data-action="nsight-scroll-install" title="${escapeAttr(I18n.t('nsight.error.not_available'))}">
        ${escapeHtml(I18n.t('nsight.status.unavailable'))}
      </tf-chip>
    `);
  } else {
    // Brak nsys i brak collectorow = naprawde nic nie potrafi profilowac.
    // Pokazujemy informacyjny chip zeby user wiedzial dlaczego brak przycisku.
    parts.push(`
      <tf-chip status="muted" title="${escapeAttr(I18n.t('nsight.status.no_nvidia_tooltip'))}">
        ${escapeHtml(I18n.t('nsight.status.no_nvidia'))}
      </tf-chip>
    `);
  }
  return parts.join('');
}

// Karta z instrukcja instalacji nsys gdy node ma NVIDIA ale nsys nie jest dostepny.
// Renderowana w mesh-detail.js obok renderProfilingWrap. Pusta gdy nsys juz dziala
// albo gdy nie ma NVIDIA (innych vendorow nsys nie wspiera).
export function nsightInstallHintHtml(node) {
  if (!node) return '';
  if (node.nsys_available === true) return '';
  if (!hasNvidiaGpu(node)) return '';

  const platform = detectPlatformForInstall(node);
  const docsUrl = 'https://developer.nvidia.com/nsight-systems';

  // Per-platforma: na linux pokazujemy obie wersje (apt + dnf) bo heartbeat nie
  // rozroznia distro. Wybor zostawiamy uzytkownikowi.
  let cmds = '';
  if (platform === 'linux' || platform === 'android') {
    const aptCmd = 'sudo apt install nvidia-nsight-systems';
    const dnfCmd = 'sudo dnf install cuda-nsight-systems-12-x';
    cmds = `
      ${installCmdRow(I18n.t('nsight.install.linux_apt_label'), aptCmd)}
      ${installCmdRow(I18n.t('nsight.install.linux_dnf_label'), dnfCmd)}
    `;
  } else if (platform === 'windows') {
    cmds = `
      <div class="nsight-install-cmd-row">
        <div class="nsight-install-cmd-label">${escapeHtml(I18n.t('nsight.install.windows_label'))}</div>
        <div class="nsight-install-cmd-windows">
          <a href="${escapeAttr(docsUrl)}" target="_blank" rel="noopener noreferrer">${escapeHtml(docsUrl)}</a>
        </div>
      </div>
    `;
  } else if (platform === 'macos' || platform === 'ios') {
    return `
      <div class="nsight-install-card" data-nsight-install-card>
        <h3 class="mesh-section-title">${escapeHtml(I18n.t('nsight.install.title'))}</h3>
        <div class="nsight-install-warn">${escapeHtml(I18n.t('nsight.install.macos_unsupported'))}</div>
      </div>
    `;
  }

  return `
    <div class="nsight-install-card" data-nsight-install-card>
      <h3 class="mesh-section-title">${escapeHtml(I18n.t('nsight.install.title'))}</h3>
      <p class="nsight-install-desc">${escapeHtml(I18n.t('nsight.install.description'))}</p>
      ${cmds}
      <div class="nsight-install-docs">
        <a href="${escapeAttr(docsUrl)}" target="_blank" rel="noopener noreferrer">${escapeHtml(I18n.t('nsight.install.docs_link'))}</a>
      </div>
    </div>
  `;
}

function installCmdRow(label, cmd) {
  return `
    <div class="nsight-install-cmd-row">
      <div class="nsight-install-cmd-label">${escapeHtml(label)}</div>
      <div class="nsight-install-cmd">
        <code>${escapeHtml(cmd)}</code>
        <tf-button size="sm" variant="ghost" data-action="nsight-copy-cmd" data-cmd="${escapeAttr(cmd)}">
          ${escapeHtml(I18n.t('nsight.install.copy_btn'))}
        </tf-button>
      </div>
    </div>
  `;
}

// HTML sekcji listy sesji (pod GPU). Ukryty gdy nsys niedostepny i brak historii.
export function sessionsSectionHtml(node) {
  if (!node || node.nsys_available !== true) return '';
  const sessions = (lastSessionsNodeId === node.node_id) ? cachedSessions : [];
  const count = sessions.length;
  const rows = sessions.length === 0
    ? `<div class="empty">${escapeHtml(I18n.t('nsight.sessions.empty'))}</div>`
    : `<table class="data-table">
        <thead>
          <tr>
            <th>${escapeHtml(I18n.t('nsight.sessions.col.timestamp'))}</th>
            <th>${escapeHtml(I18n.t('nsight.sessions.col.label'))}</th>
            <th>${escapeHtml(I18n.t('nsight.sessions.col.scope'))}</th>
            <th>${escapeHtml(I18n.t('nsight.sessions.col.duration'))}</th>
            <th>${escapeHtml(I18n.t('nsight.sessions.col.status'))}</th>
            <th>${escapeHtml(I18n.t('nsight.sessions.col.actions'))}</th>
          </tr>
        </thead>
        <tbody>${sessions.map((s) => sessionRowHtml(s)).join('')}</tbody>
      </table>`;
  return `
    <h3 class="mesh-section-title">${escapeHtml(I18n.t('nsight.sessions.title'))}<span class="section-count">${count}</span></h3>
    <div class="mesh-detail-card sessions-card">${rows}</div>
  `;
}

function sessionRowHtml(s) {
  const ts = formatDateTime(s.startedAtMs);
  const labelTxt = s.label ? escapeHtml(s.label) : '<span class="muted">—</span>';
  const scopeTxt = formatScopeForDisplay(s.scope);
  const durationTxt = formatDurationForRow(s);
  const statusChip = statusChipHtml(s.status);
  const errorRow = s.error
    ? `<div class="session-error" title="${escapeAttr(s.error)}">${escapeHtml(truncate(s.error, 80))}</div>`
    : '';
  const isRunning = s.status === 'Running' || s.status === 'Stopping';
  const isDone = s.status === 'Done';
  const reportDisabled = isDone ? '' : 'disabled';
  const downloadDisabled = isDone ? '' : 'disabled';
  return `
    <tr data-session-id="${escapeAttr(s.sessionId)}">
      <td>${escapeHtml(ts)}</td>
      <td>${labelTxt}${errorRow}</td>
      <td><span class="session-scope">${escapeHtml(scopeTxt)}</span></td>
      <td>${escapeHtml(durationTxt)}</td>
      <td>${statusChip}</td>
      <td class="session-actions">
        <tf-button size="sm" variant="ghost" ${reportDisabled} data-action="nsight-open-report" data-session-id="${escapeAttr(s.sessionId)}" title="${escapeAttr(I18n.t('nsight.action.open'))}">
          ${escapeHtml(I18n.t('nsight.action.open'))}
        </tf-button>
        <tf-button size="sm" variant="ghost" ${downloadDisabled} data-action="nsight-download" data-session-id="${escapeAttr(s.sessionId)}" title="${escapeAttr(I18n.t('nsight.action.download'))}">
          ${escapeHtml(I18n.t('nsight.action.download'))}
        </tf-button>
        <tf-button size="sm" variant="ghost" data-action="nsight-delete" data-session-id="${escapeAttr(s.sessionId)}" title="${escapeAttr(I18n.t('nsight.action.delete'))}">
          ${escapeHtml(I18n.t('nsight.action.delete'))}
        </tf-button>
      </td>
    </tr>
  `;
}

// ---- Event binding ---------------------------------------------------------
//
// Wpinamy jeden listener w korzeniu mesh-detail; mesh-detail.js wola to po
// kazdym renderDetail, wiec listener musi byc idempotentny (markujemy root).

export function bindNsightActions(root, node) {
  if (!root || !node) return;
  // Idempotentnie po stronie (root, ekran). Cleanup() w mesh-detail wola
  // cleanupNsight ktory zdejmuje listener — tu wystarczy sprawdzic czy juz
  // mamy aktywny handler na tym wlasnie root'cie.
  if (boundActionsRoot === root && boundActionsHandler) return;
  if (boundActionsRoot && boundActionsHandler) {
    boundActionsRoot.removeEventListener('click', boundActionsHandler);
  }
  boundActionsRoot = root;
  boundActionsHandler = async (e) => {
    const btn = e.target.closest('[data-action]');
    if (!btn) return;
    const action = btn.dataset.action;
    if (!action || !action.startsWith('nsight-')) return;
    if (btn.hasAttribute('disabled')) return;

    if (action === 'nsight-profile-node') {
      openStartModal(node, { defaultScope: 'both', defaultGpu: 'all' });
      return;
    }
    if (action === 'nsight-copy-cmd') {
      // Komenda osadzona w data-attribute — bez globalnego stanu, dziala
      // niezaleznie od node id (przyklad ze strony jest zawsze ten sam tekst).
      const cmd = btn.dataset.cmd || '';
      if (!cmd) return;
      try {
        await navigator.clipboard.writeText(cmd);
        const original = btn.textContent;
        btn.textContent = I18n.t('nsight.install.copied');
        setTimeout(() => { btn.textContent = original; }, 1500);
      } catch (_err) {
        toast(I18n.t('nsight.session.error'), 'error');
      }
      return;
    }
    if (action === 'nsight-scroll-install') {
      // Chip "Nsight: niedostepny" w topbarze — przewija do instrukcji.
      const card = root.querySelector('[data-nsight-install-card]');
      if (card) card.scrollIntoView({ behavior: 'smooth', block: 'start' });
      return;
    }
    if (action === 'nsight-profile-card') {
      const idx = parseInt(btn.dataset.gpuIdx, 10);
      const idxStr = Number.isFinite(idx) ? String(idx) : 'all';
      openStartModal(node, { defaultScope: 'gpu', defaultGpu: idxStr });
      return;
    }
    if (action === 'nsight-stop-session') {
      await stopActiveSession();
      return;
    }
    if (action === 'nsight-open-report') {
      const sid = btn.dataset.sessionId;
      if (!sid) return;
      const { Router } = await import('/js/router.js');
      Router.navigate('profile-report', { nodeId: node.node_id, sessionId: sid });
      return;
    }
    if (action === 'nsight-download') {
      const sid = btn.dataset.sessionId;
      if (!sid) return;
      try {
        const resp = await nsightDownload({ nodeId: node.node_id, sessionId: sid });
        const bytes = resp?.bytes;
        const filename = resp?.filename || `${sid}.nsys-rep`;
        if (!bytes || !(bytes.byteLength || bytes.length)) {
          throw new Error('empty payload');
        }
        const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
        const blob = new Blob([u8], { type: 'application/octet-stream' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      } catch (err) {
        toast(`${I18n.t('nsight.session.error')}: ${err.message || err}`, 'error');
      }
      return;
    }
    if (action === 'nsight-delete') {
      const sid = btn.dataset.sessionId;
      if (!sid) return;
      const ok = await TfWindow.confirm({
        title: I18n.t('nsight.action.delete'),
        message: I18n.t('nsight.action.confirm_delete'),
        confirmLabel: I18n.t('nsight.action.delete'),
        cancelLabel: I18n.t('nsight.cancel'),
        danger: true,
      });
      if (!ok) return;
      try {
        await nsightDelete({ nodeId: node.node_id, sessionId: sid });
        await loadSessions(node.node_id);
        notifyChange();
      } catch (err) {
        toast(`${I18n.t('nsight.session.error')}: ${err.message}`, 'error');
      }
      return;
    }
  };
  root.addEventListener('click', boundActionsHandler);
}

// ---- Modal start ----------------------------------------------------------

function openStartModal(node, { defaultScope = 'both', defaultGpu = 'all' } = {}) {
  if (activeSession) {
    toast(I18n.t('nsight.error.busy'), 'warn');
    return;
  }
  const gpus = Array.isArray(node.gpus) ? node.gpus.filter((g) => g && String(g.vendor || '').toLowerCase() === 'nvidia') : [];
  const gpuOptions = ['<option value="all">' + escapeHtml(I18n.t('nsight.gpu.all')) + '</option>']
    .concat(gpus.map((g, idx) => {
      const realIdx = Array.isArray(node.gpus) ? node.gpus.indexOf(g) : idx;
      const label = `GPU ${realIdx}: ${g.name || ''}`.trim();
      return `<option value="${realIdx}">${escapeHtml(label)}</option>`;
    }))
    .join('');

  const bodyHtml = `
    <div class="nsight-form">
      <div class="field">
        <label class="field-label">${escapeHtml(I18n.t('nsight.scope.label'))}</label>
        <tf-select id="nsight-scope" value="${escapeAttr(defaultScope)}">
          <option value="cpu">${escapeHtml(I18n.t('nsight.scope.cpu'))}</option>
          <option value="gpu">${escapeHtml(I18n.t('nsight.scope.gpu'))}</option>
          <option value="both">${escapeHtml(I18n.t('nsight.scope.both'))}</option>
        </tf-select>
      </div>
      <div class="field" id="nsight-gpu-field">
        <label class="field-label">${escapeHtml(I18n.t('nsight.gpu.label'))}</label>
        <tf-select id="nsight-gpu" value="${escapeAttr(defaultGpu)}">${gpuOptions}</tf-select>
      </div>
      <div class="field">
        <label class="field-label">${escapeHtml(I18n.t('nsight.duration.label'))}</label>
        <tf-select id="nsight-duration" value="60">
          <option value="30">${escapeHtml(I18n.t('nsight.duration.30s'))}</option>
          <option value="60">${escapeHtml(I18n.t('nsight.duration.60s'))}</option>
          <option value="120">${escapeHtml(I18n.t('nsight.duration.120s'))}</option>
          <option value="0">${escapeHtml(I18n.t('nsight.duration.manual'))}</option>
        </tf-select>
      </div>
      <tf-input id="nsight-label" label="${escapeAttr(I18n.t('nsight.label.label'))}" placeholder="${escapeAttr(I18n.t('nsight.label.placeholder'))}"></tf-input>
      <div class="form-error" hidden></div>
    </div>
  `;
  const footerHtml = `
    <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('nsight.cancel'))}</tf-button>
    <tf-button variant="primary" icon="play" data-action="start">${escapeHtml(I18n.t('nsight.start'))}</tf-button>
  `;

  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('nsight.modal.title'));
  win.setAttribute('icon', 'record');
  win.setAttribute('buttons', 'close');
  win.setAttribute('width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.setAttribute('draggable', '');
  const bodyWrap = document.createElement('div');
  bodyWrap.slot = 'body';
  bodyWrap.innerHTML = bodyHtml;
  win.appendChild(bodyWrap);
  const footWrap = document.createElement('div');
  footWrap.slot = 'footer';
  footWrap.innerHTML = footerHtml;
  win.appendChild(footWrap);
  const backdrop = document.createElement('div');
  backdrop.className = 'tf-window-backdrop';
  document.body.append(backdrop, win);

  const cleanup = () => {
    if (win.isConnected) win.remove();
    if (backdrop.isConnected) backdrop.remove();
  };

  // Pokazywanie/ukrywanie pola GPU zaleznie od scope (cpu nie potrzebuje wyboru).
  const scopeSel = bodyWrap.querySelector('#nsight-scope');
  const gpuField = bodyWrap.querySelector('#nsight-gpu-field');
  const syncGpuVisibility = () => {
    const scope = scopeSel?.value || 'both';
    if (scope === 'cpu') gpuField.classList.add('hidden');
    else gpuField.classList.remove('hidden');
  };
  scopeSel?.addEventListener('change', syncGpuVisibility);
  // Inicjalizacja po mount (tf-select buduje sie w connectedCallback).
  setTimeout(syncGpuVisibility, 0);

  win.addEventListener('action', async (e) => {
    const a = e.detail?.action;
    if (a === 'cancel') return cleanup();
    if (a !== 'start') return;
    e.preventDefault();
    const errEl = bodyWrap.querySelector('.form-error');
    errEl.hidden = true;

    const scopeVal = scopeSel?.value || 'both';
    const gpuVal = bodyWrap.querySelector('#nsight-gpu')?.value || 'all';
    const durationVal = parseInt(bodyWrap.querySelector('#nsight-duration')?.value || '60', 10);
    const labelInput = bodyWrap.querySelector('#nsight-label');
    const labelVal = (labelInput?.value || '').trim() || `profile-${Date.now()}`;

    const scope = mapScopeToProtocol(scopeVal, gpuVal);
    if (!scope) {
      errEl.hidden = false;
      errEl.textContent = I18n.t('nsight.error.invalid_scope');
      return;
    }

    try {
      const resp = await nsightStart({
        nodeId: node.node_id,
        scope,
        durationSecs: Math.max(0, durationVal | 0),
        label: labelVal,
      });
      activeSession = {
        sessionId: resp.sessionId,
        startedAtMs: resp.startedAtMs || Date.now(),
        scope,
        scopeKey: scopeVal,
        gpuKey: gpuVal,
        label: labelVal,
        durationSecs: Math.max(0, durationVal | 0),
      };
      activeNodeId = node.node_id;
      startCountdown();
      toast(I18n.t('nsight.session.started'), 'success');
      cleanup();
      // Odswiez liste sesji + zmusza rerender mesh-detail.
      await loadSessions(node.node_id);
      notifyChange();
    } catch (err) {
      errEl.hidden = false;
      errEl.textContent = `${I18n.t('nsight.session.error')}: ${err.message || err}`;
    }
  });
}

// ---- Stop / lifecycle -----------------------------------------------------

async function stopActiveSession() {
  if (!activeSession || !activeNodeId) return;
  const sid = activeSession.sessionId;
  const nodeId = activeNodeId;
  try {
    await nsightStop({ nodeId, sessionId: sid });
    // Po Stop status idzie do Stopping -> Done; polling listy posprza activeSession.
    startSessionsPolling(nodeId, sid);
  } catch (err) {
    toast(`${I18n.t('nsight.session.error')}: ${err.message}`, 'error');
  }
}

function startCountdown() {
  if (countdownInterval) clearInterval(countdownInterval);
  countdownInterval = setInterval(() => {
    if (!activeSession) {
      clearInterval(countdownInterval);
      countdownInterval = null;
      return;
    }
    const elapsed = Math.floor((Date.now() - activeSession.startedAtMs) / 1000);
    // Auto-stop osiagniety — przejdz na polling i czekaj az backend zamknie sesje.
    if (activeSession.durationSecs > 0 && elapsed >= activeSession.durationSecs) {
      clearInterval(countdownInterval);
      countdownInterval = null;
      startSessionsPolling(activeNodeId, activeSession.sessionId);
      return;
    }
    notifyChange();
  }, 1000);
}

function startSessionsPolling(nodeId, sessionId) {
  if (pollSessionsInterval) clearInterval(pollSessionsInterval);
  pollSessionsInterval = setInterval(async () => {
    if (!activeSession || activeSession.sessionId !== sessionId) {
      clearInterval(pollSessionsInterval);
      pollSessionsInterval = null;
      return;
    }
    try {
      await loadSessions(nodeId);
    } catch (_e) { /* przeczekaj */ }
    const entry = cachedSessions.find((s) => s.sessionId === sessionId);
    if (entry && (entry.status === 'Done' || entry.status === 'Failed')) {
      clearInterval(pollSessionsInterval);
      pollSessionsInterval = null;
      const wasFail = entry.status === 'Failed';
      activeSession = null;
      activeNodeId = null;
      if (countdownInterval) { clearInterval(countdownInterval); countdownInterval = null; }
      toast(
        wasFail ? `${I18n.t('nsight.session.error')}: ${entry.error || ''}` : I18n.t('nsight.session.finished'),
        wasFail ? 'error' : 'success',
      );
      notifyChange();
    }
  }, 2000);
}

function notifyChange() {
  if (onChangeCallback) {
    try { onChangeCallback(); } catch (_e) { /* nie blokuj timera */ }
  }
}

// ---- Helpers --------------------------------------------------------------

// Mapuje wybor z UI na enum protokolu (kompatybilny z codec.js — lowercase strings).
function mapScopeToProtocol(scopeKey, gpuKey) {
  if (scopeKey === 'cpu') return 'cpu';
  if (scopeKey === 'gpu') {
    if (gpuKey === 'all') return 'gpu_all';
    const idx = parseInt(gpuKey, 10);
    if (!Number.isFinite(idx) || idx < 0 || idx > 255) return null;
    return { kind: 'gpu_index', idx };
  }
  if (scopeKey === 'both') {
    if (gpuKey === 'all') return 'both_all';
    const idx = parseInt(gpuKey, 10);
    if (!Number.isFinite(idx) || idx < 0 || idx > 255) return null;
    return { kind: 'both_index', idx };
  }
  return null;
}

// scope w odpowiedzi przychodzi w formie CamelCase tagged enum (patrz wasm glue).
function formatScopeForDisplay(scope) {
  if (typeof scope === 'string') {
    if (scope === 'Cpu') return 'CPU';
    if (scope === 'GpuAll') return 'GPU all';
    if (scope === 'BothAll') return 'CPU + GPU all';
    return scope;
  }
  if (scope && typeof scope === 'object') {
    if (scope.kind === 'GpuIndex') return `GPU ${scope.idx}`;
    if (scope.kind === 'BothIndex') return `CPU + GPU ${scope.idx}`;
  }
  return '—';
}

function statusChipHtml(status) {
  const map = {
    Running: { cls: 'recording', dot: true, label: 'Running' },
    Stopping: { cls: 'pending', dot: true, label: 'Stopping' },
    Done: { cls: 'online', dot: false, label: 'Done' },
    Failed: { cls: 'err', dot: true, label: 'Failed' },
  };
  const e = map[status] || { cls: 'info', dot: false, label: status || '—' };
  const dotAttr = e.dot ? ' dot' : '';
  return `<tf-chip status="${e.cls}"${dotAttr}>${escapeHtml(e.label)}</tf-chip>`;
}

function formatDurationForRow(s) {
  if (s.status === 'Running' || s.status === 'Stopping') {
    return I18n.t('nsight.session.in_progress');
  }
  if (typeof s.durationMs === 'number' && s.durationMs > 0) {
    return formatMillis(s.durationMs);
  }
  return '—';
}

function formatMillis(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(1)} s`;
  const m = Math.floor(sec / 60);
  const s = Math.round(sec % 60);
  return `${m}m ${s}s`;
}

function formatCountdown(seconds) {
  const s = Math.max(0, Math.floor(seconds));
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${String(m).padStart(2, '0')}:${String(r).padStart(2, '0')}`;
}

function formatElapsed(seconds) {
  return formatCountdown(seconds);
}

function formatDateTime(epochMs) {
  if (!epochMs) return '—';
  const d = new Date(Number(epochMs));
  if (Number.isNaN(d.getTime())) return '—';
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function truncate(s, max) {
  if (!s) return '';
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

// Zachowane na potrzeby ewentualnego rozmiaru raportu w UI (PR6).
export const _internal = { formatBytes, formatMillis };
