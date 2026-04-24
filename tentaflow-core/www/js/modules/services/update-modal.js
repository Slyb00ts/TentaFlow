// =============================================================================
// File: modules/services/update-modal.js
// Opis: Modal "Aktualizacja serwisu". Wysyla ServiceRedeployRequest i
//       subskrybuje DeploymentLogStreamRequest na deployId zwroconym w
//       odpowiedzi. Obsluguje teams-bot active_sessions handshake (force=false
//       -> warning -> force=true), stany success/failed/unsupported/no_source
//       oraz timeout 5 min.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeHtml, toast } from '/js/utils.js';

const LOG_BUFFER_LIMIT = 200;
const TIMEOUT_MS = 5 * 60 * 1000;

/// Otwiera modal aktualizacji dla podanego serwisu. onUpdated() jest wolane po
/// sukcesie zeby strona mogla odswiezyc liste i schowac badge.
export function openUpdateModal({ service, onUpdated } = {}) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', I18n.t('services.update_title', { name: service.name }));
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '560');
  win.setAttribute('width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = renderBodyInitial(service);
  win.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;justify-content:flex-end;padding:8px;';
  footer.innerHTML = renderFooterIdle();
  win.appendChild(footer);

  document.body.appendChild(win);

  let unsubscribe = null;
  let timeoutTimer = null;
  let logLines = [];
  let currentDeployId = null;

  const logBox = () => body.querySelector('[data-log-box]');
  const statusChip = () => body.querySelector('[data-status-chip]');
  const messageBox = () => body.querySelector('[data-message-box]');
  const warningBox = () => body.querySelector('[data-warning-box]');

  function setStatus(variant, labelKey) {
    const chip = statusChip();
    if (!chip) return;
    chip.setAttribute('status', variant);
    chip.textContent = I18n.t(labelKey);
  }

  function appendLine(line, level = 'info') {
    if (!line) return;
    logLines.push({ line, level });
    if (logLines.length > LOG_BUFFER_LIMIT) logLines = logLines.slice(-LOG_BUFFER_LIMIT);
    const box = logBox();
    if (!box) return;
    box.innerHTML = logLines
      .map((l) => `<div class="svc-update-log-line svc-update-log-${l.level}">${escapeHtml(l.line)}</div>`)
      .join('');
    box.scrollTop = box.scrollHeight;
  }

  function stopStream() {
    if (unsubscribe) {
      try { unsubscribe(); } catch (_) { /* already closed */ }
      unsubscribe = null;
    }
    if (timeoutTimer) {
      clearTimeout(timeoutTimer);
      timeoutTimer = null;
    }
  }

  function closeModal() {
    stopStream();
    if (win.parentNode) win.parentNode.removeChild(win);
  }
  win.addEventListener('close', closeModal);

  function showFooter(html) {
    footer.innerHTML = html;
    footer.querySelector('[data-close-btn]')?.addEventListener('click', closeModal);
    footer.querySelector('[data-retry-btn]')?.addEventListener('click', () => {
      logLines = [];
      appendLine('');
      const box = logBox();
      if (box) box.innerHTML = '';
      const mb = messageBox();
      if (mb) { mb.hidden = true; mb.textContent = ''; }
      footer.innerHTML = renderFooterIdle();
      wireIdleFooter();
    });
    footer.querySelector('[data-cancel-btn]')?.addEventListener('click', closeModal);
    footer.querySelector('[data-confirm-btn]')?.addEventListener('click', () => startRedeploy(false));
    footer.querySelector('[data-force-btn]')?.addEventListener('click', () => startRedeploy(true));
  }

  function wireIdleFooter() {
    showFooter(renderFooterIdle());
  }

  function onResponse(resp) {
    if (!resp) return;
    const status = resp.status || '';
    if (status === 'started') {
      currentDeployId = resp.deployId || '';
      setStatus('warn', 'services.update_progress');
      showFooter(`
        <tf-button variant="ghost" data-close-btn>${escapeHtml(I18n.t('services.update_cancel'))}</tf-button>
      `);
      subscribeToStream(currentDeployId);
    } else if (status === 'active_sessions') {
      const wb = warningBox();
      if (wb) {
        wb.textContent = I18n.t('services.update_active_sessions_warning', { count: resp.activeSessionCount ?? 0 });
        wb.hidden = false;
      }
      showFooter(`
        <tf-button variant="ghost" data-cancel-btn>${escapeHtml(I18n.t('services.update_cancel'))}</tf-button>
        <tf-button variant="danger" data-force-btn>${escapeHtml(I18n.t('services.update_force'))}</tf-button>
      `);
      setStatus('warn', 'services.update_button');
    } else if (status === 'unsupported') {
      showTerminalInfo('services.update_unsupported', resp.error);
    } else if (status === 'no_source') {
      showTerminalInfo('services.update_no_source', resp.error);
    } else if (status === 'not_found') {
      showTerminalInfo('services.update_failed', resp.error);
    } else if (status === 'failed') {
      showTerminalFailure(resp.error);
    }
  }

  function showTerminalInfo(labelKey, detail) {
    setStatus('info', labelKey);
    const mb = messageBox();
    if (mb) {
      mb.textContent = detail ? `${I18n.t(labelKey)} — ${detail}` : I18n.t(labelKey);
      mb.hidden = false;
    }
    showFooter(`
      <tf-button variant="primary" data-close-btn>${escapeHtml(I18n.t('services.update_close'))}</tf-button>
    `);
  }

  function showTerminalFailure(errorMsg) {
    stopStream();
    setStatus('danger', 'services.update_failed');
    const mb = messageBox();
    if (mb) {
      mb.textContent = errorMsg || I18n.t('services.update_failed');
      mb.hidden = false;
      mb.classList.add('svc-update-message-error');
    }
    showFooter(`
      <tf-button variant="ghost" data-close-btn>${escapeHtml(I18n.t('services.update_close'))}</tf-button>
      <tf-button variant="primary" data-retry-btn>${escapeHtml(I18n.t('services.update_retry'))}</tf-button>
    `);
  }

  function showTerminalSuccess(durationMs) {
    stopStream();
    setStatus('success', 'services.update_success');
    appendLine(`— ${I18n.t('services.update_success')}${durationMs ? ` (${durationMs} ms)` : ''}`, 'success');
    toast(I18n.t('services.update_success'), 'success');
    showFooter(`
      <tf-button variant="primary" data-close-btn>${escapeHtml(I18n.t('services.update_close'))}</tf-button>
    `);
    try { onUpdated?.(); } catch (_) { /* caller refresh best-effort */ }
  }

  function subscribeToStream(deployId) {
    if (!deployId) {
      showTerminalFailure(I18n.t('services.update_failed'));
      return;
    }
    timeoutTimer = setTimeout(() => {
      showTerminalFailure(I18n.t('services.update_timeout'));
    }, TIMEOUT_MS);

    ApiBinary.subscribe(
      'deploymentLogStreamRequest',
      { deployId, replayTail: true },
      {
        onChunk: (chunk) => {
          if (!chunk || chunk.variant !== 'DeploymentStreamChunk') return;
          if (chunk.deployId && chunk.deployId !== deployId) return;
          if (chunk.kind === 'log') {
            appendLine(chunk.line, classifyLevel(chunk.line));
          } else if (chunk.kind === 'phase') {
            appendLine(`— ${chunk.phase || chunk.line}`, 'info');
          }
        },
        onEnd: (end) => {
          if (!end || end.variant !== 'DeploymentStreamEnd') return;
          if (end.finalStatus === 'success') {
            showTerminalSuccess(end.durationMs || 0);
          } else {
            showTerminalFailure(end.errorMessage || I18n.t('services.update_failed'));
          }
        },
        onError: (err) => {
          showTerminalFailure(err?.message || I18n.t('services.update_failed'));
        },
      }
    ).then((fn) => { unsubscribe = fn; })
     .catch((err) => showTerminalFailure(err?.message || I18n.t('services.update_failed')));
  }

  async function startRedeploy(force) {
    setStatus('warn', 'services.update_progress');
    const wb = warningBox();
    if (wb) { wb.hidden = true; wb.textContent = ''; }
    const mb = messageBox();
    if (mb) { mb.hidden = true; mb.textContent = ''; mb.classList.remove('svc-update-message-error'); }
    showFooter(`
      <tf-button variant="ghost" data-close-btn>${escapeHtml(I18n.t('services.update_cancel'))}</tf-button>
    `);
    try {
      const resp = await ApiBinary.one('serviceRedeployRequest', {
        serviceId: Number(service.id),
        forceIfActiveSessions: !!force,
      });
      onResponse(resp);
    } catch (err) {
      showTerminalFailure(err?.message || I18n.t('services.update_failed'));
    }
  }

  wireIdleFooter();
  // Initial dispatch: first try without force; backend decides if active_sessions warning is needed.
  startRedeploy(false);
}

