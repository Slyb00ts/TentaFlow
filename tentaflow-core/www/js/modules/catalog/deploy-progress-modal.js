// =============================================================================
// File: modules/catalog/deploy-progress-modal.js
// Opis: Live progress modal dla deploymentu silnika. Subscribes via binary
//       protocol (deploymentLogStreamRequest) — replay log_tail z DB, potem
//       live chunki z runnera. Pokazuje pasek postępu, fazę, ostatnie ~50 linii
//       build output. Po StreamEnd emituje toast + zamyka modal (success) lub
//       trzyma otwarty z error message (failure).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';

const LOG_BUFFER_LIMIT = 200;

export function openDeployProgressModal({ deployId, engineId, deployMethod }) {
  const win = document.createElement('tf-window');
  win.setAttribute('title', `${I18n.t('deploy.progress_title')}: ${engineId}`);
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '560');
  win.setAttribute('width', '640');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  body.innerHTML = renderBodyInitial({ deployId, engineId, deployMethod });
  win.appendChild(body);

  const footer = document.createElement('div');
  footer.slot = 'footer';
  footer.style.cssText = 'display:flex;gap:8px;justify-content:flex-end;padding:8px;';
  footer.innerHTML = `
    <tf-button variant="ghost" data-close-btn>${escapeHtml(I18n.t('common.close'))}</tf-button>
  `;
  win.appendChild(footer);

  document.body.appendChild(win);

  let unsubscribe = null;
  let logLines = [];
  const logBox = () => body.querySelector('[data-log-box]');
  const progressBar = () => body.querySelector('[data-progress-bar]');
  const progressLabel = () => body.querySelector('[data-progress-label]');
  const phaseLabel = () => body.querySelector('[data-phase-label]');
  const statusChip = () => body.querySelector('[data-status-chip]');
  const errorBox = () => body.querySelector('[data-error-box]');

  function appendLine(line) {
    if (!line) return;
    logLines.push(line);
    if (logLines.length > LOG_BUFFER_LIMIT) {
      logLines = logLines.slice(-LOG_BUFFER_LIMIT);
    }
    const box = logBox();
    if (box) {
      box.textContent = logLines.join('\n');
      box.scrollTop = box.scrollHeight;
    }
  }

  function applyProgress(pct) {
    const bar = progressBar();
    const label = progressLabel();
    if (bar) bar.style.width = `${Math.max(0, Math.min(100, pct))}%`;
    if (label) label.textContent = `${Math.max(0, Math.min(100, pct))}%`;
  }

  function applyPhase(phase) {
    const el = phaseLabel();
    if (el) el.textContent = phase || '';
  }

  function applyStatus(status) {
    const chip = statusChip();
    if (!chip) return;
    chip.setAttribute('status', statusToChipVariant(status));
    chip.textContent = I18n.t(`deploy.status_${status}`) || status;
  }

  function onChunk(body) {
    if (!body || body.variant !== 'DeploymentStreamChunk') return;
    if (body.deployId && body.deployId !== deployId) return;
    if (body.kind === 'log') {
      appendLine(body.line);
    } else if (body.kind === 'progress') {
      applyProgress(body.progressPct);
      if (body.phase) applyPhase(body.phase);
    } else if (body.kind === 'phase') {
      applyPhase(body.phase || body.line);
      applyProgress(body.progressPct);
      appendLine(`— ${body.phase || body.line}`);
    }
  }

  function onEnd(body) {
    if (!body) {
      applyStatus('ended');
      return;
    }
    if (body.variant !== 'DeploymentStreamEnd') return;
    applyProgress(100);
    applyStatus(body.finalStatus || 'ended');
    if (body.finalStatus === 'failure') {
      const box = errorBox();
      if (box) {
        box.textContent = body.errorMessage || I18n.t('deploy.err_generic');
        box.hidden = false;
      }
      toast(`${I18n.t('deploy.failed')}: ${body.errorMessage || ''}`, 'error');
    } else if (body.finalStatus === 'success') {
      appendLine(`— ${I18n.t('deploy.success')} (${body.durationMs || 0} ms)`);
      toast(I18n.t('deploy.success'), 'success');
    }
  }

  (async () => {
    try {
      unsubscribe = await ApiBinary.subscribe(
        'deploymentLogStreamRequest',
        { deployId, replayTail: true },
        {
          onChunk,
          onEnd,
          onError: (err) => {
            applyStatus('failure');
            const box = errorBox();
            if (box) {
              box.textContent = err?.message || I18n.t('deploy.err_generic');
              box.hidden = false;
            }
          },
        }
      );
    } catch (err) {
      applyStatus('failure');
      appendLine(`[stream error] ${err?.message || ''}`);
    }
  })();

  const closeModal = () => {
    if (unsubscribe) {
      try {
        unsubscribe();
      } catch (_) {}
      unsubscribe = null;
    }
    if (win.parentNode) win.parentNode.removeChild(win);
  };
  footer.querySelector('[data-close-btn]')?.addEventListener('click', closeModal);
  win.addEventListener('close', closeModal);
}

function renderBodyInitial({ deployId, engineId, deployMethod }) {
  return `
    <div class="deploy-progress">
      <div class="deploy-progress-head">
        <div>
          <div class="deploy-progress-engine">${escapeHtml(engineId)}</div>
          <div class="deploy-progress-meta">
            <span>${escapeHtml(deployMethod)}</span>
            <span>·</span>
            <code>${escapeHtml(deployId)}</code>
          </div>
        </div>
        <tf-chip data-status-chip status="warn" dot>${escapeHtml(I18n.t('deploy.status_building'))}</tf-chip>
      </div>
      <div class="deploy-progress-phase">
        <span data-phase-label>—</span>
        <span data-progress-label>0%</span>
      </div>
      <div class="deploy-progress-track">
        <div class="deploy-progress-bar" data-progress-bar style="width:0%"></div>
      </div>
      <pre class="deploy-log-box" data-log-box></pre>
      <div class="deploy-progress-error" data-error-box hidden></div>
    </div>
  `;
}

function statusToChipVariant(status) {
  switch (status) {
    case 'success':
      return 'success';
    case 'failure':
    case 'cancelled':
      return 'danger';
    default:
      return 'warn';
  }
}
