// =============================================================================
// Plik: modules/services/ContainerLogsBinary.js
// Opis: Container logs ekran z subskrypcja na ContainerLogChunk stream.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const ContainerLogsBinary = (() => {
  'use strict';
  let unsubscribe = null;
  const MAX_LINES = 1000;
  let buffer = [];

  async function tail(containerId, follow = true) {
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
    buffer = [];
    renderLogs();

    unsubscribe = await ApiBinary.subscribe(
      'containerLogStreamRequest',
      { containerId, follow },
      {
        onChunk: (body) => {
          if (body.variant === 'ContainerLogChunk') {
            buffer.push(body);
            if (buffer.length > MAX_LINES) buffer.shift();
            renderLogs();
          }
        },
        onEnd: () => {
          unsubscribe = null;
        },
        onError: (err) => {
          App.showToast(`logs error: ${err.message}`, 'error');
        },
      }
    );
  }

  function renderLogs() {
    const pre = document.getElementById('container-logs');
    if (!pre) return;
    pre.textContent = buffer
      .map(l => `[${new Date(l.tsEpoch * 1000).toISOString()}] ${l.stream}: ${l.line}`)
      .join('\n');
    pre.scrollTop = pre.scrollHeight;
  }

  function stop() {
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  }

  return {
    mount: () => {
      document.getElementById('logs-tail-btn')?.addEventListener('click', () => {
        const id = document.getElementById('logs-container-id')?.value;
        if (id) tail(id, true);
      });
    },
    unmount: stop,
    tail,
    stop,
  };
})();

export default ContainerLogsBinary;
