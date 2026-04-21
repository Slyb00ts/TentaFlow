// =============================================================================
// Plik: modules/mesh/NodeDetailBinary.js
// Opis: Node detail ekran (R-ONE archetyp) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const NodeDetailBinary = (() => {
  'use strict';
  let nodeData = null;

  async function loadNode(nodeIdHex) {
    const nodeId = hexToBytes(nodeIdHex);
    if (nodeId.length !== 32) {
      console.error('[node-detail] node_id must be 32 bytes');
      return;
    }
    try {
      const body = await ApiBinary.one('nodeInfoRequest', nodeId);
      nodeData = body;
      renderDetail();
    } catch (err) {
      console.error('[node-detail] failed:', err);
      // Bootstrap returns NotFound stub — to oczekiwane.
      renderNotFound(err.message);
    }
  }

  function renderDetail() {
    const el = document.getElementById('node-detail');
    if (!el) return;
    el.innerHTML = `<pre>${Utils.escapeHtml(JSON.stringify(nodeData, null, 2))}</pre>`;
  }

  function renderNotFound(msg) {
    const el = document.getElementById('node-detail');
    if (!el) return;
    el.innerHTML = `<div class="empty-state"><div class="empty-state-text">${Utils.escapeHtml(msg)}</div></div>`;
  }

  function hexToBytes(hex) {
    const clean = hex.replace(/[^0-9a-f]/gi, '');
    const out = new Uint8Array(clean.length / 2);
    for (let i = 0; i < out.length; i++) {
      out[i] = parseInt(clean.substr(i * 2, 2), 16);
    }
    return out;
  }

  return {
    mount: () => {},
    unmount: () => { nodeData = null; },
    loadNode,
  };
})();

export default NodeDetailBinary;
