// =============================================================================
// Plik: modules/services/DeployProgressBinary.js
// Opis: Service deploy progress widget (R-STREAM) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const DeployProgressBinary = (() => {
  'use strict';
  let unsubscribe = null;
  let currentDeployId = null;

  async function deploy(engineId, modelId, deployMethod, nodeId) {
    try {
      const accept = await ApiBinary.action('serviceDeployRequest', {
        engineId,
        modelId,
        deployMethod,
        nodeId,
      });
      currentDeployId = accept.deployId;
      App.showToast(`Deploy started: ${currentDeployId}`, 'info');

      // Subskrypcja na progress events przyjdzie w phase 2 gdy
      // ServiceDeployProgress bedzie streaming variant na serwerze.
      // Bootstrap: tylko ack.
    } catch (err) {
      App.showToast(`deploy failed: ${err.message}`, 'error');
    }
  }

  function renderProgress(progress) {
    const bar = document.getElementById('deploy-progress-bar');
    const stage = document.getElementById('deploy-progress-stage');
    const msg = document.getElementById('deploy-progress-msg');
    if (bar) bar.style.width = `${progress.progressPercent}%`;
    if (stage) stage.textContent = progress.stage;
    if (msg) msg.textContent = progress.message;
  }

  return {
    mount: () => {},
    unmount: () => {
      if (unsubscribe) unsubscribe();
      unsubscribe = null;
      currentDeployId = null;
    },
    deploy,
    renderProgress,
  };
})();

export default DeployProgressBinary;
