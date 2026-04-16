// =============================================================================
// Plik: modules/catalog/DeployUtils.js
// Opis: Wspolne funkcje deploymentu (uzywane przez ServiceCatalog/NIM) -
//       wyszukiwanie wolnego portu, pasek postepu deploy.
// =============================================================================

const DeployUtils = (() => {
  'use strict';

  // Wyszukanie wolnego portu na hoscie
  async function findFreePort(targetNodeId, desiredPort) {
    try {
      const deployments = await ApiClient.get(`/api/mesh/nodes/${encodeURIComponent(targetNodeId)}/deployments`);
      const usedPorts = new Set();
      for (const d of (deployments || [])) {
        try {
          const config = JSON.parse(d.config_json || '{}');
          if (config.port) usedPorts.add(parseInt(config.port, 10));
        } catch {}
      }

      let port = desiredPort;
      while (usedPorts.has(port)) {
        port++;
      }
      return port;
    } catch {
      return desiredPort;
    }
  }

  // Renderuje pasek postepu deploy z logami
  function renderDeployProgress(phase, message, data, logs, startTime) {
    const phases = ['certs', 'deploying', 'deployed', 'health_check_waiting', 'health_check_ready', 'discovering_models', 'registering_service', 'progress', 'registering', 'done'];
    const phaseLabels = {
      connecting: I18n.t('topbar.connected').replace('Connected', 'Connecting...'),
      certs: I18n.t('settings.tls.title'),
      deploying: I18n.t('deploy.progress.deploying'),
      deployed: 'Container deployed',
      health_check_waiting: 'Waiting for container health...',
      health_check_ready: 'Container ready',
      discovering_models: 'Discovering models...',
      registering_service: 'Registering service...',
      service_registered: 'Service registered',
      progress: 'Docker Compose',
      registering: I18n.t('models.registry'),
      done: I18n.t('common.success'),
    };

    const phaseIdx = phases.indexOf(phase);
    const totalPhases = phases.length - 1;
    let percent = 0;
    if (phase === 'connecting') {
      percent = 2;
    } else if (phase === 'done') {
      percent = 100;
    } else if (phaseIdx >= 0) {
      percent = Math.round(((phaseIdx + 0.5) / totalPhases) * 100);
    }

    const elapsed = startTime ? ((Date.now() - startTime) / 1000).toFixed(0) : '0';
    const label = phaseLabels[phase] || phase;
    const isDone = phase === 'done';
    const isError = isDone && data && !data.success;
    const isOk = isDone && data && data.success;

    let barClass = 'deploy-bar-fill';
    if (isOk) barClass += ' deploy-bar--done';
    else if (isError) barClass += ' deploy-bar--error';
    else barClass += ' deploy-bar--active';

    let html = '<div class="deploy-progress-wrapper">';
    html += `<div class="deploy-phase-label"><span>${Utils.escapeHtml(label)}</span><span class="deploy-timer">${elapsed}s</span></div>`;
    html += `<div class="deploy-bar-track"><div class="${barClass}" style="width:${percent}%"></div></div>`;

    if (logs && logs.length > 0) {
      html += '<div class="deploy-log-box">';
      const tail = logs.slice(-30);
      for (const line of tail) {
        html += `<div class="log-line">${Utils.escapeHtml(line)}</div>`;
      }
      html += '</div>';
    }

    if (isOk) {
      html += `<div class="deploy-done-msg deploy-done--ok">${I18n.t('catalog.deploy_modal.done')}</div>`;
    } else if (isError) {
      html += `<div class="deploy-done-msg deploy-done--fail">${I18n.t('common.error')}: ${Utils.escapeHtml(data.error || I18n.t('common.unknown'))}</div>`;
    }

    html += '</div>';
    return html;
  }

  return { findFreePort, renderDeployProgress };
})();
