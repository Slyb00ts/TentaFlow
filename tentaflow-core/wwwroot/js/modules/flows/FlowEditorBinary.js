// =============================================================================
// Plik: modules/flows/FlowEditorBinary.js
// Opis: Flow editor (R-ONE detail + W-CREATE/UPDATE) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const FlowEditorBinary = (() => {
  'use strict';
  let flow = null;

  async function loadFlow(flowId) {
    try {
      const body = await ApiBinary.one('flowDetailRequest', { flowId });
      flow = body;
      renderEditor();
    } catch (err) {
      console.error('[flow-editor] load failed:', err);
    }
  }

  async function createFlow(name, description, graphJson) {
    try {
      const result = await ApiBinary.action('flowCreateRequest', {
        name,
        description,
        graphJson,
      });
      App.showToast(`Created: ${result.flowId}`, 'success');
      return result.flowId;
    } catch (err) {
      App.showToast(err.message, 'error');
      throw err;
    }
  }

  function renderEditor() {
    const el = document.getElementById('flow-editor-content');
    if (!el || !flow) return;
    el.innerHTML = `
      <input id="flow-name" value="${Utils.escapeHtml(flow.name)}" />
      <textarea id="flow-graph">${Utils.escapeHtml(flow.graphJson)}</textarea>
    `;
  }

  return {
    mount: () => {
      document.getElementById('btn-create-flow')?.addEventListener('click', () => {
        const name = document.getElementById('flow-name')?.value;
        const desc = document.getElementById('flow-description')?.value;
        const graph = document.getElementById('flow-graph')?.value;
        if (name && graph) createFlow(name, desc || null, graph);
      });
    },
    unmount: () => { flow = null; },
    loadFlow,
    createFlow,
  };
})();

export default FlowEditorBinary;