function renderBodyInitial(service) {
  return `
    <div class="deploy-progress svc-update-body">
      <div class="deploy-progress-head">
        <div>
          <div class="deploy-progress-engine">${escapeHtml(service.name)}</div>
          <div class="deploy-progress-meta">
            <span>${escapeHtml(service.engineId || '')}</span>
            ${service.deployMethod ? `<span>·</span><span>${escapeHtml(service.deployMethod)}</span>` : ''}
          </div>
        </div>
        <tf-chip data-status-chip status="warn" dot>${escapeHtml(I18n.t('services.update_progress'))}</tf-chip>
      </div>
      <div class="svc-update-warning" data-warning-box hidden></div>
      <div class="svc-update-message" data-message-box hidden></div>
      <div class="deploy-log-box svc-update-log" data-log-box></div>
    </div>
  `;
}

function renderFooterIdle() {
  // While the initial request is in flight we hide actions — once the response
  // arrives the real footer takes over (active_sessions warning, progress, etc.)
  return `
    <tf-button variant="ghost" data-close-btn>${escapeHtml(I18n.t('services.update_cancel'))}</tf-button>
  `;
}

function classifyLevel(line) {
  const lower = String(line || '').toLowerCase();
  if (lower.includes('error') || lower.includes('failed') || lower.includes('fatal')) return 'error';
  if (lower.includes('warn')) return 'warn';
  return 'info';
}
